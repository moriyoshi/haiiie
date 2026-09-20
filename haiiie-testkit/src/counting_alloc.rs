//! A counting global allocator, so allocation can be asserted rather than hoped.
//!
//! # Why this is a test and not a benchmark
//!
//! The scan's non-materializing structure -- one accumulator and one scratch
//! plane reused across every block -- is a *parallel implementation* of the
//! obvious one that allocates per block per key. Both return identical results,
//! so no correctness test can tell them apart, and a benchmark only says
//! "slower", which is noise-shaped and easy to explain away. An allocation count
//! is the only instrument that fails loudly when the structure decays.
//!
//! Adopted from yesnodb's `tests/allocation.rs`, which exists for the same
//! reason and says so.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static COUNT: AtomicU64 = AtomicU64::new(0);
static ARMED: AtomicBool = AtomicBool::new(false);

/// Wraps the system allocator and counts allocations while armed.
pub struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            COUNT.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: `layout` is forwarded unchanged to the system allocator, which
        // is the only allocator in play; this wrapper adds a counter and nothing
        // else. The invariant `alloc` requires -- a non-zero-size layout -- is
        // the caller's and is untouched here.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` and `layout` are forwarded unchanged; the pointer came
        // from `System.alloc` above, since this wrapper allocates nothing itself.
        unsafe { System.dealloc(ptr, layout) }
    }
}

/// Serialize a test against every other test that measures allocation.
///
/// The counter is process-wide, so two tests measuring at once measure each
/// other. Being in a dedicated binary is **not** sufficient: libtest still runs
/// the tests within one binary in parallel, and a neighbour's fixture setup
/// lands in your count. An earlier version documented the constraint and did
/// not enforce it, which passed under `--test-threads=1` and failed in the gate
/// -- a test that is correct only under a flag the gate does not pass is not a
/// test.
///
/// Hold this for the **whole** test body, setup included, not just the measured
/// region.
pub fn lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    // Poisoning only means some other test panicked; the counter is still sound.
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Count the allocations performed by `f`.
///
/// The caller must hold [`lock`] for the whole test, setup included.
pub fn count<T>(f: impl FnOnce() -> T) -> (T, u64) {
    COUNT.store(0, Ordering::SeqCst);
    ARMED.store(true, Ordering::SeqCst);
    let out = f();
    ARMED.store(false, Ordering::SeqCst);
    (out, COUNT.load(Ordering::SeqCst))
}
