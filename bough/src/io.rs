//! The same-thread handle (RFD 6, RFD 7): an [`Io`] queues its calls for
//! the driver's next pump.
//!
//! A GTK signal handler, a DOM closure or a listener is `'static`, and
//! can't hold `&mut Runtime`. So it holds an `Io`, which reaches the
//! runtime only through a queue the two share. The pump runs the queue
//! after the input slots and the remote units, taking only the calls made
//! before it began, in the order they were made.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::{Rc, Weak};
use core::cell::{Cell, RefCell};
use core::task::Waker;

use crate::error::IoError;
#[cfg(target_has_atomic = "ptr")]
use crate::mode::Threaded;
use crate::mode::{Local, Mode};
use crate::runtime::{IoTransaction, Runtime, Stop};
use crate::token::Input;

/// A call a handle queued. The pump runs it with the runtime, and says
/// whether a stale token is skipped, as in the panicking pump's release
/// build, or stops the pump.
pub(crate) type Call<M> = Box<dyn FnOnce(&mut Runtime<M>, bool) -> Result<(), Stop>>;

/// What a runtime keeps for its same-thread handle: the queue its [`Io`]s
/// share in `Local`, and nothing in `Threaded`, which has no `Io`.
#[doc(hidden)]
pub trait IoQueue<M: Mode>: 'static {
    /// An empty queue.
    fn new() -> Self;
    /// Graph code starts or stops running. A call is refused while it
    /// runs.
    fn graph_code(&self, running: bool);
    /// Mirrors the runtime's poison.
    fn poison(&self);
    /// Replaces the waker a call wakes.
    fn set_waker(&self, waker: &Waker);
    /// A pump begins: the number of calls waiting, which it runs. The next
    /// call wakes the driver again.
    fn begin_pump(&self) -> usize;
    /// The oldest call.
    fn pop(&self) -> Option<Call<M>>;
}

/// What a `Local` runtime shares with its [`Io`]s.
#[doc(hidden)]
pub struct IoState {
    /// The calls waiting for the next pump, in the order they were made.
    calls: RefCell<VecDeque<Call<Local>>>,
    /// Set while graph code runs, from `arm` to `disarm`.
    graph_code: Cell<bool>,
    /// The runtime's poison, mirrored by the first entry that finds it.
    poisoned: Cell<bool>,
    /// The driver's waker.
    waker: Cell<Option<Waker>>,
    /// A call has woken the driver since the last pump began.
    woken: Cell<bool>,
}

impl IoState {
    /// Wakes the driver, unless a call has since the last pump began: one
    /// wake is enough for every call it will run. The waker is cloned out
    /// of its cell, so nothing is borrowed while its code runs.
    fn wake(&self) {
        if self.woken.get() {
            return;
        }
        let waker = self.waker.take();
        let clone = waker.clone();
        self.waker.set(waker);
        if let Some(waker) = clone {
            self.woken.set(true);
            waker.wake();
        }
    }
}

impl IoQueue<Local> for Rc<IoState> {
    fn new() -> Self {
        Rc::new(IoState {
            calls: RefCell::new(VecDeque::new()),
            graph_code: Cell::new(false),
            poisoned: Cell::new(false),
            waker: Cell::new(None),
            woken: Cell::new(false),
        })
    }

    #[inline]
    fn graph_code(&self, running: bool) {
        self.graph_code.set(running);
    }

    fn poison(&self) {
        self.poisoned.set(true);
    }

    fn set_waker(&self, waker: &Waker) {
        drop(self.waker.replace(Some(waker.clone())));
        self.woken.set(false);
    }

    #[inline]
    fn begin_pump(&self) -> usize {
        self.woken.set(false);
        self.calls.borrow().len()
    }

    fn pop(&self) -> Option<Call<Local>> {
        self.calls.borrow_mut().pop_front()
    }
}

/// A `Threaded` runtime has no [`Io`], so nothing waits.
#[cfg(target_has_atomic = "ptr")]
#[doc(hidden)]
pub struct NoIo;

#[cfg(target_has_atomic = "ptr")]
impl IoQueue<Threaded> for NoIo {
    fn new() -> Self {
        NoIo
    }
    #[inline]
    fn graph_code(&self, _: bool) {}
    fn poison(&self) {}
    fn set_waker(&self, _: &Waker) {}
    #[inline]
    fn begin_pump(&self) -> usize {
        0
    }
    fn pop(&self) -> Option<Call<Threaded>> {
        None
    }
}

/// A handle for I/O code that can't hold the runtime: a GTK signal
/// handler, a DOM closure, a listener. `Clone`, not `Send`, and on every
/// target. From [`Runtime::io`].
///
/// Every call queues for the driver's next [`pump`](Runtime::pump) and
/// returns at once, and nothing reads through it: only the `Runtime`
/// reads. The pump runs the calls after the input slots and the remote
/// units, in the order they were made, taking only those made before it
/// began. So a call a listener makes during a pump waits for the next
/// one, and a listener that always sends can't keep a pump from
/// returning.
///
/// ```
/// use bough::{Runtime, Source};
///
/// let (mut graph, edge) = Runtime::build(|b| {
///     let (clicks, clicks_in) = b.input::<()>();
///     (clicks_in, clicks.accumulate(b, 0u32, |_, n| n + 1))
/// });
/// let (clicks_in, count) = edge.keep();
/// let io = graph.io();
/// let on_click = move || io.send(clicks_in, ()).unwrap(); // what a handler holds
/// on_click();
/// on_click();
/// assert_eq!(*graph.sample(count), 0); // both wait
/// graph.pump(); // the driver
/// assert_eq!(*graph.sample(count), 2);
/// ```
///
/// A call wakes the waker the driver registered with
/// [`set_waker`](Runtime::set_waker), unless another call has since the
/// last pump began: one wake is enough for every call that pump will run.
///
/// An `Io` is `Clone + 'static`, so graph code can capture one: a `map`
/// function, a `construct` closure, a split's iterator. A call from there
/// would be I/O inside FRP logic, so it's refused with
/// [`IoError::FromGraphCode`]. A panic that escapes graph code leaves that
/// check's flag set, so a call reports `FromGraphCode` until an entry on
/// the runtime finds the poison, and `Poisoned` after that. A call after
/// the runtime drops reports `Gone`.
#[derive(Clone)]
pub struct Io(Weak<IoState>);

impl Io {
    /// A handle to `state`'s runtime.
    pub(crate) fn new(state: &Rc<IoState>) -> Io {
        Io(Rc::downgrade(state))
    }

    /// Queues one value as a unit of its own, which the next pump runs as
    /// one transaction.
    ///
    /// Whether the input is collected is graph knowledge, so it's found at
    /// the pump, as for a remote unit: a panic in a debug build, and in a
    /// release build a no-op that
    /// [`stale_operations`](Runtime::stale_operations) counts. A token from
    /// another graph is found there too, and panics in both builds.
    /// [`try_pump`](Runtime::try_pump) returns either.
    pub fn send<A: 'static>(&self, input: Input<A>, value: A) -> Result<(), IoError> {
        self.transaction(move |tx| tx.send(input, value))
    }

    /// Queues several sends as one unit, and so one transaction: `f` runs
    /// on the driver at its next pump, with an [`IoTransaction`] whose sends
    /// are simultaneous. The closure is I/O code: it has no graph access,
    /// and its order of sends doesn't matter.
    ///
    /// A unit whose send fails is dropped whole at the pump, with none of
    /// its sends run, as a remote's is. A panic in the closure poisons the
    /// runtime.
    pub fn transaction<F>(&self, f: F) -> Result<(), IoError>
    where
        F: FnOnce(&mut IoTransaction<'_>) + 'static,
    {
        self.queue(Box::new(move |runtime, skip_stale| {
            runtime.run_unit(skip_stale, f)
        }))
    }

    /// Queues `call`, and wakes the driver.
    fn queue(&self, call: Call<Local>) -> Result<(), IoError> {
        let state = self.0.upgrade().ok_or(IoError::Gone)?;
        if state.poisoned.get() {
            return Err(IoError::Poisoned);
        }
        if state.graph_code.get() {
            return Err(IoError::FromGraphCode);
        }
        state.calls.borrow_mut().push_back(call);
        state.wake();
        Ok(())
    }
}
