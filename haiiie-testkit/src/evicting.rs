//! A store that evicts its snapshots on a schedule, so the resume path is
//! reachable from a test.
//!
//! # Why a double rather than a real eviction
//!
//! yesnodb evicts a snapshot to bound space amplification, which needs a
//! long-lived reader, sustained writes and a checkpoint policy tuned to fire --
//! a slow, timing-dependent setup that would make a correctness test flaky. The
//! contract haiiie depends on is much narrower: **a read may fail with an
//! eviction error, and a fresh snapshot may succeed.** That is what this
//! injects, deterministically, on the read after a chosen count.
//!
//! It is a fault injector, not a performance model, and it deliberately reports
//! eviction through `Error::SnapshotExpired` rather than a yesnodb error --
//! eviction is part of the `SetStore` contract, and a store that is not yesnodb
//! must be able to raise it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use haiiie_core::slice::BlockMask;
use haiiie_core::store::{Batch, Lanes, SetSnapshot, SetStore, Version};
use haiiie_core::{Error, Result};

use crate::MemStore;

/// Wraps a store and fails reads on a schedule.
///
/// # Generic over the inner store, and that is load-bearing
///
/// It wrapped [`MemStore`] concretely, and **a fault injector can only cover the
/// implementation it wraps**. `MemStore` returns `None` from
/// [`SetSnapshot::open_lanes`], so no cursor ever existed under injection, and
/// every invariant belonging to a *stateful* cursor was unreachable from this
/// suite -- including a scan's duty to rebuild its cursors against the fresh
/// snapshot after a retry. Reverting that rebuild left the whole workspace
/// green.
///
/// That blind spot went unnoticed twice. Wrapping a store that does offer
/// cursors is what makes those paths reachable, so the parameter is the fix
/// rather than a convenience.
pub struct EvictingStore<S: SetStore = MemStore> {
    inner: S,
    /// Reads before the next eviction. Reset after each one fires.
    every: u64,
    counter: Arc<AtomicU64>,
    fired: Arc<AtomicU64>,
    armed: Arc<AtomicU64>,
    on_evict: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl<S: SetStore> std::fmt::Debug for EvictingStore<S> {
    /// Hand-written because a hook closure has no `Debug`. It reports the
    /// schedule and whether a hook is installed, which is what a failing test
    /// needs to know.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EvictingStore")
            .field("every", &self.every)
            .field("fired", &self.fired.load(Ordering::SeqCst))
            .field("armed", &self.armed.load(Ordering::SeqCst))
            .field("on_evict", &self.on_evict.is_some())
            .finish_non_exhaustive()
    }
}

impl EvictingStore<MemStore> {
    /// Evict once every `every` block reads, at most `budget` times in total.
    #[must_use]
    pub fn new(every: u64, budget: u64) -> Self {
        Self::wrapping(MemStore::new(), every, budget)
    }
}

impl<S: SetStore> EvictingStore<S> {
    /// The same, over a store the caller supplies.
    ///
    /// Use this with a store whose snapshots offer cursors when the fault being
    /// injected should reach them; `MemStore` cannot exercise those paths.
    #[must_use]
    pub fn wrapping(inner: S, every: u64, budget: u64) -> Self {
        Self {
            inner,
            on_evict: None,
            every,
            counter: Arc::new(AtomicU64::new(0)),
            fired: Arc::new(AtomicU64::new(0)),
            armed: Arc::new(AtomicU64::new(budget)),
        }
    }

    /// Run `f` at the moment a fault fires, before the error is returned.
    ///
    /// # Why an injector needs this
    ///
    /// Without it, this double **simulates** eviction: it returns an error and
    /// leaves the underlying snapshot perfectly valid. A scan that retries then
    /// reads the same bytes whether or not it rebuilt its cursors against the
    /// fresh snapshot, so the two are indistinguishable by any assertion about
    /// results -- and deleting the rebuild left the whole suite green.
    ///
    /// A hook that **commits a change** is what closes that: the retry's
    /// snapshot sees a version the stale cursor cannot, and the difference
    /// becomes a difference in the answer. The fault and the write have to be
    /// simultaneous, which is exactly what a callback at the injection point
    /// gives and what a writer thread racing the scan does not.
    #[must_use]
    pub fn on_evict(mut self, f: impl Fn() + Send + Sync + 'static) -> Self {
        self.on_evict = Some(Arc::new(f));
        self
    }

    /// How many evictions actually fired. A test that does not check this can
    /// pass without the fault ever being injected.
    #[must_use]
    pub fn evictions(&self) -> u64 {
        self.fired.load(Ordering::SeqCst)
    }

    /// Stop injecting, so ingest and setup are not disrupted.
    /// The wrapped store, for setup a fault injector should not disturb --
    /// checkpointing a real store, for instance.
    #[must_use]
    pub fn inner(&self) -> &S {
        &self.inner
    }

    pub fn disarm(&self) {
        self.armed.store(0, Ordering::SeqCst);
    }

    /// Resume injecting with a fresh budget.
    /// Arm `budget` faults, and **restart the read count**.
    ///
    /// Resetting is what makes the timing of a fault a property of the test
    /// rather than of everything that ran before it. The counter is shared and
    /// monotonic, so without this the next fault lands at whatever multiple of
    /// `every` happens to come round -- fine for "does the resume path work",
    /// useless for "was a cursor open when the fault arrived", which depends on
    /// *where* in a scan the fault lands.
    pub fn arm(&self, budget: u64) {
        self.counter.store(0, Ordering::SeqCst);
        self.arm_keeping_count(budget);
    }

    /// Arm without disturbing the read count.
    pub fn arm_keeping_count(&self, budget: u64) {
        self.armed.store(budget, Ordering::SeqCst);
    }
}

/// A read view that can refuse.
pub struct EvictingSnapshot<S: SetStore> {
    on_evict: Option<Arc<dyn Fn() + Send + Sync>>,
    inner: S::Snap,
    every: u64,
    counter: Arc<AtomicU64>,
    fired: Arc<AtomicU64>,
    armed: Arc<AtomicU64>,
}

impl<S: SetStore> std::fmt::Debug for EvictingSnapshot<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EvictingSnapshot")
            .field("on_evict", &self.on_evict.is_some())
            .finish_non_exhaustive()
    }
}

impl<S: SetStore> EvictingSnapshot<S> {
    fn tick(&self) -> Result<()> {
        if self.armed.load(Ordering::SeqCst) == 0 {
            return Ok(());
        }
        let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        if self.every > 0 && n.is_multiple_of(self.every) {
            self.armed.fetch_sub(1, Ordering::SeqCst);
            self.fired.fetch_add(1, Ordering::SeqCst);
            if let Some(f) = self.on_evict.as_ref() {
                f();
            }
            return Err(Error::SnapshotExpired {
                version: self.inner.version(),
            });
        }
        Ok(())
    }
}

impl<S: SetStore> SetStore for EvictingStore<S> {
    type Snap = EvictingSnapshot<S>;

    fn snapshot(&self) -> Result<Self::Snap> {
        Ok(EvictingSnapshot {
            on_evict: self.on_evict.clone(),
            inner: self.inner.snapshot()?,
            every: self.every,
            counter: Arc::clone(&self.counter),
            fired: Arc::clone(&self.fired),
            armed: Arc::clone(&self.armed),
        })
    }

    fn write(&self, batch: &Batch) -> Result<Version> {
        self.inner.write(batch)
    }
}

impl<S: SetStore> SetSnapshot for EvictingSnapshot<S> {
    fn version(&self) -> Version {
        self.inner.version()
    }
    fn load(&self, key: u64) -> Result<Vec<u64>> {
        self.inner.load(key)
    }
    fn key_range(&self, lo: u64, hi: u64) -> Result<Vec<u64>> {
        self.inner.key_range(lo, hi)
    }
    fn cardinality(&self, key: u64) -> Result<u64> {
        self.inner.cardinality(key)
    }
    fn contains(&self, key: u64, ordinal: u64) -> Result<bool> {
        self.inner.contains(key, ordinal)
    }
    fn max(&self, key: u64) -> Result<Option<u64>> {
        self.inner.max(key)
    }
    /// The injection point: a block read is what a scan does most of, so this
    /// is where an eviction realistically lands.
    fn load_block(&self, key: u64, block: u64, out: &mut BlockMask) -> Result<bool> {
        self.tick()?;
        self.inner.load_block(key, block, out)
    }

    /// **Forwarded, and injected into.**
    ///
    /// Leaving this at the trait default returns `None`, which tells the scan
    /// that no cursor is available -- so the scan falls back to `load_block` and
    /// every invariant belonging to a stateful cursor stays unreachable no
    /// matter what this store wraps. Making the wrapper generic was necessary
    /// and not sufficient; this is the other half, and a sabotage caught its
    /// absence.
    ///
    /// The faults land on `read` for the same reason they land on `load_block`:
    /// mid-block is where a stale cursor would still be consulted, and between
    /// blocks is where it would not.
    fn open_lanes(&self, keys: &[u64]) -> Result<Option<Box<dyn Lanes>>> {
        Ok(self.inner.open_lanes(keys)?.map(|l| {
            Box::new(EvictingLanes {
                inner: l,
                counter: Arc::clone(&self.counter),
                fired: Arc::clone(&self.fired),
                armed: Arc::clone(&self.armed),
                every: self.every,
                version: self.inner.version(),
                on_evict: self.on_evict.clone(),
            }) as Box<dyn Lanes>
        }))
    }
}

/// A cursor that fails on the same schedule as its snapshot's block reads.
struct EvictingLanes {
    inner: Box<dyn Lanes>,
    counter: Arc<AtomicU64>,
    fired: Arc<AtomicU64>,
    armed: Arc<AtomicU64>,
    every: u64,
    version: Version,
    on_evict: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl EvictingLanes {
    fn tick(&self) -> Result<()> {
        if self.armed.load(Ordering::SeqCst) > 0 {
            let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
            if self.every > 0 && n.is_multiple_of(self.every) {
                self.armed.fetch_sub(1, Ordering::SeqCst);
                self.fired.fetch_add(1, Ordering::SeqCst);
                if let Some(f) = self.on_evict.as_ref() {
                    f();
                }
                return Err(Error::SnapshotExpired {
                    version: self.version,
                });
            }
        }
        Ok(())
    }
}

impl Lanes for EvictingLanes {
    fn read(&mut self, lane: usize, block: u64, out: &mut BlockMask) -> Result<bool> {
        self.tick()?;
        self.inner.read(lane, block, out)
    }

    fn with_block(
        &mut self,
        lane: usize,
        block: u64,
        visit: &mut dyn FnMut(&BlockMask),
    ) -> Result<()> {
        // Forward the borrowed path as well as injecting: falling back through
        // read() would make every resumed forward query test the copying path.
        self.tick()?;
        self.inner.with_block(lane, block, visit)
    }
}
