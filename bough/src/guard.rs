//! What a guard, a [`Listener`](crate::Listener) or an
//! [`Anchor`](crate::Anchor), shares with the runtime's entries for it
//! (RFD 3).
//!
//! The shared state counts the guard's owners. The guard is live while the
//! count is above zero, and when its last owner goes, the state counts one
//! released guard on the runtime's count, which the automatic collection
//! policy reads. So releasing a guard needs no access to the runtime. The
//! runtime's entries hold the state too, so a guard that
//! [`keep`](crate::Listener::keep) gave up leaves the count raised, and the
//! state is freed with the entries.
//!
//! Where the target has pointer atomics the state is an `Arc` of atomics,
//! so a guard can be dropped on any thread, even a `Local` runtime's. On a
//! Cortex-M0 it is an `Rc` of cells, and a guard stays on its runtime's
//! thread, like the runtime.
//!
//! Every load and update is relaxed. Nothing passes through the count from
//! the thread that drops a guard to the thread that runs the runtime: a
//! stale read lets a listener run once more, which a drop on another thread
//! can cause anyway. A relaxed load is the same instruction as a cell's.

#[cfg(target_has_atomic = "ptr")]
mod count {
    pub(super) use alloc::sync::Arc as Ptr;
    use core::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    pub(super) struct Count(AtomicUsize);

    impl Count {
        #[inline]
        pub(super) const fn new(n: usize) -> Self {
            Count(AtomicUsize::new(n))
        }
        #[inline]
        pub(super) fn get(&self) -> usize {
            self.0.load(Relaxed)
        }
        /// Adds one, wrapping.
        #[inline]
        pub(super) fn increment(&self) {
            self.0.fetch_add(1, Relaxed);
        }
        /// Takes one away, and returns whether that was the last.
        #[inline]
        pub(super) fn decrement(&self) -> bool {
            self.0.fetch_sub(1, Relaxed) == 1
        }
    }
}

#[cfg(not(target_has_atomic = "ptr"))]
mod count {
    pub(super) use alloc::rc::Rc as Ptr;
    use core::cell::Cell;

    pub(super) struct Count(Cell<usize>);

    impl Count {
        #[inline]
        pub(super) const fn new(n: usize) -> Self {
            Count(Cell::new(n))
        }
        #[inline]
        pub(super) fn get(&self) -> usize {
            self.0.get()
        }
        /// Adds one, wrapping.
        #[inline]
        pub(super) fn increment(&self) {
            self.0.set(self.0.get().wrapping_add(1));
        }
        /// Takes one away, and returns whether that was the last.
        #[inline]
        pub(super) fn decrement(&self) -> bool {
            let n = self.0.get() - 1;
            self.0.set(n);
            n == 0
        }
    }
}

use count::{Count, Ptr};

/// A runtime's count of released guards, which every guard's state shares.
/// It only grows, and wraps.
#[derive(Clone)]
pub(crate) struct Released(Ptr<Count>);

impl Released {
    #[inline]
    pub(crate) fn new() -> Self {
        Released(Ptr::new(Count::new(0)))
    }

    /// The guards released so far.
    #[inline]
    pub(crate) fn count(&self) -> usize {
        self.0.get()
    }
}

/// A guard's state: the count of its owners, and its runtime's count of
/// released guards.
struct State {
    owners: Count,
    released: Released,
}

/// A share of a guard's state. A guard holds one as its owner's share, and
/// each of the runtime's entries for the guard holds one.
#[derive(Clone)]
pub(crate) struct Liveness(Ptr<State>);

impl Liveness {
    /// The state of a new guard, with its one owner.
    #[inline]
    pub(crate) fn new(released: &Released) -> Self {
        Liveness(Ptr::new(State {
            owners: Count::new(1),
            released: released.clone(),
        }))
    }

    /// Another owner's share of the same state. Only an owner makes one, so
    /// the count is above zero when it does.
    #[inline]
    pub(crate) fn add_owner(&self) -> Self {
        self.0.owners.increment();
        Liveness(self.0.clone())
    }

    /// Whether the guard still has an owner.
    #[inline]
    pub(crate) fn is_live(&self) -> bool {
        self.0.owners.get() > 0
    }

    /// Gives up an owner's share. The last one counts a released guard.
    #[inline]
    pub(crate) fn release(self) {
        if self.0.owners.decrement() {
            self.0.released.0.increment();
        }
    }
}
