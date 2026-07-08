//! Garbage collector (SPECS §7): conservative mark-sweep.
//!
//! Design (avmplus ships MMgc, a conservative mark-sweep collector with
//! stack scanning — we follow the same shape, sized for v1):
//!
//! - Every runtime allocation (objects, strings, arrays, vectors, closures,
//!   cells, expando maps) goes through [`alloc`], which records the block
//!   in a per-thread registry keyed by address.
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
//! Single-threaded (matches the v1 runtime); all state is thread-local.

use std::cell::RefCell;
use std::collections::BTreeMap;

use crate::any::{Tag, VsAny};

/// What a block holds — drives tracing and sweeping.
#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    /// Plain words (object instances, closures, cells, closure envs):
    /// scanned conservatively, no destructor.
    Raw,
    /// A `VsString` header; its UTF-16 buffer is freed on sweep.
    String,
    /// A `VsArray`; elements live in a Rust `Vec` traced precisely.
    Array,
    /// A `VsVector`; same shape as Array.
    Vector,
    /// An expando `PropMap`; values traced precisely, map dropped.
    PropMap,
    /// A `VsRegExp`; its source string is traced, the compiled program
    /// dropped on sweep.
    RegExp,
}

struct Block {
    size: usize,
    kind: Kind,
    marked: bool,
}

/// Small blocks are pooled in 16-byte size classes up to this; larger
/// blocks go straight to the system allocator. Pooling matters: batch
/// free-then-realloc churn fragments macOS malloc (measured: RSS grows
/// linearly with total churn), while recycling swept blocks in-collector
/// keeps the footprint at the high-water mark.
const MAX_SMALL: usize = 1024;

struct Heap {
    /// Blocks by payload start address (BTreeMap: interior-pointer lookup
    /// is a predecessor query).
    blocks: BTreeMap<usize, Block>,
    /// Recycled small blocks by size class (index = size / 16).
    free_lists: Vec<Vec<usize>>,
    /// Cheap pre-filter bounds for candidate words.
    lo: usize,
    hi: usize,
    /// Live payload bytes.
    live: usize,
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
    /// Fresh system allocations vs pool reuses since the last collection
    /// (VS_GC_LOG diagnostics).
    fresh: u64,
    reused: u64,
    /// Reused sweep scratch (a fresh multi-MB Vec per collection leaves
    /// macOS malloc growing the footprint every cycle).
    dead_scratch: Vec<(usize, usize, Kind)>,
}

const INITIAL_THRESHOLD: usize = 4 << 20;

thread_local! {
    /// Fast-path flag for [`safepoint`]: set by [`alloc`] when the debt
    /// crosses the threshold, so the per-loop/per-call safepoint is one
    /// thread-local load instead of a RefCell borrow.
    static PENDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static HEAP: RefCell<Heap> = const {
        RefCell::new(Heap {
            blocks: BTreeMap::new(),
            free_lists: Vec::new(),
            lo: usize::MAX,
            hi: 0,
            live: 0,
            since: 0,
            threshold: INITIAL_THRESHOLD,
            defer: 0,
            stack_base: 0,
            roots: Vec::new(),
            collections: 0,
            fresh: 0,
            reused: 0,
            dead_scratch: Vec::new(),
        })
    };
}

/// Allocates a zeroed, 8-aligned GC block. Never collects (collection is
/// safepoint-only; see module docs).
pub fn alloc(size: usize, kind: Kind) -> *mut u8 {
    // Round to the 16-byte size-class grid (also the pool key).
    let size = (size.max(8) + 15) & !15;
    HEAP.with(|h| {
        let mut h = h.borrow_mut();
        let recycled = (size <= MAX_SMALL)
            .then(|| h.free_lists.get_mut(size / 16).and_then(Vec::pop))
            .flatten();
        match recycled {
            Some(_) => h.reused += 1,
            None => h.fresh += 1,
        }
        let p = match recycled {
            Some(addr) => {
                // SAFETY: pooled block of exactly `size` bytes.
                unsafe { std::ptr::write_bytes(addr as *mut u8, 0, size) };
                addr as *mut u8
            }
            None => {
                let layout = std::alloc::Layout::from_size_align(size, 8).expect("gc layout");
                // SAFETY: non-zero size (clamped above).
                let p = unsafe { std::alloc::alloc_zeroed(layout) };
                assert!(!p.is_null(), "out of memory");
                p
            }
        };
        let addr = p as usize;
        h.lo = h.lo.min(addr);
        h.hi = h.hi.max(addr + size);
        h.live += size;
        h.since += size;
        if h.since > h.threshold && h.stack_base != 0 {
            PENDING.with(|p| p.set(true));
        }
        h.blocks.insert(
            addr,
            Block {
                size,
                kind,
                marked: false,
            },
        );
        p
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
    unsafe extern "C" {
        fn setjmp(buf: *mut u8) -> i32;
    }
    // SAFETY: buffer is large enough for any platform jmp_buf we target
    // (macOS arm64: 192 bytes; x86-64: 148). We never longjmp to it.
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

        // Mark: trace until the worklist drains.
        while let Some(addr) = work.pop() {
            let (kind, size) = {
                let b = h.blocks.get_mut(&addr).expect("worklist block");
                if b.marked {
                    continue;
                }
                b.marked = true;
                (b.kind, b.size)
            };
            match kind {
                Kind::Raw => scan_words(&h, addr, size / 8, &mut work),
                Kind::String => {}
                Kind::Array => {
                    // SAFETY: block layout fixed by seq::new_array.
                    let a = unsafe { &*(addr as *const crate::seq::VsArray) };
                    for v in a.data.borrow().iter() {
                        mark_any(&h, v, &mut work);
                    }
                }
                Kind::Vector => {
                    // SAFETY: block layout fixed by seq::new_vector.
                    let v = unsafe { &*(addr as *const crate::seq::VsVector) };
                    for e in v.data.borrow().iter() {
                        mark_any(&h, e, &mut work);
                    }
                }
                Kind::PropMap => {
                    // SAFETY: block layout fixed by object::expando.
                    let m = unsafe { &*(addr as *const crate::object::PropMap) };
                    for (_, v) in m.iter() {
                        mark_any(&h, v, &mut work);
                    }
                }
                Kind::RegExp => {
                    // SAFETY: block layout fixed by regexp::new.
                    let r = unsafe { &*(addr as *const crate::regexp::VsRegExp) };
                    if let Some(start) = h.find_block(r.source as usize) {
                        work.push(start);
                    }
                }
            }
        }

        // Sweep.
        let mut dead = std::mem::take(&mut h.dead_scratch);
        dead.clear();
        dead.extend(
            h.blocks
                .iter()
                .filter(|(_, b)| !b.marked)
                .map(|(&a, b)| (a, b.size, b.kind)),
        );
        for &(addr, size, kind) in &dead {
            h.blocks.remove(&addr);
            h.live -= size;
            drop_side_storage(addr, kind);
            if size <= MAX_SMALL {
                let class = size / 16;
                if h.free_lists.len() <= class {
                    h.free_lists.resize_with(class + 1, Vec::new);
                }
                h.free_lists[class].push(addr);
            } else {
                // SAFETY: block came from alloc with this exact layout.
                unsafe {
                    let layout = std::alloc::Layout::from_size_align_unchecked(size, 8);
                    std::alloc::dealloc(addr as *mut u8, layout);
                }
            }
        }
        for b in h.blocks.values_mut() {
            b.marked = false;
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
                dead.len(),
                dead.iter().map(|d| d.1).sum::<usize>(),
                h.blocks.len(),
                h.live,
                h.fresh,
                h.reused,
                pooled
            );
            h.fresh = 0;
            h.reused = 0;
        }
        h.dead_scratch = dead;
        let bounds = match (h.blocks.first_key_value(), h.blocks.last_key_value()) {
            (Some((&lo, _)), Some((&hi, b))) => (lo, hi + b.size),
            _ => (usize::MAX, 0),
        };
        (h.lo, h.hi) = bounds;
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

impl Heap {
    /// Block containing `addr` (interior pointers allowed), if unmarked
    /// work remains for it.
    fn find_block(&self, addr: usize) -> Option<usize> {
        if addr < self.lo || addr >= self.hi {
            return None;
        }
        let (&start, b) = self.blocks.range(..=addr).next_back()?;
        (addr < start + b.size && !b.marked).then_some(start)
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
