//! The I/O edge's engine side (RFD 6, RFD 7): the lock the edge's shared
//! state lives behind, the input slots connected to the graph, the inbox of
//! remote units, and the waker the driver registered.
//!
//! A slot and the inbox are shared between the code that writes them, an
//! interrupt handler or another thread, and the driver, so their state
//! needs a lock. With no `unsafe` in the crate there is no lock to build
//! from atomics: `core` and `alloc` have no interior mutability that is
//! `Sync`, and a spin lock would need an `UnsafeCell` and would deadlock
//! against an interrupt that preempts the driver while it holds it. So the
//! lock is the standard mutex under `std`, a critical section with the
//! `critical-section` feature, and nothing otherwise: a `no_std` build
//! without that feature has no input slots and no `Remote`.

#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
use alloc::boxed::Box;
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
use alloc::collections::VecDeque;
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
use alloc::sync::Arc;
#[cfg(any(feature = "std", feature = "critical-section"))]
use alloc::vec::Vec;
use core::any::Any;
#[cfg(all(feature = "std", target_has_atomic = "ptr"))]
use core::sync::atomic::AtomicUsize;
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
use core::sync::atomic::{AtomicBool, Ordering};
#[cfg(any(feature = "std", feature = "critical-section"))]
use core::task::Waker;

use super::DoubleSend;
use super::TokenFault;
use crate::build::Build;
use crate::mode::Mode;
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
use crate::runtime::IoTransaction;
use crate::token::Token;

/// The lock under the edge's shared state: the standard mutex.
///
/// Only a slot's fold and the clone or drop of a waker run under it, never
/// a transaction, so a panic there leaves the state consistent and a
/// poisoned mutex is used as it is.
#[cfg(feature = "std")]
pub(crate) struct Lock<T>(std::sync::Mutex<T>);

#[cfg(feature = "std")]
impl<T> Lock<T> {
    pub(crate) const fn new(value: T) -> Self {
        Lock(std::sync::Mutex::new(value))
    }

    /// Runs `f` with the state locked.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut state)
    }
}

/// The lock under the edge's shared state on bare metal: a critical
/// section, which the target's HAL provides through the
/// `critical-section` crate.
#[cfg(all(not(feature = "std"), feature = "critical-section"))]
pub(crate) struct Lock<T>(critical_section::Mutex<core::cell::RefCell<T>>);

#[cfg(all(not(feature = "std"), feature = "critical-section"))]
impl<T> Lock<T> {
    pub(crate) const fn new(value: T) -> Self {
        Lock(critical_section::Mutex::new(core::cell::RefCell::new(
            value,
        )))
    }

    /// Runs `f` inside a critical section.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        critical_section::with(|cs| f(&mut self.0.borrow_ref_mut(cs)))
    }
}

/// What the graph needs from an input slot whose event type it does not
/// know. `Sync`, so that a graph holding `&'static dyn Drain`s is `Send`
/// where its mode says it is.
#[cfg(any(feature = "std", feature = "critical-section"))]
pub(crate) trait Drain: Sync {
    /// Connects the slot to graph `graph`, with the graph's waker if it has
    /// one. False if the slot is connected already.
    fn connect(&self, graph: u32, waker: Option<&Waker>) -> bool;
    /// Takes the pending event, if there is one, and hands it to `fire` as
    /// an `&mut Option<A>`, after the lock is released.
    fn drain(&self, fire: &mut dyn FnMut(&mut dyn Any));
    /// Replaces the waker a write wakes.
    fn set_waker(&self, waker: Option<Waker>);
    /// Forgets the graph and the waker, and drops a pending event.
    fn disconnect(&self);
}

/// A slot connected to an input, in connection order.
#[cfg(any(feature = "std", feature = "critical-section"))]
#[derive(Clone, Copy)]
pub(crate) struct Connection {
    pub(crate) input: Token,
    pub(crate) slot: &'static dyn Drain,
}

/// The graph's side of the edge. It lives in the build context, since
/// [`connect`](crate::Build::connect) takes one.
pub(crate) struct Edge {
    /// The waker the driver registered; a slot connected later gets it too.
    #[cfg(any(feature = "std", feature = "critical-section"))]
    pub(crate) waker: Option<Waker>,
    /// The connected slots, in connection order.
    #[cfg(any(feature = "std", feature = "critical-section"))]
    pub(crate) slots: Vec<Connection>,
    /// The queue every `Remote` of this graph shares. Made with the graph,
    /// so `Runtime::remote` takes `&self`.
    #[cfg(all(
        target_has_atomic = "ptr",
        any(feature = "std", feature = "critical-section")
    ))]
    pub(crate) inbox: Arc<Inbox>,
}

impl Edge {
    /// Graph code starts running on this thread: evaluation, commit, a
    /// construct closure, a split's iterator. A remote send from this
    /// thread is refused until [`disarm`](Edge::disarm) (RFD 6).
    #[inline]
    pub(crate) fn arm(&self) {
        #[cfg(all(feature = "std", target_has_atomic = "ptr"))]
        self.inbox.running.store(thread_token(), Ordering::Relaxed);
    }

    /// Graph code has stopped: before listeners run, and when a
    /// transaction ends.
    #[inline]
    pub(crate) fn disarm(&self) {
        #[cfg(all(feature = "std", target_has_atomic = "ptr"))]
        self.inbox.running.store(0, Ordering::Relaxed);
    }

    pub(crate) fn new(graph: u32) -> Self {
        let _ = graph;
        Edge {
            #[cfg(any(feature = "std", feature = "critical-section"))]
            waker: None,
            #[cfg(any(feature = "std", feature = "critical-section"))]
            slots: Vec::new(),
            #[cfg(all(
                target_has_atomic = "ptr",
                any(feature = "std", feature = "critical-section")
            ))]
            inbox: Arc::new(Inbox::new(graph)),
        }
    }
}

/// A graph that goes away, dropped or unwound out of a panicking build,
/// disconnects its slots, so that each can be connected again and no event
/// written for this graph reaches another, and closes its inbox, so that
/// no remote keeps filling a queue that no pump will drain.
impl Drop for Edge {
    fn drop(&mut self) {
        #[cfg(any(feature = "std", feature = "critical-section"))]
        for connection in self.slots.drain(..) {
            connection.slot.disconnect();
        }
        #[cfg(all(
            target_has_atomic = "ptr",
            any(feature = "std", feature = "critical-section")
        ))]
        self.inbox.close();
    }
}

/// What the inbox queues: one remote send, or one remote transaction's
/// closure, each run by the driver as one transaction. A remote send is a
/// closure of one send, so the driver has one path for both.
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
pub(crate) type Unit = Box<dyn FnOnce(&mut IoTransaction<'_>) + Send>;

/// The queue of units every `Remote` of one graph shares (RFD 6).
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
pub(crate) struct Inbox {
    /// The graph's id, for a remote send's foreign-token check.
    pub(crate) graph: u32,
    /// The graph's poison, mirrored by the first entry that finds it, so
    /// that remote sends fail from then on.
    poisoned: AtomicBool,
    /// The guard: the token of the thread running this graph's code, or 0.
    /// Only that thread can find its own token here, so a relaxed load
    /// suffices: it reads its own store.
    #[cfg(feature = "std")]
    running: AtomicUsize,
    state: Lock<Queue>,
}

/// A number no other live thread has: the address of a thread-local byte.
/// Never 0. A thread being torn down has none, and runs no graph code.
#[cfg(all(feature = "std", target_has_atomic = "ptr"))]
fn thread_token() -> usize {
    std::thread_local! {
        static TOKEN: u8 = const { 0 };
    }
    TOKEN
        .try_with(|token| core::ptr::from_ref(token).addr())
        .unwrap_or(0)
}

#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
struct Queue {
    /// Units in arrival order: the total order the semantics need.
    units: VecDeque<Unit>,
    /// What a push wakes.
    waker: Option<Waker>,
    /// The graph was dropped: nothing will drain the queue.
    closed: bool,
}

#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
impl Inbox {
    fn new(graph: u32) -> Self {
        Inbox {
            graph,
            poisoned: AtomicBool::new(false),
            #[cfg(feature = "std")]
            running: AtomicUsize::new(0),
            state: Lock::new(Queue {
                units: VecDeque::new(),
                waker: None,
                closed: false,
            }),
        }
    }

    /// Queues a unit and wakes the driver, after the lock is released.
    /// Gives the unit back if the graph was dropped, to be dropped outside
    /// the lock, since its captures' `Drop` is user code.
    pub(crate) fn push(&self, unit: Unit) -> Result<(), Unit> {
        let waker = self.state.with(|q| {
            if q.closed {
                return Err(unit);
            }
            q.units.push_back(unit);
            Ok(q.waker.clone())
        })?;
        if let Some(waker) = waker {
            waker.wake();
        }
        Ok(())
    }

    /// The oldest unit. The lock is released before it runs, so a
    /// transaction never runs under it.
    pub(crate) fn pop(&self) -> Option<Unit> {
        self.state.with(|q| q.units.pop_front())
    }

    /// The units queued now.
    pub(crate) fn len(&self) -> usize {
        self.state.with(|q| q.units.len())
    }

    /// Replaces the waker a push wakes.
    pub(crate) fn set_waker(&self, waker: Waker) {
        let old = self.state.with(|q| q.waker.replace(waker));
        drop(old);
    }

    /// The graph is gone: refuses every later push, and drops what is
    /// queued and the waker outside the lock.
    fn close(&self) {
        #[cfg(feature = "std")]
        self.running.store(0, Ordering::Relaxed);
        let (units, waker) = self.state.with(|q| {
            q.closed = true;
            (core::mem::take(&mut q.units), q.waker.take())
        });
        drop((units, waker));
    }

    /// Whether the calling thread is running this graph's code: a remote
    /// send from there is I/O inside a transaction. Unchecked without
    /// `std`, which has no thread id (RFD 7).
    pub(crate) fn inside(&self) -> bool {
        #[cfg(feature = "std")]
        {
            let token = thread_token();
            token != 0 && self.running.load(Ordering::Relaxed) == token
        }
        #[cfg(not(feature = "std"))]
        false
    }

    /// Mirrors the graph's poison.
    pub(crate) fn poison(&self) {
        self.poisoned.store(true, Ordering::Release);
    }

    pub(crate) fn is_poisoned(&self) -> bool {
        self.poisoned.load(Ordering::Acquire)
    }
}

/// Why a send inside a unit failed, found by the driver at `pump`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    Stale,
    ForeignGraph,
    DoubleSend,
}

/// The build context as a unit's sends see it: with no mode, since an
/// `IoTransaction` has none, and with the event's type behind `dyn Any`.
pub(crate) trait Start {
    /// Starts `input` with the event in `event`, an `&mut Option<A>`, in
    /// the transaction the driver opened for the unit.
    fn start(&mut self, input: Token, event: &mut dyn Any) -> Result<(), Fault>;
}

impl<M: Mode> Start for Build<M> {
    fn start(&mut self, input: Token, event: &mut dyn Any) -> Result<(), Fault> {
        let i = self.lookup(input).map_err(|fault| match fault {
            TokenFault::Foreign => Fault::ForeignGraph,
            TokenFault::Stale => Fault::Stale,
        })?;
        let fire = self.store.ops[i as usize].fire;
        fire(self, i, event).map_err(|DoubleSend| Fault::DoubleSend)
    }
}

#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
impl<M: Mode> Build<M> {
    /// Drops a unit that failed after the driver opened its transaction:
    /// empties the slots its sends filled and closes the transaction
    /// without running it. Nothing ran, so there is nothing to undo, and
    /// the graph is not poisoned; the serial goes unused.
    pub(crate) fn cancel(&mut self) {
        let Build { store, s, .. } = self;
        for &n in &s.starts {
            (store.ops[n as usize].clear_slot)(&mut store.data[n as usize]);
        }
        s.starts.clear();
        self.in_tx = false;
    }
}

/// `ops.fire` of an input whose events are `A`s: takes the event out of
/// `event`, an `Option<A>`, and starts the input with it. How a value whose
/// type the caller does not know, a slot's or a remote unit's, reaches the
/// typed [`fire_start`](Build::fire_start).
pub(crate) fn fire_input<M: Mode, A: 'static>(
    b: &mut Build<M>,
    input: u32,
    event: &mut dyn Any,
) -> Result<(), DoubleSend> {
    let event = event
        .downcast_mut::<Option<A>>()
        .expect("bough engine: send type")
        .take()
        .expect("bough engine: a send carries a value");
    b.fire_start(input, event)
}
