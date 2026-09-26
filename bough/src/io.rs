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
use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::task::Waker;

use crate::cell::CellRef;
use crate::error::IoError;
use crate::guard::{Liveness, Released};
#[cfg(target_has_atomic = "ptr")]
use crate::mode::Threaded;
use crate::mode::{Local, Mode};
use crate::runtime::{Anchor, Anchored, IoTransaction, Listener, Runtime, Stop};
use crate::source::Node;
use crate::token::{Input, Token};
use crate::trace::{Trace, Tracer};

/// A call a handle queued. The pump runs it with the runtime, and says
/// whether a stale token is skipped, as in the panicking pump's release
/// build, or stops the pump.
pub(crate) type Call<M> = Box<dyn FnOnce(&mut Runtime<M>, bool) -> Result<(), Stop>>;

/// A call waiting in the queue, and the tokens it roots until it runs.
struct Waiting {
    call: Call<Local>,
    /// What collection keeps alive while the call waits: a send's input,
    /// a registration's node, the tokens an anchor's value holds. A
    /// transaction's closure hides its tokens, so it names none.
    roots: Vec<Token>,
    /// A registration's guard. Once the guard has gone, the call roots
    /// nothing, and the pump skips it.
    guard: Option<Liveness>,
}

/// What a runtime keeps for its same-thread handle: the queue its [`Io`]s
/// share in `Local`, and nothing in `Threaded`, which has no `Io`.
#[doc(hidden)]
pub trait IoQueue<M: Mode>: 'static {
    /// An empty queue for graph `graph`, whose guards count their releases
    /// on `released`.
    fn new(graph: u32, released: &Released) -> Self;
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
    /// Adds to `roots` the tokens the waiting calls name, but none of a
    /// registration whose guard has gone.
    fn roots(&self, roots: &mut Vec<Token>);
}

/// What a `Local` runtime shares with its [`Io`]s.
#[doc(hidden)]
pub struct IoState {
    /// The graph's id, for a call's foreign-token check.
    graph: u32,
    /// The calls waiting for the next pump, in the order they were made.
    calls: RefCell<VecDeque<Waiting>>,
    /// Set while graph code runs, from `arm` to `disarm`.
    graph_code: Cell<bool>,
    /// The runtime's poison, mirrored by the first entry that finds it.
    poisoned: Cell<bool>,
    /// The driver's waker.
    waker: Cell<Option<Waker>>,
    /// A call has woken the driver since the last pump began.
    woken: Cell<bool>,
    /// The runtime's count of released guards, which a guard made here
    /// shares.
    released: Released,
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
    fn new(graph: u32, released: &Released) -> Self {
        Rc::new(IoState {
            graph,
            calls: RefCell::new(VecDeque::new()),
            graph_code: Cell::new(false),
            poisoned: Cell::new(false),
            waker: Cell::new(None),
            woken: Cell::new(false),
            released: released.clone(),
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
        self.calls
            .borrow_mut()
            .pop_front()
            .map(|waiting| waiting.call)
    }

    fn roots(&self, roots: &mut Vec<Token>) {
        for waiting in self.calls.borrow().iter() {
            if waiting.guard.as_ref().is_none_or(Liveness::is_live) {
                roots.extend_from_slice(&waiting.roots);
            }
        }
    }
}

/// A `Threaded` runtime has no [`Io`], so nothing waits.
#[cfg(target_has_atomic = "ptr")]
#[doc(hidden)]
pub struct NoIo;

#[cfg(target_has_atomic = "ptr")]
impl IoQueue<Threaded> for NoIo {
    fn new(_: u32, _: &Released) -> Self {
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
    fn roots(&self, _: &mut Vec<Token>) {}
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
/// returning. A [`Listener`] or an [`Anchored`] comes back at once, and
/// its registration waits like any call; dropping it first cancels the
/// registration.
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
/// A waiting call keeps the tokens it names alive until the pump runs it,
/// through any collection before then: a send's input, but not the value
/// it carries; a registration's node; the tokens an anchor's value holds.
/// So a listener handed a plain row can listen to it or anchor it, and the
/// row lasts until the pump registers what keeps it. A transaction's
/// closure hides its tokens, so a waiting transaction keeps none, and a
/// registration whose guard has gone keeps none either.
///
/// A call is refused, with an [`IoError`], if the runtime has dropped, is
/// poisoned or is running graph code, or if a token the call names is
/// another graph's, checked in that order. An `Io` is `Clone + 'static`,
/// so graph code can capture one: a `map` function, a `construct`
/// closure, a split's iterator. A call from there would be I/O inside FRP
/// logic, so it's refused with [`IoError::FromGraphCode`]. A panic that
/// escapes graph code leaves that check's flag set, so a call reports
/// `FromGraphCode` until an entry on the runtime finds the poison, and
/// `Poisoned` after that.
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
    /// [`stale_operations`](Runtime::stale_operations) counts;
    /// [`try_pump`](Runtime::try_pump) returns it. A token from another
    /// graph is refused now, with [`IoError::ForeignGraph`].
    pub fn send<A: 'static>(&self, input: Input<A>, value: A) -> Result<(), IoError> {
        self.unit(vec![input.token], move |tx| tx.send(input, value))
    }

    /// Queues several sends as one unit, and so one transaction: `f` runs
    /// on the driver at its next pump, with an [`IoTransaction`] whose sends
    /// are simultaneous. The closure is I/O code: it has no graph access,
    /// and its order of sends doesn't matter.
    ///
    /// A unit whose send fails is dropped whole at the pump, with none of
    /// its sends run, as a remote's is; that's where a token from another
    /// graph is found, too. A panic in the closure poisons the runtime.
    /// While it waits it keeps no token alive, since its closure hides
    /// them: anchor what it sends to.
    pub fn transaction<F>(&self, f: F) -> Result<(), IoError>
    where
        F: FnOnce(&mut IoTransaction<'_>) + 'static,
    {
        self.unit(Vec::new(), f)
    }

    /// Listens to a materialized node, as [`Runtime::listen`] does, from the
    /// next pump. The [`Listener`] comes back now, and dropping it before
    /// that pump cancels the registration.
    ///
    /// A listener misses what ran before its registration: the
    /// transactions before the pump, and those the pump ran before it. A
    /// stale token is found at the pump, and a foreign one refused now, as
    /// for [`send`](Io::send).
    pub fn listen<S, F>(&self, source: S, f: F) -> Result<Listener, IoError>
    where
        S: Node,
        S::Event: 'static,
        F: FnMut(S::Event) + 'static,
    {
        let flag = self.register(
            vec![source.node_token()],
            move |runtime, skip_stale, flag| runtime.listen_queued(skip_stale, flag, source, f),
        )?;
        Ok(Listener::new(Some(flag)))
    }

    /// Listens to a cell, as [`Runtime::listen_cell`] does, from the next
    /// pump: the first call, with the current value, runs at the pump.
    /// The [`Listener`] comes back now, and dropping it before that pump
    /// cancels the registration.
    pub fn listen_cell<C, F>(&self, cell: C, f: F) -> Result<Listener, IoError>
    where
        C: CellRef,
        F: FnMut(&C::Value) + 'static,
    {
        let flag = self.register(vec![cell.token()], move |runtime, skip_stale, flag| {
            runtime.listen_cell_queued(skip_stale, flag, cell, f)
        })?;
        Ok(Listener::new(Some(flag)))
    }

    /// Listens to a cell's steps, as [`Runtime::listen_steps`] does, from
    /// the next pump. The [`Listener`] comes back now, and dropping it
    /// before that pump cancels the registration.
    pub fn listen_steps<C, F>(&self, cell: C, f: F) -> Result<Listener, IoError>
    where
        C: CellRef,
        F: FnMut(&C::Value) + 'static,
    {
        let flag = self.register(vec![cell.token()], move |runtime, skip_stale, flag| {
            runtime.listen_steps_queued(skip_stale, flag, cell, f)
        })?;
        Ok(Listener::new(Some(flag)))
    }

    /// Anchors what `value` holds, as [`Runtime::anchor`] does. The
    /// [`Anchored`] comes back now, carrying the value. The waiting call
    /// keeps the value's tokens alive until the next pump registers the
    /// anchor, and the anchor keeps them after; dropping the `Anchored`
    /// before that pump cancels both. A stale token is found at the pump,
    /// and a foreign one refused now, as for [`send`](Io::send).
    pub fn anchor<T: Trace>(&self, value: T) -> Result<Anchored<T>, IoError> {
        let mut tracer = Tracer::new();
        value.trace(&mut tracer);
        let tokens = tracer.visited;
        let flag = self.register(tokens.clone(), move |runtime, skip_stale, flag| {
            runtime.anchor_queued(skip_stale, flag, tokens)
        })?;
        Ok(Anchored::new(value, Anchor::new(Some(flag))))
    }

    /// The state the handle shares with its runtime, if the runtime takes
    /// a call that names `tokens`: it hasn't dropped, isn't poisoned, isn't
    /// running graph code, and owns every token.
    fn state(&self, tokens: &[Token]) -> Result<Rc<IoState>, IoError> {
        let state = self.0.upgrade().ok_or(IoError::Gone)?;
        if state.poisoned.get() {
            return Err(IoError::Poisoned);
        }
        if state.graph_code.get() {
            return Err(IoError::FromGraphCode);
        }
        if tokens.iter().any(|token| token.graph != state.graph) {
            return Err(IoError::ForeignGraph);
        }
        Ok(state)
    }

    /// Queues a unit that roots `roots` while it waits.
    fn unit(
        &self,
        roots: Vec<Token>,
        f: impl FnOnce(&mut IoTransaction<'_>) + 'static,
    ) -> Result<(), IoError> {
        let state = self.state(&roots)?;
        push(
            &state,
            Waiting {
                call: Box::new(move |runtime, skip_stale| runtime.run_unit(skip_stale, f)),
                roots,
                guard: None,
            },
        );
        Ok(())
    }

    /// Makes the liveness a guard shares with its registration, and queues
    /// the registration, which roots `roots` while it waits and the guard
    /// lives. The pump skips it if the guard has gone.
    fn register(
        &self,
        roots: Vec<Token>,
        register: impl FnOnce(&mut Runtime<Local>, bool, Liveness) -> Result<(), Stop> + 'static,
    ) -> Result<Liveness, IoError> {
        let state = self.state(&roots)?;
        let flag = Liveness::new(&state.released);
        let shared = flag.clone();
        push(
            &state,
            Waiting {
                call: Box::new(move |runtime, skip_stale| register(runtime, skip_stale, shared)),
                roots,
                guard: Some(flag.clone()),
            },
        );
        Ok(flag)
    }
}

/// Queues a call, and wakes the driver.
fn push(state: &IoState, waiting: Waiting) {
    state.calls.borrow_mut().push_back(waiting);
    state.wake();
}
