//! The I/O edge's engine side (RFD 6, RFD 7): the lock the edge's shared
//! state lives behind, the input slots connected to the graph, the inbox of
//! remote calls, and the waker the driver registered.
//!
//! A slot and the inbox are shared between the code that writes them, an
//! interrupt handler or another thread, and the driver, so their state
//! needs a lock. With no `unsafe` in the crate there is no lock to build
//! from atomics: `core` and `alloc` have no interior mutability that is
//! `Sync`, and a spin lock would need an `UnsafeCell` and would deadlock
//! against an interrupt that preempts the driver while it holds it. So the
//! lock is the standard mutex under `std`, a critical section with the
//! `critical-section` feature, and nothing otherwise: a `no_std` build
//! without that feature has no input slots and no `RemoteIo`.

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
use core::task::Waker;

use super::DoubleSend;
use super::TokenFault;
use crate::build::Build;
use crate::guard::{Released, Stamps};
use crate::io::IoQueue;
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
use crate::io::{Registration, Waiting};
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
    /// an `&mut Option<A>`, after the lock is released. Returns whether
    /// there was one.
    fn drain(&self, fire: &mut dyn FnMut(&mut dyn Any)) -> bool;
    /// Replaces the waker a write wakes.
    fn set_waker(&self, waker: Option<Waker>);
    /// Forgets the graph and the waker, and drops a pending event.
    fn disconnect(&self);
}

/// A slot connected to an input.
#[cfg(any(feature = "std", feature = "critical-section"))]
#[derive(Clone, Copy)]
pub(crate) struct Connection {
    pub(crate) input: Token,
    pub(crate) slot: &'static dyn Drain,
    /// Higher drains first.
    pub(crate) priority: u8,
    /// The serial of the last pump that drained it, so that it drains
    /// once per pump.
    pub(crate) drained: u64,
}

/// The graph's side of the edge. It lives in the build context, since
/// [`connect`](crate::Build::connect) takes one.
pub(crate) struct Edge {
    /// The waker the driver registered; a slot connected later gets it too.
    pub(crate) waker: Option<Waker>,
    /// The connected slots in the order the pump drains them: higher
    /// priority first, and connection order among equals.
    #[cfg(any(feature = "std", feature = "critical-section"))]
    pub(crate) slots: Vec<Connection>,
    /// The serial of the pump running now, or of the last one.
    #[cfg(any(feature = "std", feature = "critical-section"))]
    pub(crate) pumps: u64,
    /// The queue every `RemoteIo` of this graph shares. Made with the graph,
    /// so `Runtime::remote_io` takes `&self`.
    #[cfg(all(
        target_has_atomic = "ptr",
        any(feature = "std", feature = "critical-section")
    ))]
    pub(crate) inbox: Arc<Inbox>,
}

impl Edge {
    /// The remote's half of [`Build::arm`]: a remote send from this
    /// thread is refused until [`disarm`](Edge::disarm) (RFD 6).
    #[inline]
    pub(crate) fn arm(&self) {
        #[cfg(all(feature = "std", target_has_atomic = "ptr"))]
        self.inbox.running.store(thread_token(), Ordering::Relaxed);
    }

    /// The remote's half of [`Build::disarm`].
    #[inline]
    pub(crate) fn disarm(&self) {
        #[cfg(all(feature = "std", target_has_atomic = "ptr"))]
        self.inbox.running.store(0, Ordering::Relaxed);
    }

    pub(crate) fn new(graph: u32, released: &Released, stamps: &Stamps) -> Self {
        let _ = (graph, released, stamps);
        Edge {
            waker: None,
            #[cfg(any(feature = "std", feature = "critical-section"))]
            slots: Vec::new(),
            #[cfg(any(feature = "std", feature = "critical-section"))]
            pumps: 0,
            #[cfg(all(
                target_has_atomic = "ptr",
                any(feature = "std", feature = "critical-section")
            ))]
            inbox: Arc::new(Inbox::new(graph, released, stamps)),
        }
    }
}

/// A graph that goes away, dropped or unwound out of a panicking build,
/// disconnects its slots, so that each can be connected again and no event
/// written for this graph reaches another, and closes its inbox, so that
/// no `RemoteIo` keeps filling a queue that no pump will drain.
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

/// One remote send, or one remote transaction's closure, run by the driver
/// as one transaction. A remote send is a closure of one send, so the
/// driver has one path for both.
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
pub(crate) type Unit = Box<dyn FnOnce(&mut IoTransaction<'_>) + Send>;

/// What a `RemoteIo` queues: a unit, or a registration of a listener or an
/// anchor.
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
pub(crate) enum RemoteCall {
    Unit(Unit),
    Register(Box<dyn Registration>),
}

/// The queue of calls every `RemoteIo` of one graph shares (RFD 6).
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
pub(crate) struct Inbox {
    /// The graph's id, for a remote call's foreign-token check.
    pub(crate) graph: u32,
    /// The graph's poison, mirrored by the first entry that finds it, so
    /// that remote calls fail from then on.
    poisoned: AtomicBool,
    /// The graph was dropped, so nothing will drain the queue. Set under
    /// the lock, and read there by a push; a call reads it first without
    /// the lock, to refuse early.
    closed: AtomicBool,
    /// The guard: the token of the thread running this graph's code, or 0.
    /// Only that thread can find its own token here, so a relaxed load
    /// suffices: it reads its own store.
    #[cfg(feature = "std")]
    running: AtomicUsize,
    /// The graph's count of released guards, which a guard a `RemoteIo`
    /// makes shares.
    pub(crate) released: Released,
    /// What a call takes its stamp from, shared with the `Io`s' queue.
    stamps: Stamps,
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
    /// Calls in arrival order, which is stamp order: a call takes its stamp
    /// under the lock.
    calls: VecDeque<Waiting<RemoteCall>>,
    /// What a push wakes.
    waker: Option<Waker>,
    /// A call has woken the driver since the last pump began.
    woken: bool,
}

#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
impl Inbox {
    fn new(graph: u32, released: &Released, stamps: &Stamps) -> Self {
        Inbox {
            graph,
            poisoned: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            #[cfg(feature = "std")]
            running: AtomicUsize::new(0),
            released: released.clone(),
            stamps: stamps.clone(),
            state: Lock::new(Queue {
                calls: VecDeque::new(),
                waker: None,
                woken: false,
            }),
        }
    }

    /// Stamps a call and queues it, and wakes the driver after the lock is
    /// released, unless a call has since the last pump began. Gives the call
    /// back if the graph was dropped, to be dropped outside the lock, since
    /// its captures' `Drop` is user code.
    pub(crate) fn push(&self, mut waiting: Waiting<RemoteCall>) -> Result<(), Waiting<RemoteCall>> {
        let waker = self.state.with(|q| {
            if self.closed.load(Ordering::Relaxed) {
                return Err(waiting);
            }
            waiting.stamp = self.stamps.take();
            q.calls.push_back(waiting);
            if q.woken || q.waker.is_none() {
                return Ok(None);
            }
            q.woken = true;
            Ok(q.waker.clone())
        })?;
        if let Some(waker) = waker {
            waker.wake();
        }
        Ok(())
    }

    /// A pump begins: the next call wakes the driver again, and the pump
    /// runs the calls stamped before the stamp `stamps` would give now.
    /// Both happen under the lock a call stamps and wakes under, so a call
    /// either is stamped in time to run or finds the wake still to make.
    pub(crate) fn begin_pump(&self, stamps: &Stamps) -> usize {
        self.state.with(|q| {
            q.woken = false;
            stamps.next()
        })
    }

    /// The stamp of the oldest call.
    pub(crate) fn front(&self) -> Option<usize> {
        self.state
            .with(|q| q.calls.front().map(|waiting| waiting.stamp))
    }

    /// The oldest call. The lock is released before it runs, so a
    /// transaction never runs under it.
    pub(crate) fn pop(&self) -> Option<RemoteCall> {
        self.state
            .with(|q| q.calls.pop_front())
            .map(|waiting| waiting.call)
    }

    /// Adds to `roots` the tokens the waiting registrations name, but none
    /// of one whose guard has gone.
    pub(crate) fn roots(&self, roots: &mut Vec<Token>) {
        self.state.with(|q| {
            for waiting in &q.calls {
                waiting.roots(roots);
            }
        });
    }

    /// Replaces the waker a push wakes. The next call wakes the new one.
    pub(crate) fn set_waker(&self, waker: Waker) {
        let old = self.state.with(|q| {
            q.woken = false;
            q.waker.replace(waker)
        });
        drop(old);
    }

    /// The graph is gone: refuses every later push, and drops what is
    /// queued and the waker outside the lock.
    fn close(&self) {
        #[cfg(feature = "std")]
        self.running.store(0, Ordering::Relaxed);
        let (calls, waker) = self.state.with(|q| {
            self.closed.store(true, Ordering::Relaxed);
            (core::mem::take(&mut q.calls), q.waker.take())
        });
        drop((calls, waker));
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

    /// Whether the graph was dropped. A push checks again under the lock.
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
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

impl<M: Mode> Build<M> {
    /// Graph code starts running: evaluation, commit, a construct closure,
    /// a split's iterator. A remote send from this thread, and a call
    /// through an `Io`, are refused until [`disarm`](Build::disarm)
    /// (RFD 6).
    #[inline]
    pub(crate) fn arm(&self) {
        self.edge.arm();
        self.io.graph_code(true);
    }

    /// Graph code has stopped: before listeners run, and when a
    /// transaction ends.
    #[inline]
    pub(crate) fn disarm(&self) {
        self.edge.disarm();
        self.io.graph_code(false);
    }

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
