//! Garbage collector (SPECS §7): conservative mark-sweep, generational.
//!
//! Design (avmplus ships MMgc, a conservative mark-sweep collector with
//! stack scanning — we follow the same shape, sized for v1):
//!
//! - Every runtime allocation (objects, strings, arrays, vectors, closures,
//!   cells, expando maps) goes through [`alloc`]. Small blocks are carved
//!   from per-size-class **arenas** by bump allocation; each block carries a
//!   16-byte inline header (kind/size/mark/live/gen/remembered) so metadata
//!   lookup is pointer arithmetic, not a map probe. Large blocks go straight
//!   to the system allocator. Only arenas and large blocks are registered in
//!   the `regions` map (tens of entries, not one per object), which is what
//!   the conservative scan does a predecessor query against.
//! - **Collection only happens at safepoints** ([`safepoint`]), which the
//!   backend emits at compiled-function entries and loop headers. At a
//!   safepoint no runtime (Rust) frames are active, so the only places a
//!   live reference can be are the machine stack, callee-saved registers,
//!   and registered globals — all of which we scan. This is what makes it
//!   safe for runtime internals to hold GC pointers in Rust `Vec`s between
//!   allocations: those sequences never cross a safepoint.
//! - The one way runtime frames *can* be live at a safepoint is callback
//!   reentry (e.g. `Array.map` invoking a compiled closure). Those paths
//!   hold a [`DeferGuard`], which turns safepoints into no-ops for their
//!   duration (the natives that stage elements in Rust `Vec`s across a
//!   callback — apply/sort/iterate — take one).
//! - Marking is conservative with interior-pointer support: any word on
//!   the stack (or in a Raw block) that points anywhere *into* a live
//!   block keeps that block alive. Blocks with out-of-heap side storage
//!   (string data, array element buffers, expando maps) are traced
//!   precisely via their kind and dropped properly on sweep.
//!
//! **Generational (P27 Part B), non-moving.** A conservatively-scanned word
//! is only *maybe* a pointer, so a block can never be relocated — instead of
//! copying survivors to an old space, we flip a `gen` bit in place. A block
//! is born *young*; surviving a collection promotes it to *old*. A **minor**
//! collection traces and sweeps only young blocks (the nursery), treating
//! old blocks as live roots; a **major** collection is the full mark-sweep.
//! Old→young pointers created by the mutator between collections would be
//! invisible to a minor collection, so a **write barrier** records the old
//! container into a remembered set whenever it gains a reference:
//! [`vs_gc_remember`], called inline from compiled field/cell stores (guarded
//! by an inline `gen == old` check) and from the runtime container setters.
//! Every collection promotes all survivors, so afterwards no young block
//! exists and the remembered set is cleared — the barrier rebuilds it.
//!
//! Single-threaded (matches the v1 runtime); all state is thread-local.

use std::alloc::Layout;
use std::cell::RefCell;
use std::collections::BTreeMap;

use crate::any::{Tag, VsAny};

/// What a block holds — drives tracing and sweeping.
#[derive(Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum Kind {
    /// Plain words (object instances, closures, cells, closure envs):
    /// scanned conservatively, no destructor.
    Raw = 0,
    /// A `VsString` header; its UTF-16 buffer is freed on sweep.
    String = 1,
    /// A `VsArray`; elements live in a Rust `Vec` traced precisely.
    Array = 2,
    /// A `VsVector`; same shape as Array.
    Vector = 3,
    /// An expando `PropMap`; values traced precisely, map dropped.
    PropMap = 4,
    /// A `VsRegExp`; its source string is traced, the compiled program
    /// dropped on sweep.
    RegExp = 5,
    /// A `VsSocket`; dropping closes the descriptor.
    Socket = 6,
}

impl Kind {
    fn from_u8(v: u8) -> Kind {
        match v {
            0 => Kind::Raw,
            1 => Kind::String,
            2 => Kind::Array,
            3 => Kind::Vector,
            4 => Kind::PropMap,
            5 => Kind::RegExp,
            _ => Kind::Socket,
        }
    }
}

/// Generation tags stored in [`Hdr::gen`]. `OLD` must match the constant the
/// codegen write barrier compares against (see `llvm.rs` `write_barrier`).
const GEN_YOUNG: u8 = 0;
const GEN_OLD: u8 = 1;

/// Per-block metadata, stored inline at the block start (the payload the
/// rest of the runtime sees begins [`HEADER`] bytes later). `gen` sits at
/// offset 7 — the codegen barrier reads it at `payload - 9`.
#[repr(C)]
struct Hdr {
    size: u32,
    kind: u8,
    marked: u8,
    live: u8,
    age: u8,
    remembered: u8,
    _pad: [u8; 7],
}

/// Inline metadata prefix on every block. 16 bytes keeps the payload
/// 16-aligned (the strongest alignment any runtime value needs).
const HEADER: usize = 16;
/// Alignment for arena regions and large blocks.
const BLOCK_ALIGN: usize = 16;
/// Target arena size; each per-class arena is the largest whole number of
/// blocks that fits (min one block).
const ARENA_BYTES: usize = 256 * 1024;

/// Small blocks (payload) up to this are pooled in 16-byte size classes and
/// carved from arenas; larger payloads go straight to the system allocator.
/// Pooling matters: batch free-then-realloc churn fragments macOS malloc
/// (measured: RSS grows linearly with total churn), while recycling swept
/// blocks in-collector keeps the footprint at the high-water mark.
const MAX_SMALL: usize = 1024;

/// A per-size-class bump region. Never freed back to the OS in v1: the
/// arenas *are* the pool, so the footprint plateaus at the high-water mark.
struct Arena {
    base: usize,
    cap: usize,
    used: usize,
    stride: usize,
    /// Young blocks currently in this arena. A minor collection skips
    /// arenas with no young blocks (they hold only promoted-old survivors).
    young: usize,
}

/// What a `regions` entry covers (for the conservative interior-pointer
/// predecessor query).
enum Region {
    /// An arena; the index is into [`Heap::arenas`].
    Arena(usize),
    /// A single large block; its size lives in the inline header.
    Large,
}

/// Reads the inline header of a block given its payload address.
///
/// SAFETY: `payload` must be the payload of a live block allocated here, so
/// `payload - HEADER` points at its `Hdr`.
unsafe fn hdr<'a>(payload: usize) -> &'a mut Hdr {
    unsafe { &mut *((payload - HEADER) as *mut Hdr) }
}

/// Whether the block at `payload` is still in the nursery.
///
/// SAFETY: `payload` must be a live block payload.
unsafe fn is_young(payload: usize) -> bool {
    unsafe { hdr(payload).age == GEN_YOUNG }
}

struct Heap {
    /// Arenas indexed by creation order (`regions` maps a base to an index).
    arenas: Vec<Arena>,
    /// Current bump arena per size class (index = payload_size / 16).
    cur: Vec<Option<usize>>,
    /// Recycled small blocks by size class: each entry is a block *start*
    /// (header address), ready to be re-headered and handed out.
    free_lists: Vec<Vec<usize>>,
    /// Arena bases and large-block starts, for interior-pointer lookup
    /// (a predecessor query). Tens of entries, not one per object.
    regions: BTreeMap<usize, Region>,
    /// Old blocks that may hold a pointer into the nursery (write-barrier
    /// output). Seeded as roots for a minor collection, cleared after every
    /// collection. Deduped by the `remembered` header bit.
    remembered: Vec<usize>,
    /// Cheap pre-filter bounds for candidate words.
    lo: usize,
    hi: usize,
    /// Live payload bytes (young + old).
    live: usize,
    /// Live payload bytes in the old generation (drives the major trigger).
    old_live: usize,
    /// Live block count (stats/log).
    live_blocks: usize,
    /// Bytes allocated since the last collection.
    since: usize,
    /// Collect (minor) when `since` exceeds this — a fixed nursery budget,
    /// since minor cost tracks the young set, not total live.
    threshold: usize,
    /// Promote to a major collection when `old_live` exceeds this (adapts to
    /// 2× old-live after each major).
    old_threshold: usize,
    /// Safepoints no-op while positive (callback reentry).
    defer: u32,
    /// Deepest stack address (recorded once by `main` before the script).
    stack_base: usize,
    /// Registered global root ranges (static fields): (addr, words).
    roots: Vec<(usize, usize)>,
    /// Collections run (stats).
    collections: u64,
    minor: u64,
    major: u64,
    /// Fresh bump allocations vs pool reuses since the last collection
    /// (VS_GC_LOG diagnostics).
    fresh: u64,
    reused: u64,
}

/// First major fires once the old generation reaches this; also the floor
/// for the adaptive old-gen threshold.
const INITIAL_OLD_THRESHOLD: usize = 4 << 20;
/// Bytes of nursery allocation between (minor) collections.
const NURSERY_TARGET: usize = 2 << 20;

thread_local! {
    /// Fast-path flag for [`safepoint`]: set by [`alloc`] when the debt
    /// crosses the threshold, so the per-loop/per-call safepoint is one
    /// thread-local load instead of a RefCell borrow.
    static PENDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static HEAP: RefCell<Heap> = const {
        RefCell::new(Heap {
            arenas: Vec::new(),
            cur: Vec::new(),
            free_lists: Vec::new(),
            regions: BTreeMap::new(),
            remembered: Vec::new(),
            lo: usize::MAX,
            hi: 0,
            live: 0,
            old_live: 0,
            live_blocks: 0,
            since: 0,
            threshold: NURSERY_TARGET,
            old_threshold: INITIAL_OLD_THRESHOLD,
            defer: 0,
            stack_base: 0,
            roots: Vec::new(),
            collections: 0,
            minor: 0,
            major: 0,
            fresh: 0,
            reused: 0,
        })
    };
}

impl Heap {
    /// Grows the per-class vectors so `class` is a valid index.
    fn ensure_class(&mut self, class: usize) {
        if self.free_lists.len() <= class {
            self.free_lists.resize_with(class + 1, Vec::new);
        }
        if self.cur.len() <= class {
            self.cur.resize(class + 1, None);
        }
    }

    /// Allocates a fresh arena for `stride`-byte blocks, registers it, and
    /// returns its index.
    fn new_arena(&mut self, stride: usize) -> usize {
        let n = (ARENA_BYTES / stride).max(1);
        let cap = n * stride;
        let layout = Layout::from_size_align(cap, BLOCK_ALIGN).expect("arena layout");
        // SAFETY: non-zero cap; zeroed so fresh bump slots have zero payloads.
        let p = unsafe { std::alloc::alloc_zeroed(layout) };
        assert!(!p.is_null(), "out of memory");
        let base = p as usize;
        let idx = self.arenas.len();
        self.arenas.push(Arena {
            base,
            cap,
            used: 0,
            stride,
            young: 0,
        });
        self.regions.insert(base, Region::Arena(idx));
        self.lo = self.lo.min(base);
        self.hi = self.hi.max(base + cap);
        idx
    }

    /// Carves a small block (payload `size`, already 16-rounded) from a pool
    /// entry or an arena, writes its (young) header, returns the payload.
    fn alloc_small(&mut self, size: usize, kind: Kind) -> usize {
        let class = size / 16;
        self.ensure_class(class);
        if let Some(hs) = self.free_lists[class].pop() {
            self.reused += 1;
            // The recycled slot belongs to some arena of this class; find it
            // so the young count is charged to the right arena.
            self.charge_young(hs);
            // SAFETY: `hs` is a block start from this class's pool.
            unsafe {
                let h = &mut *(hs as *mut Hdr);
                h.size = size as u32;
                h.kind = kind as u8;
                h.marked = 0;
                h.live = 1;
                h.age = GEN_YOUNG;
                h.remembered = 0;
                // Pooled payload may hold stale bytes; hand out zeroed.
                std::ptr::write_bytes((hs + HEADER) as *mut u8, 0, size);
            }
            return hs + HEADER;
        }
        self.fresh += 1;
        let stride = HEADER + size;
        let ai = match self.cur[class] {
            Some(ai) if self.arenas[ai].used + stride <= self.arenas[ai].cap => ai,
            _ => {
                let ai = self.new_arena(stride);
                self.cur[class] = Some(ai);
                ai
            }
        };
        let a = &mut self.arenas[ai];
        let hs = a.base + a.used;
        a.used += stride;
        a.young += 1;
        // SAFETY: `hs` is within the arena's zeroed region; payload stays 0.
        unsafe {
            let h = &mut *(hs as *mut Hdr);
            h.size = size as u32;
            h.kind = kind as u8;
            h.marked = 0;
            h.live = 1;
            h.age = GEN_YOUNG;
            h.remembered = 0;
        }
        hs + HEADER
    }

    /// Charges a recycled block start to its owning arena's young count.
    fn charge_young(&mut self, hs: usize) {
        if let Some((&base, Region::Arena(idx))) = self.regions.range(..=hs).next_back() {
            let idx = *idx;
            if hs < base + self.arenas[idx].cap {
                self.arenas[idx].young += 1;
            }
        }
    }

    /// Allocates a large (young) block from the system allocator.
    fn alloc_large(&mut self, size: usize, kind: Kind) -> usize {
        debug_assert!(size <= u32::MAX as usize, "large block too big for header");
        let total = HEADER + size;
        let layout = Layout::from_size_align(total, BLOCK_ALIGN).expect("large layout");
        // SAFETY: non-zero size; zeroed payload.
        let p = unsafe { std::alloc::alloc_zeroed(layout) };
        assert!(!p.is_null(), "out of memory");
        let base = p as usize;
        // SAFETY: fresh region of `total` bytes.
        unsafe {
            let h = &mut *(base as *mut Hdr);
            h.size = size as u32;
            h.kind = kind as u8;
            h.marked = 0;
            h.live = 1;
            h.age = GEN_YOUNG;
            h.remembered = 0;
        }
        self.regions.insert(base, Region::Large);
        self.lo = self.lo.min(base);
        self.hi = self.hi.max(base + total);
        base + HEADER
    }

    /// Payload start of the block containing `addr` (interior pointers
    /// allowed), if it is live and not already marked.
    fn find_block(&self, addr: usize) -> Option<usize> {
        if addr < self.lo || addr >= self.hi {
            return None;
        }
        let (&base, region) = self.regions.range(..=addr).next_back()?;
        match region {
            Region::Arena(idx) => {
                let a = &self.arenas[*idx];
                if addr >= base + a.used {
                    return None;
                }
                let hs = base + ((addr - base) / a.stride) * a.stride;
                // SAFETY: `hs` is a block start within the bumped region.
                let h = unsafe { &*(hs as *const Hdr) };
                (h.live != 0 && h.marked == 0).then_some(hs + HEADER)
            }
            Region::Large => {
                // SAFETY: `base` is a live large block start.
                let h = unsafe { &*(base as *const Hdr) };
                let end = base + HEADER + h.size as usize;
                (addr < end && h.live != 0 && h.marked == 0).then_some(base + HEADER)
            }
        }
    }

    /// Recomputes the pre-filter bounds after a sweep (large frees can
    /// shrink them; arenas persist).
    fn recompute_bounds(&mut self) {
        let mut lo = usize::MAX;
        let mut hi = 0;
        for a in &self.arenas {
            lo = lo.min(a.base);
            hi = hi.max(a.base + a.cap);
        }
        for (&base, region) in &self.regions {
            if let Region::Large = region {
                // SAFETY: live large block start.
                let sz = unsafe { (*(base as *const Hdr)).size } as usize;
                lo = lo.min(base);
                hi = hi.max(base + HEADER + sz);
            }
        }
        self.lo = lo;
        self.hi = hi;
    }
}

/// Allocates a zeroed, 16-aligned GC block. Never collects (collection is
/// safepoint-only; see module docs).
pub fn alloc(size: usize, kind: Kind) -> *mut u8 {
    // Round to the 16-byte size-class grid (also the pool key).
    let size = (size.max(8) + 15) & !15;
    HEAP.with(|h| {
        let mut h = h.borrow_mut();
        let payload = if size <= MAX_SMALL {
            h.alloc_small(size, kind)
        } else {
            h.alloc_large(size, kind)
        };
        h.live += size;
        h.live_blocks += 1;
        h.since += size;
        if h.since > h.threshold && h.stack_base != 0 {
            PENDING.with(|p| p.set(true));
        }
        payload as *mut u8
    })
}

/// Write barrier: records `obj` in the remembered set if it is an old block
/// (it may now hold a pointer into the nursery). Idempotent per collection
/// via the `remembered` header bit. Called inline from compiled field/cell
/// stores (already guarded by a `gen == old` check) and from the runtime
/// container setters (which call unconditionally — the check is here too).
///
/// # Safety
/// `obj` must be null or the payload of a live GC block.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_gc_remember(obj: *mut u8) {
    if obj.is_null() {
        return;
    }
    let addr = obj as usize;
    HEAP.with(|h| {
        let mut h = h.borrow_mut();
        // SAFETY: caller contract — `obj` is a live block payload.
        let hd = unsafe { hdr(addr) };
        if hd.age == GEN_OLD && hd.remembered == 0 {
            hd.remembered = 1;
            h.remembered.push(addr);
        }
    });
}

/// Safe wrapper over [`vs_gc_remember`] for the runtime's own container
/// setters (which know they hold a live block or null).
pub(crate) fn remember(obj: *const u8) {
    // SAFETY: callers pass a live GC block payload or null.
    unsafe { vs_gc_remember(obj as *mut u8) };
}

/// Records the deepest stack address (called once from `main` before the
/// script runs; scanning covers `[current SP, stack_base)`).
pub fn record_stack_base(p: *const u8) {
    HEAP.with(|h| h.borrow_mut().stack_base = p as usize);
}

/// Registers a global root range of `words` machine words (static fields;
/// called from the compiled prologue).
pub fn add_root(addr: *const u8, words: usize) {
    HEAP.with(|h| h.borrow_mut().roots.push((addr as usize, words)));
}

/// Enters a GC-deferred section (callback reentry from runtime internals).
pub fn defer() -> DeferGuard {
    HEAP.with(|h| h.borrow_mut().defer += 1);
    DeferGuard
}

/// See [`defer`].
pub struct DeferGuard;

impl Drop for DeferGuard {
    fn drop(&mut self) {
        HEAP.with(|h| h.borrow_mut().defer -= 1);
    }
}

/// The current defer depth (saved by exception handlers: a `longjmp` past
/// a [`DeferGuard`] skips its Drop, so `catch` restores the saved depth).
pub fn defer_depth() -> u32 {
    HEAP.with(|h| h.borrow().defer)
}

/// Restores the defer depth after unwinding (see [`defer_depth`]).
pub fn set_defer_depth(depth: u32) {
    HEAP.with(|h| h.borrow_mut().defer = depth);
}

/// Live payload bytes (stats/tests).
pub fn live_bytes() -> usize {
    HEAP.with(|h| h.borrow().live)
}

/// Backend-emitted safepoint: collect if the allocation debt is due and
/// no runtime frames are live. The hot path is a single thread-local
/// flag read (set by [`alloc`] on threshold crossing).
pub fn safepoint() {
    if !PENDING.with(std::cell::Cell::get) {
        return;
    }
    let deferred = HEAP.with(|h| h.borrow().defer != 0);
    if deferred {
        // Keep the flag: the next safepoint after the guard drops collects.
        return;
    }
    PENDING.with(|p| p.set(false));
    collect_impl(false);
}

/// Forces a full (major) collection (System.gc()). Safe to call from a
/// native only because natives are invoked directly from compiled code with
/// all their arguments rooted in compiled frames.
pub fn collect() {
    collect_impl(true);
}

/// Runs a collection. `force_major` requests a full trace; otherwise a minor
/// collection runs unless the old generation has outgrown its threshold.
fn collect_impl(force_major: bool) {
    // Spill callee-saved registers onto the stack so the stack scan sees
    // them (the classic conservative-GC trick; MMgc does the same).
    let mut regs = std::mem::MaybeUninit::<[u8; 256]>::uninit();
    #[cfg(not(windows))]
    unsafe extern "C" {
        fn setjmp(buf: *mut u8) -> i32;
    }
    #[cfg(windows)]
    unsafe extern "C" {
        /// The runtime's own Win64 register spill (winjmp.rs).
        #[link_name = "vs_setjmp"]
        fn setjmp(buf: *mut u8) -> i32;
    }
    // SAFETY: buffer is large enough for any platform jmp_buf we target
    // (macOS arm64: 192 bytes; x86-64 SysV: 148; Win64: 256). We never
    // longjmp to it.
    unsafe { setjmp(regs.as_mut_ptr() as *mut u8) };

    HEAP.with(|h| {
        let mut h = h.borrow_mut();
        let minor = !force_major && h.old_live < h.old_threshold;
        let mut work: Vec<usize> = Vec::new();

        // Roots: machine stack (this frame's locals sit below every live
        // compiled frame), spilled registers, globals, in-flight exception.
        // In a minor collection only *young* referents are queued; old
        // blocks are assumed live and are not traced.
        let sp = regs.as_ptr() as usize;
        let base = h.stack_base;
        if base > sp {
            scan_words(&h, sp, (base - sp) / 8, minor, &mut work);
        }
        scan_words(&h, regs.as_ptr() as usize, 256 / 8, minor, &mut work);
        let roots = h.roots.clone();
        for (addr, words) in roots {
            scan_words(&h, addr, words, minor, &mut work);
        }
        mark_any(&h, &crate::exc::current_peek(), minor, &mut work);

        // Remembered set: old blocks that may point into the nursery. Seed
        // their young referents as roots (minor only), and clear the dedup
        // bits — the barrier rebuilds the set after this collection.
        let remembered = std::mem::take(&mut h.remembered);
        for &b in &remembered {
            // SAFETY: remembered entries are live old block payloads.
            let (kind, size) = unsafe {
                let hb = hdr(b);
                hb.remembered = 0;
                (Kind::from_u8(hb.kind), hb.size as usize)
            };
            if minor {
                trace_children(&h, b, kind, size, minor, &mut work);
            }
        }

        // Mark: trace until the worklist drains. In a minor collection the
        // worklist only ever holds young blocks (scan/consider filter old).
        while let Some(payload) = work.pop() {
            // SAFETY: worklist entries are payloads returned by find_block.
            let (kind, size) = unsafe {
                let b = hdr(payload);
                if b.marked != 0 {
                    continue;
                }
                b.marked = 1;
                (Kind::from_u8(b.kind), b.size as usize)
            };
            trace_children(&h, payload, kind, size, minor, &mut work);
        }

        // Sweep. Small blocks: walk each arena's bumped region linearly. A
        // minor sweep skips arenas with no young blocks, and skips old
        // blocks within the arenas it does walk. Survivors are promoted in
        // place (gen := old); the dead are recycled into the pool.
        let mut freed_blocks: usize = 0;
        let mut freed_bytes: usize = 0;
        let mut promoted: usize = 0;

        let arenas: Vec<(usize, usize, usize, usize)> = h
            .arenas
            .iter()
            .map(|a| (a.base, a.used, a.stride, a.young))
            .collect();
        for (ai, (abase, used, stride, young)) in arenas.into_iter().enumerate() {
            if minor && young == 0 {
                continue;
            }
            let mut off = 0;
            while off < used {
                let hs = abase + off;
                off += stride;
                // SAFETY: `hs` is a block start within the bumped region.
                let (live, marked, age, kind, size) = unsafe {
                    let b = &*(hs as *const Hdr);
                    (b.live, b.marked, b.age, Kind::from_u8(b.kind), b.size as usize)
                };
                if live == 0 || (minor && age == GEN_OLD) {
                    continue;
                }
                if marked != 0 {
                    // Survivor. Promote young → old in place.
                    // SAFETY: same block.
                    unsafe {
                        let b = &mut *(hs as *mut Hdr);
                        b.marked = 0;
                        if b.age == GEN_YOUNG {
                            b.age = GEN_OLD;
                            h.old_live += size;
                            h.arenas[ai].young -= 1;
                            promoted += 1;
                        }
                    }
                    continue;
                }
                // Dead: run its side-storage destructor and recycle the slot.
                drop_side_storage(hs + HEADER, kind);
                // SAFETY: same block.
                unsafe { (*(hs as *mut Hdr)).live = 0 };
                h.live -= size;
                h.live_blocks -= 1;
                freed_blocks += 1;
                freed_bytes += size;
                if age == GEN_OLD {
                    h.old_live -= size;
                } else {
                    h.arenas[ai].young -= 1;
                }
                let class = size / 16;
                h.ensure_class(class);
                h.free_lists[class].push(hs);
            }
        }

        // Large blocks: same rules, tracked individually in `regions`.
        // Classify under the `&h.regions` borrow, then mutate `h` after it
        // releases (promotions must charge `old_live`).
        let mut dead_large: Vec<usize> = Vec::new();
        let mut promoted_large: Vec<usize> = Vec::new();
        for (&lbase, region) in &h.regions {
            if let Region::Large = region {
                // SAFETY: live large block start.
                let (live, marked, age) = unsafe {
                    let b = &*(lbase as *const Hdr);
                    (b.live, b.marked, b.age)
                };
                if live == 0 || (minor && age == GEN_OLD) {
                    continue;
                }
                if marked != 0 {
                    // SAFETY: clearing the mark touches the header, not the
                    // `regions` map we are iterating.
                    unsafe { (*(lbase as *mut Hdr)).marked = 0 };
                    if age == GEN_YOUNG {
                        promoted_large.push(lbase);
                    }
                } else {
                    dead_large.push(lbase);
                }
            }
        }
        for lbase in promoted_large {
            // SAFETY: live large block start; promote in place.
            let size = unsafe {
                let b = &mut *(lbase as *mut Hdr);
                b.age = GEN_OLD;
                b.size as usize
            };
            h.old_live += size;
            promoted += 1;
        }
        for lbase in dead_large {
            // SAFETY: dead large block start; layout matches alloc_large.
            let (kind, size, age) = unsafe {
                let b = &*(lbase as *const Hdr);
                (Kind::from_u8(b.kind), b.size as usize, b.age)
            };
            drop_side_storage(lbase + HEADER, kind);
            h.regions.remove(&lbase);
            h.live -= size;
            h.live_blocks -= 1;
            freed_blocks += 1;
            freed_bytes += size;
            if age == GEN_OLD {
                h.old_live -= size;
            }
            // SAFETY: region came from alloc_large with this exact layout.
            unsafe {
                let layout = Layout::from_size_align_unchecked(HEADER + size, BLOCK_ALIGN);
                std::alloc::dealloc(lbase as *mut u8, layout);
            }
        }

        h.since = 0;
        h.collections += 1;
        // Size the next nursery to the live set (floored), not a fixed budget:
        // a nursery smaller than a data structure under construction forces a
        // minor mid-build that promotes everything and frees nothing.
        h.threshold = NURSERY_TARGET.max(h.live);
        if minor {
            h.minor += 1;
        } else {
            h.major += 1;
            h.old_threshold = INITIAL_OLD_THRESHOLD.max(h.old_live * 2);
        }
        PENDING.with(|p| p.set(false));
        if std::env::var_os("VS_GC_LOG").is_some() {
            let pooled: usize = h.free_lists.iter().map(Vec::len).sum();
            eprintln!(
                "gc#{} {}: freed {} blocks / {} bytes, promoted {}, live {} blocks / {} bytes (old {}), fresh {} reused {} pooled {}",
                h.collections,
                if minor { "minor" } else { "MAJOR" },
                freed_blocks,
                freed_bytes,
                promoted,
                h.live_blocks,
                h.live,
                h.old_live,
                h.fresh,
                h.reused,
                pooled
            );
            h.fresh = 0;
            h.reused = 0;
        }
        h.recompute_bounds();
    });
}

/// Queues the block containing `addr` (interior pointers allowed). In a
/// minor collection, old blocks are assumed live and not queued.
fn consider(h: &Heap, addr: usize, minor: bool, work: &mut Vec<usize>) {
    if let Some(p) = h.find_block(addr) {
        // SAFETY: `p` is a live block payload returned by find_block.
        if !minor || unsafe { is_young(p) } {
            work.push(p);
        }
    }
}

/// Conservatively scans `words` machine words at `addr`: any word that
/// points into a block queues that block (interior pointers count).
fn scan_words(h: &Heap, addr: usize, words: usize, minor: bool, work: &mut Vec<usize>) {
    for i in 0..words {
        // SAFETY: callers pass ranges they own (stack span, register
        // spill buffer, registered globals, or a live block's payload).
        let w = unsafe { *((addr + i * 8) as *const usize) };
        consider(h, w, minor, work);
    }
}

/// Queues the referent of a boxed value, if it is a GC reference.
fn mark_any(h: &Heap, v: &VsAny, minor: bool, work: &mut Vec<usize>) {
    match v.tag() {
        Tag::String | Tag::Object | Tag::Array | Tag::Vector | Tag::Function => {
            consider(h, v.data as usize, minor, work);
        }
        _ => {}
    }
}

/// Queues the GC references held by the block at `payload` (of the given
/// kind/size), following its precise layout.
fn trace_children(h: &Heap, payload: usize, kind: Kind, size: usize, minor: bool, work: &mut Vec<usize>) {
    match kind {
        Kind::Raw => scan_words(h, payload, size / 8, minor, work),
        Kind::String => {}
        Kind::Array => {
            // SAFETY: block layout fixed by seq::new_array.
            let a = unsafe { &*(payload as *const crate::seq::VsArray) };
            for v in a.data.borrow().iter() {
                mark_any(h, v, minor, work);
            }
        }
        Kind::Vector => {
            // SAFETY: block layout fixed by seq::new_vector.
            let v = unsafe { &*(payload as *const crate::seq::VsVector) };
            // Numeric (unboxed) vectors are GC leaves — no references to
            // trace. Only boxed vectors carry `VsAny` elements.
            if v.kind == crate::seq::VEC_BOXED {
                // SAFETY: `data` holds `len` initialized `VsAny`s.
                let elems =
                    unsafe { std::slice::from_raw_parts(v.data as *const VsAny, v.len as usize) };
                for e in elems {
                    mark_any(h, e, minor, work);
                }
            }
        }
        Kind::PropMap => {
            // SAFETY: block layout fixed by object::expando.
            let m = unsafe { &*(payload as *const crate::object::PropMap) };
            for (_, v) in m.iter() {
                mark_any(h, v, minor, work);
            }
        }
        Kind::RegExp => {
            // SAFETY: block layout fixed by regexp::new.
            let r = unsafe { &*(payload as *const crate::regexp::VsRegExp) };
            consider(h, r.source as usize, minor, work);
        }
        // Sockets hold no GC references.
        Kind::Socket => {}
    }
}

/// Runs a dead block's kind destructor for its out-of-heap side storage
/// (the block itself is recycled or freed by the sweep loop).
fn drop_side_storage(addr: usize, kind: Kind) {
    // SAFETY: the block is unreachable (sweep proved it) and its layout
    // is fixed by the allocation site for its kind.
    unsafe {
        match kind {
            Kind::Raw => {}
            Kind::String => {
                let s = &*(addr as *const crate::string::VsString);
                if s.len > 0 {
                    drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                        s.data as *mut u16,
                        s.len as usize,
                    )));
                }
            }
            Kind::Array => {
                std::ptr::drop_in_place(addr as *mut crate::seq::VsArray);
            }
            Kind::Vector => {
                std::ptr::drop_in_place(addr as *mut crate::seq::VsVector);
            }
            Kind::PropMap => {
                std::ptr::drop_in_place(addr as *mut crate::object::PropMap);
            }
            Kind::RegExp => {
                std::ptr::drop_in_place(addr as *mut crate::regexp::VsRegExp);
            }
            Kind::Socket => {
                std::ptr::drop_in_place(addr as *mut crate::socket::VsSocket);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rss_kb() -> usize {
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .expect("ps");
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .expect("rss")
    }

    /// Churn far past the threshold: the footprint must plateau, not grow
    /// with total allocation volume.
    #[test]
    fn churn_footprint_plateaus() {
        let base = 0u8;
        record_stack_base(&base as *const u8);
        let mut readings = Vec::new();
        for _ in 0..12 {
            for _ in 0..65_000 {
                alloc(16, Kind::Raw);
                alloc(16, Kind::Raw);
                alloc(32, Kind::Raw);
            }
            collect();
            readings.push(rss_kb());
        }
        let early = readings[2];
        let late = *readings.last().expect("readings");
        assert!(
            late < early + 8 * 1024,
            "footprint grew {early} KB -> {late} KB across churn cycles"
        );
    }
}
