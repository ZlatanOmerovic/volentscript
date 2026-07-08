//! Garbage collector (SPECS §7): conservative mark-sweep.
//!
//! Design (avmplus ships MMgc, a conservative mark-sweep collector with
//! stack scanning — we follow the same shape, sized for v1):
//!
//! - Every runtime allocation (objects, strings, arrays, vectors, closures,
//!   cells, expando maps) goes through [`alloc`]. Small blocks are carved
//!   from per-size-class **arenas** by bump allocation; each block carries a
//!   16-byte inline header (kind/size/mark/live) so metadata lookup is
//!   pointer arithmetic, not a map probe. Large blocks go straight to the
//!   system allocator. Only arenas and large blocks are registered in the
//!   `regions` map (tens of entries, not one per object), which is what the
//!   conservative scan does a predecessor query against.
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
//! Because the collector never relocates a block (a conservatively-scanned
//! word is only *maybe* a pointer, so it cannot be rewritten), this is a
//! strictly non-moving design — arenas give allocation speed, not compaction.
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

/// Per-block metadata, stored inline at the block start (the payload the
/// rest of the runtime sees begins [`HEADER`] bytes later). `_pad` rounds
/// the header to 16 bytes so payloads stay 16-aligned.
#[repr(C)]
struct Hdr {
    size: u32,
    kind: u8,
    marked: u8,
    live: u8,
    _pad: u8,
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
    /// Cheap pre-filter bounds for candidate words.
    lo: usize,
    hi: usize,
    /// Live payload bytes.
    live: usize,
    /// Live block count (stats/log).
    live_blocks: usize,
    /// Bytes allocated since the last collection.
    since: usize,
    /// Collect when `since` exceeds this (adapts to 2× live after each GC).
    threshold: usize,
    /// Safepoints no-op while positive (callback reentry).
    defer: u32,
    /// Deepest stack address (recorded once by `main` before the script).
    stack_base: usize,
    /// Registered global root ranges (static fields): (addr, words).
    roots: Vec<(usize, usize)>,
    /// Collections run (stats).
    collections: u64,
    /// Fresh bump allocations vs pool reuses since the last collection
    /// (VS_GC_LOG diagnostics).
    fresh: u64,
    reused: u64,
}

const INITIAL_THRESHOLD: usize = 4 << 20;

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
            lo: usize::MAX,
            hi: 0,
            live: 0,
            live_blocks: 0,
            since: 0,
            threshold: INITIAL_THRESHOLD,
            defer: 0,
            stack_base: 0,
            roots: Vec::new(),
            collections: 0,
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
        });
        self.regions.insert(base, Region::Arena(idx));
        self.lo = self.lo.min(base);
        self.hi = self.hi.max(base + cap);
        idx
    }

    /// Carves a small block (payload `size`, already 16-rounded) from a pool
    /// entry or an arena, writes its header, and returns the payload address.
    fn alloc_small(&mut self, size: usize, kind: Kind) -> usize {
        let class = size / 16;
        self.ensure_class(class);
        if let Some(hs) = self.free_lists[class].pop() {
            self.reused += 1;
            // SAFETY: `hs` is a block start from this class's pool.
            unsafe {
                let h = &mut *(hs as *mut Hdr);
                h.size = size as u32;
                h.kind = kind as u8;
                h.marked = 0;
                h.live = 1;
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
        // SAFETY: `hs` is within the arena's zeroed region; payload stays 0.
        unsafe {
            let h = &mut *(hs as *mut Hdr);
            h.size = size as u32;
            h.kind = kind as u8;
            h.marked = 0;
            h.live = 1;
        }
        hs + HEADER
    }

    /// Allocates a large block from the system allocator and registers it.
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
    collect();
}

/// Forces a collection (System.gc()). Safe to call from a native only
/// because natives are invoked directly from compiled code with all
/// their arguments rooted in compiled frames.
pub fn collect() {
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
        let mut work: Vec<usize> = Vec::new();

        // Roots: machine stack (this frame's locals sit below every live
        // compiled frame), spilled registers, globals, in-flight exception.
        let sp = regs.as_ptr() as usize;
        let base = h.stack_base;
        if base > sp {
            scan_words(&h, sp, (base - sp) / 8, &mut work);
        }
        scan_words(&h, regs.as_ptr() as usize, 256 / 8, &mut work);
        let roots = h.roots.clone();
        for (addr, words) in roots {
            scan_words(&h, addr, words, &mut work);
        }
        mark_any(&h, &crate::exc::current_peek(), &mut work);

        // Mark: trace until the worklist drains. `payload` is a block's
        // payload address; its header sits `HEADER` bytes before it.
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
            match kind {
                Kind::Raw => scan_words(&h, payload, size / 8, &mut work),
                Kind::String => {}
                Kind::Array => {
                    // SAFETY: block layout fixed by seq::new_array.
                    let a = unsafe { &*(payload as *const crate::seq::VsArray) };
                    for v in a.data.borrow().iter() {
                        mark_any(&h, v, &mut work);
                    }
                }
                Kind::Vector => {
                    // SAFETY: block layout fixed by seq::new_vector.
                    let v = unsafe { &*(payload as *const crate::seq::VsVector) };
                    // Numeric (unboxed) vectors are GC leaves — no references
                    // to trace. Only boxed vectors carry `VsAny` elements.
                    if v.kind == crate::seq::VEC_BOXED {
                        // SAFETY: `data` holds `len` initialized `VsAny`s.
                        let elems = unsafe {
                            std::slice::from_raw_parts(v.data as *const VsAny, v.len as usize)
                        };
                        for e in elems {
                            mark_any(&h, e, &mut work);
                        }
                    }
                }
                Kind::PropMap => {
                    // SAFETY: block layout fixed by object::expando.
                    let m = unsafe { &*(payload as *const crate::object::PropMap) };
                    for (_, v) in m.iter() {
                        mark_any(&h, v, &mut work);
                    }
                }
                Kind::RegExp => {
                    // SAFETY: block layout fixed by regexp::new.
                    let r = unsafe { &*(payload as *const crate::regexp::VsRegExp) };
                    if let Some(start) = h.find_block(r.source as usize) {
                        work.push(start);
                    }
                }
                // Sockets hold no GC references.
                Kind::Socket => {}
            }
        }

        // Sweep. Small blocks: walk each arena's bumped region linearly
        // (cache-friendly, no per-block map ops); recycle the dead into the
        // pool and clear survivors' marks. Large blocks: free to the system.
        let mut freed_blocks: usize = 0;
        let mut freed_bytes: usize = 0;

        let arenas: Vec<(usize, usize, usize)> =
            h.arenas.iter().map(|a| (a.base, a.used, a.stride)).collect();
        for (abase, used, stride) in arenas {
            let mut off = 0;
            while off < used {
                let hs = abase + off;
                off += stride;
                // SAFETY: `hs` is a block start within the bumped region.
                let (live, marked, kind, size) = unsafe {
                    let b = &*(hs as *const Hdr);
                    (b.live, b.marked, Kind::from_u8(b.kind), b.size as usize)
                };
                if live == 0 {
                    continue;
                }
                if marked != 0 {
                    // SAFETY: same block.
                    unsafe { (*(hs as *mut Hdr)).marked = 0 };
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
                let class = size / 16;
                h.ensure_class(class);
                h.free_lists[class].push(hs);
            }
        }

        // Large blocks: survivors keep their region; dead ones are freed.
        let mut dead_large: Vec<usize> = Vec::new();
        for (&lbase, region) in &h.regions {
            if let Region::Large = region {
                // SAFETY: live large block start.
                let b = unsafe { &mut *(lbase as *mut Hdr) };
                if b.live == 0 {
                    continue;
                }
                if b.marked != 0 {
                    b.marked = 0;
                } else {
                    dead_large.push(lbase);
                }
            }
        }
        for lbase in dead_large {
            // SAFETY: dead large block start; layout matches alloc_large.
            let (kind, size) = unsafe {
                let b = &*(lbase as *const Hdr);
                (Kind::from_u8(b.kind), b.size as usize)
            };
            drop_side_storage(lbase + HEADER, kind);
            h.regions.remove(&lbase);
            h.live -= size;
            h.live_blocks -= 1;
            freed_blocks += 1;
            freed_bytes += size;
            // SAFETY: region came from alloc_large with this exact layout.
            unsafe {
                let layout = Layout::from_size_align_unchecked(HEADER + size, BLOCK_ALIGN);
                std::alloc::dealloc(lbase as *mut u8, layout);
            }
        }

        h.since = 0;
        h.threshold = INITIAL_THRESHOLD.max(h.live * 2);
        h.collections += 1;
        PENDING.with(|p| p.set(false));
        if std::env::var_os("VS_GC_LOG").is_some() {
            let pooled: usize = h.free_lists.iter().map(Vec::len).sum();
            eprintln!(
                "gc#{}: freed {} blocks / {} bytes, live {} blocks / {} bytes, fresh {} reused {} pooled {}",
                h.collections,
                freed_blocks,
                freed_bytes,
                h.live_blocks,
                h.live,
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

/// Conservatively scans `words` machine words at `addr`: any word that
/// points into a block queues that block (interior pointers count).
fn scan_words(h: &Heap, addr: usize, words: usize, work: &mut Vec<usize>) {
    for i in 0..words {
        // SAFETY: callers pass ranges they own (stack span, register
        // spill buffer, registered globals, or a live block's payload).
        let w = unsafe { *((addr + i * 8) as *const usize) };
        if let Some(start) = h.find_block(w) {
            work.push(start);
        }
    }
}

/// Queues the referent of a boxed value, if it is a GC reference.
fn mark_any(h: &Heap, v: &VsAny, work: &mut Vec<usize>) {
    match v.tag() {
        Tag::String | Tag::Object | Tag::Array | Tag::Vector | Tag::Function => {
            if let Some(start) = h.find_block(v.data as usize) {
                work.push(start);
            }
        }
        _ => {}
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
