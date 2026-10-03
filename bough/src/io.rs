//! The same-thread handle (RFD 6, RFD 7): an [`Io`] queues its calls for
//! the driver's next pump.
//!
//! A GTK signal handler, a DOM closure or a listener is `'static`, and
//! can't hold `&mut Runtime`. So it holds an `Io`, which reaches the
//! runtime only through a queue the two share. The pump runs the queue
//! after the input slots, merged with a `RemoteIo`'s by the stamp every
//! call takes, so both handles' calls run in the order they were made. It
//! takes only the calls made before it began.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::{Rc, Weak};
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::slice;
use core::task::Waker;

use crate::cell::CellRef;
use crate::error::{IoError, IoTransactionError};
use crate::guard::{Liveness, Released, Stamps};
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

/// The tokens a waiting registration names, which collection keeps alive
/// until it runs: a listener's node, or the tokens an anchor's value
/// holds. A unit, a send or a transaction, keeps none: a send to an input
/// no root reaches can't be observed, so the pump reports it as stale
/// rather than keep the input for it. One token needs no allocation of
/// its own.
pub(crate) enum Roots {
    None,
    One(Token),
    Many(Vec<Token>),
}

impl Roots {
    pub(crate) fn as_slice(&self) -> &[Token] {
        match self {
            Roots::None => &[],
            Roots::One(token) => slice::from_ref(token),
            Roots::Many(tokens) => tokens,
        }
    }
}

/// A call waiting in a handle's queue, and what it keeps alive until it
/// runs.
pub(crate) struct Waiting<C> {
    pub(crate) call: C,
    pub(crate) roots: Roots,
    /// A registration's guard. Once the guard has gone, the call keeps
    /// nothing alive, and the pump skips it.
    pub(crate) guard: Option<Liveness>,
    /// When the call was made, among both handles' calls. Taken as the call
    /// is queued.
    pub(crate) stamp: usize,
    /// A once-listener's registration, for the check a debug build makes
    /// as its runtime drops. It exists only where the check does.
    #[cfg(all(debug_assertions, feature = "std"))]
    once: bool,
}

impl<C> Waiting<C> {
    /// A call not yet stamped.
    pub(crate) fn new(call: C, roots: Roots, guard: Option<Liveness>) -> Self {
        Waiting {
            call,
            roots,
            guard,
            stamp: 0,
            #[cfg(all(debug_assertions, feature = "std"))]
            once: false,
        }
    }

    /// Marks a once-listener's registration, if `once`, where a debug build
    /// with `std` checks for one as its runtime drops.
    #[inline]
    pub(crate) fn once_if(self, once: bool) -> Self {
        #[cfg(all(debug_assertions, feature = "std"))]
        {
            Waiting { once, ..self }
        }
        #[cfg(not(all(debug_assertions, feature = "std")))]
        {
            let _ = once;
            self
        }
    }

    /// Whether this is a once-listener's registration whose handle was
    /// kept: live, with only the two shares the call and its closure hold.
    #[cfg(all(debug_assertions, feature = "std"))]
    pub(crate) fn kept_once(&self) -> bool {
        self.once
            && self
                .guard
                .as_ref()
                .is_some_and(|flag| flag.is_live() && flag.shares() == 2)
    }

    /// Adds to `roots` the tokens this call keeps alive: none once a
    /// registration's guard has gone.
    pub(crate) fn roots(&self, roots: &mut Vec<Token>) {
        if self.guard.as_ref().is_none_or(Liveness::is_live) {
            roots.extend_from_slice(self.roots.as_slice());
        }
    }
}

/// What a runtime keeps for its same-thread handle: the queue its [`Io`]s
/// share in `Local`, and nothing in `Threaded`, which has no `Io`.
#[doc(hidden)]
pub trait IoQueue<M: Mode>: 'static {
    /// An empty queue for graph `graph`, whose guards count their releases
    /// on `released`, and whose calls take their stamps from `stamps`.
    fn new(graph: u32, released: &Released, stamps: &Stamps) -> Self;
    /// Graph code starts or stops running. A call is refused while it
    /// runs.
    fn graph_code(&self, running: bool);
    /// Mirrors the runtime's poison.
    fn poison(&self);
    /// Replaces the waker a call wakes.
    fn set_waker(&self, waker: &Waker);
    /// A pump begins: the next call wakes the driver again.
    fn begin_pump(&self);
    /// The stamp of the oldest call.
    fn front(&self) -> Option<usize>;
    /// The oldest call.
    fn pop(&self) -> Option<Call<M>>;
    /// Adds to `roots` the tokens the waiting registrations name, but none
    /// of one whose guard has gone.
    fn roots(&self, roots: &mut Vec<Token>);
    /// How many once-listener registrations wait whose handles were kept,
    /// for the check a debug build makes as its runtime drops.
    #[cfg(all(debug_assertions, feature = "std"))]
    fn kept_once(&self) -> usize;
}

/// What a `Local` runtime shares with its [`Io`]s.
#[doc(hidden)]
pub struct IoState {
    /// The graph's id, for a call's foreign-token check.
    graph: u32,
    /// The calls waiting for the next pump, in the order they were made.
    calls: RefCell<VecDeque<Waiting<Call<Local>>>>,
    /// Set while graph code runs, from `arm` to `disarm`.
    graph_code: Cell<bool>,
    /// The runtime's poison, mirrored as the panic that caused it leaves
    /// the runtime, or by the first entry that finds it.
    poisoned: Cell<bool>,
    /// The driver's waker.
    waker: Cell<Option<Waker>>,
    /// A call has woken the driver since the last pump began.
    woken: Cell<bool>,
    /// The runtime's count of released guards, which a guard made here
    /// shares.
    released: Released,
    /// What a call takes its stamp from, shared with the inbox.
    stamps: Stamps,
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
    fn new(graph: u32, released: &Released, stamps: &Stamps) -> Self {
        Rc::new(IoState {
            graph,
            calls: RefCell::new(VecDeque::new()),
            graph_code: Cell::new(false),
            poisoned: Cell::new(false),
            waker: Cell::new(None),
            woken: Cell::new(false),
            released: released.clone(),
            stamps: stamps.clone(),
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
    fn begin_pump(&self) {
        self.woken.set(false);
    }

    #[inline]
    fn front(&self) -> Option<usize> {
        self.calls.borrow().front().map(|waiting| waiting.stamp)
    }

    fn pop(&self) -> Option<Call<Local>> {
        self.calls
            .borrow_mut()
            .pop_front()
            .map(|waiting| waiting.call)
    }

    fn roots(&self, roots: &mut Vec<Token>) {
        for waiting in self.calls.borrow().iter() {
            waiting.roots(roots);
        }
    }

    #[cfg(all(debug_assertions, feature = "std"))]
    fn kept_once(&self) -> usize {
        self.calls
            .borrow()
            .iter()
            .filter(|waiting| waiting.kept_once())
            .count()
    }
}

/// A `Threaded` runtime has no [`Io`], so nothing waits.
#[cfg(target_has_atomic = "ptr")]
#[doc(hidden)]
pub struct NoIo;

#[cfg(target_has_atomic = "ptr")]
impl IoQueue<Threaded> for NoIo {
    fn new(_: u32, _: &Released, _: &Stamps) -> Self {
        NoIo
    }
    #[inline]
    fn graph_code(&self, _: bool) {}
    fn poison(&self) {}
    fn set_waker(&self, _: &Waker) {}
    #[inline]
    fn begin_pump(&self) {}
    #[inline]
    fn front(&self) -> Option<usize> {
        None
    }
    fn pop(&self) -> Option<Call<Threaded>> {
        None
    }
    fn roots(&self, _: &mut Vec<Token>) {}
    #[cfg(all(debug_assertions, feature = "std"))]
    fn kept_once(&self) -> usize {
        0
    }
}

/// A handle for I/O code that can't hold the runtime: a GTK signal
/// handler, a DOM closure, a listener. `Clone`, not `Send`, and on every
/// target. From [`Runtime::io`].
///
/// Every call queues for the driver's next [`pump`](Runtime::pump) and
/// returns at once, and nothing reads through it: only the `Runtime`
/// reads. The pump runs the calls after the input slots, in the order
/// they were made, among a [`RemoteIo`](crate::RemoteIo)'s too, taking
/// only those made before it began. So a call a listener makes during a pump waits for the next
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
/// [`set_waker`](Runtime::set_waker), unless another call through an `Io`
/// has since the last pump began: one wake is enough for every call that
/// pump will run. A `RemoteIo` keeps the same rule for its own calls.
///
/// A waiting registration keeps the tokens it names alive until the pump
/// runs it, through any collection before then: a listener's node, or the
/// tokens an anchor's value holds. So a listener handed a plain row can
/// listen to it or anchor it, and the row lasts until the pump registers
/// what keeps it. One whose guard has gone keeps nothing alive. A waiting
/// send or transaction keeps nothing alive either: a send to an input no
/// root reaches can't be observed, so the pump reports it as stale.
///
/// A call is refused, with an [`IoError`], if the runtime has dropped, is
/// poisoned or is running graph code, or if a token the call names is
/// another graph's, checked in that order. A transaction names no tokens
/// when it's queued, so it's refused with an [`IoTransactionError`], which
/// has the first three. An `Io` is `Clone + 'static`,
/// so graph code can capture one: a `map` function, a `construct`
/// closure, a split's iterator. A call from there would be I/O inside FRP
/// logic, so it's refused with [`IoError::FromGraphCode`].
///
/// A panic that poisons the runtime, in graph code, a listener, a
/// transaction's closure or a `Drop` a collection runs, marks the poison
/// in the `Io` on its way out of the runtime, where panics unwind, so
/// every call reports `Poisoned` from then on. Where a panic is a trap, as on wasm, nothing runs after it:
/// until an entry on the runtime finds the poison, a call reports
/// `FromGraphCode` after a panic in graph code, and queues and is lost
/// after one elsewhere.
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
    /// A waiting send keeps nothing alive, so whatever keeps the input
    /// alive must still do so at the pump. Whether the input is collected
    /// is graph knowledge, found there, as for a remote unit: a panic in a
    /// debug build, and in a release build a no-op that
    /// [`stale_operations`](Runtime::stale_operations) counts;
    /// [`try_pump`](Runtime::try_pump) returns it. A token from another
    /// graph is refused now, with [`IoError::ForeignGraph`].
    pub fn send<A: 'static>(&self, input: Input<A>, value: A) -> Result<(), IoError> {
        let state = self.state(&[input.token])?;
        unit(&state, move |tx| tx.send(input, value));
        Ok(())
    }

    /// Queues several sends as one unit, and so one transaction: `f` runs
    /// on the driver at its next pump, with an [`IoTransaction`] whose sends
    /// are simultaneous. The closure is I/O code: it has no graph access,
    /// and its order of sends doesn't matter.
    ///
    /// A unit whose send fails is dropped whole at the pump, with none of
    /// its sends run, as a remote's is; that's where a token from another
    /// graph is found, too. A panic in the closure poisons the runtime.
    /// Like a send, it keeps nothing alive while it waits.
    pub fn transaction<F>(&self, f: F) -> Result<(), IoTransactionError>
    where
        F: FnOnce(&mut IoTransaction<'_>) + 'static,
    {
        let state = self.queue()?;
        unit(&state, f);
        Ok(())
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
            Roots::One(source.node_token()),
            false,
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
        let flag = self.register(
            Roots::One(cell.token()),
            false,
            move |runtime, skip_stale, flag| runtime.listen_cell_queued(skip_stale, flag, cell, f),
        )?;
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
        let flag = self.register(
            Roots::One(cell.token()),
            false,
            move |runtime, skip_stale, flag| runtime.listen_steps_queued(skip_stale, flag, cell, f),
        )?;
        Ok(Listener::new(Some(flag)))
    }

    /// Listens to a stream's next event only, as [`Runtime::listen_once`]
    /// does, from the next pump. The [`Listener`] comes back now. It's a
    /// root until the listener fires, waiting included, and dropping it
    /// before then cancels the listener.
    pub fn listen_once<S, F>(&self, source: S, f: F) -> Result<Listener, IoError>
    where
        S: Node,
        S::Event: 'static,
        F: FnOnce(S::Event) + 'static,
    {
        let flag = self.register(
            Roots::One(source.node_token()),
            true,
            move |runtime, skip_stale, flag| {
                runtime.listen_once_queued(skip_stale, flag, source, f)
            },
        )?;
        Ok(Listener::new(Some(flag)))
    }

    /// Hears a cell's value once, as [`Runtime::listen_cell_once`] does, at
    /// the next pump: `f` runs there, with the value then, and that spends
    /// the listener. It's how I/O code with no runtime reads a cell. The
    /// [`Listener`] comes back now; it's a root until the pump, and
    /// dropping it before then cancels the call.
    pub fn listen_cell_once<C, F>(&self, cell: C, f: F) -> Result<Listener, IoError>
    where
        C: CellRef,
        F: FnOnce(&C::Value) + 'static,
    {
        let flag = self.register(
            Roots::One(cell.token()),
            true,
            move |runtime, skip_stale, flag| {
                runtime.listen_cell_once_queued(skip_stale, flag, cell, f)
            },
        )?;
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
        let flag = self.register(
            Roots::Many(tokens.clone()),
            false,
            move |runtime, skip_stale, flag| runtime.anchor_queued(skip_stale, flag, tokens),
        )?;
        Ok(Anchored::new(value, Anchor::new(Some(flag))))
    }

    /// The state the handle shares with its runtime, if the runtime takes
    /// a call: it hasn't dropped, isn't poisoned, and isn't running graph
    /// code. A transaction's call makes only these checks.
    fn queue(&self) -> Result<Rc<IoState>, IoTransactionError> {
        let state = self.0.upgrade().ok_or(IoTransactionError::Gone)?;
        if state.poisoned.get() {
            return Err(IoTransactionError::Poisoned);
        }
        if state.graph_code.get() {
            return Err(IoTransactionError::FromGraphCode);
        }
        Ok(state)
    }

    /// The state, if the runtime takes a call that names `tokens`: the
    /// queue's checks, and then that it owns every token.
    fn state(&self, tokens: &[Token]) -> Result<Rc<IoState>, IoError> {
        let state = self.queue()?;
        if tokens.iter().any(|token| token.graph != state.graph) {
            return Err(IoError::ForeignGraph);
        }
        Ok(state)
    }

    /// Makes the liveness a guard shares with its registration, and queues
    /// the registration, which keeps `roots` alive while it waits and the
    /// guard lives. The pump skips it if the guard has gone. `once` marks a
    /// once-listener's.
    fn register(
        &self,
        roots: Roots,
        once: bool,
        register: impl FnOnce(&mut Runtime<Local>, bool, Liveness) -> Result<(), Stop> + 'static,
    ) -> Result<Liveness, IoError> {
        let state = self.state(roots.as_slice())?;
        let flag = Liveness::new(&state.released);
        let shared = flag.clone();
        push(
            &state,
            Waiting::<Call<Local>>::new(
                Box::new(move |runtime, skip_stale| register(runtime, skip_stale, shared)),
                roots,
                Some(flag.clone()),
            )
            .once_if(once),
        );
        Ok(flag)
    }
}

/// Queues a unit, which keeps nothing alive.
fn unit(state: &IoState, f: impl FnOnce(&mut IoTransaction<'_>) + 'static) {
    push(
        state,
        Waiting::new(
            Box::new(move |runtime, skip_stale| runtime.run_io_unit(skip_stale, f)),
            Roots::None,
            None,
        ),
    );
}

/// Stamps a call, queues it, and wakes the driver.
fn push(state: &IoState, mut waiting: Waiting<Call<Local>>) {
    waiting.stamp = state.stamps.take();
    state.calls.borrow_mut().push_back(waiting);
    state.wake();
}

/// A registration a `RemoteIo` queued. The handle doesn't know its
/// runtime's mode, so a registration can register into either, and the
/// runtime's mode picks which through `Mode::register`.
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
#[doc(hidden)]
pub trait Registration: Send {
    /// Registers into a `Local` runtime.
    fn local(self: Box<Self>, runtime: &mut Runtime<Local>, skip_stale: bool) -> Result<(), Stop>;
    /// Registers into a `Threaded` runtime.
    fn threaded(
        self: Box<Self>,
        runtime: &mut Runtime<Threaded>,
        skip_stale: bool,
    ) -> Result<(), Stop>;
}

/// What a registration does in a runtime of mode `M`. A registration that
/// can go into both modes is a [`Registration`].
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
#[doc(hidden)]
pub trait RegisterIn<M: Mode> {
    /// Registers into `runtime`, at the pump.
    fn register(self, runtime: &mut Runtime<M>, skip_stale: bool) -> Result<(), Stop>;
}

#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
impl<R> Registration for R
where
    R: RegisterIn<Local> + RegisterIn<Threaded> + Send,
{
    fn local(self: Box<Self>, runtime: &mut Runtime<Local>, skip_stale: bool) -> Result<(), Stop> {
        RegisterIn::<Local>::register(*self, runtime, skip_stale)
    }
    fn threaded(
        self: Box<Self>,
        runtime: &mut Runtime<Threaded>,
        skip_stale: bool,
    ) -> Result<(), Stop> {
        RegisterIn::<Threaded>::register(*self, runtime, skip_stale)
    }
}

/// A listener a `RemoteIo` asked for, on a stream.
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
pub(crate) struct Listen<S, F> {
    pub(crate) flag: Liveness,
    pub(crate) source: S,
    pub(crate) f: F,
}

#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
impl<M, S, F> RegisterIn<M> for Listen<S, F>
where
    M: Mode + crate::mode::Accepts<F>,
    S: Node,
    S::Event: 'static,
    F: FnMut(S::Event) + 'static,
{
    fn register(self, runtime: &mut Runtime<M>, skip_stale: bool) -> Result<(), Stop> {
        runtime.listen_queued(skip_stale, self.flag, self.source, self.f)
    }
}

/// A listener a `RemoteIo` asked for, on a cell: with a first call at
/// registration, `listen_cell`, or without, `listen_steps`.
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
pub(crate) struct ListenCell<C, F> {
    pub(crate) flag: Liveness,
    pub(crate) cell: C,
    pub(crate) f: F,
    pub(crate) now: bool,
}

#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
impl<M, C, F> RegisterIn<M> for ListenCell<C, F>
where
    M: Mode + crate::mode::Accepts<F>,
    C: CellRef,
    F: FnMut(&C::Value) + 'static,
{
    fn register(self, runtime: &mut Runtime<M>, skip_stale: bool) -> Result<(), Stop> {
        if self.now {
            runtime.listen_cell_queued(skip_stale, self.flag, self.cell, self.f)
        } else {
            runtime.listen_steps_queued(skip_stale, self.flag, self.cell, self.f)
        }
    }
}

/// A once-listener a `RemoteIo` asked for, on a stream.
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
pub(crate) struct ListenOnce<S, F> {
    pub(crate) flag: Liveness,
    pub(crate) source: S,
    pub(crate) f: F,
}

#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
impl<M, S, F> RegisterIn<M> for ListenOnce<S, F>
where
    M: Mode + crate::mode::Accepts<F>,
    S: Node,
    S::Event: 'static,
    F: FnOnce(S::Event) + 'static,
{
    fn register(self, runtime: &mut Runtime<M>, skip_stale: bool) -> Result<(), Stop> {
        runtime.listen_once_queued(skip_stale, self.flag, self.source, self.f)
    }
}

/// A cell's value a `RemoteIo` asked to hear once, at the pump. The call
/// runs there, so the runtime stores nothing.
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
pub(crate) struct ListenCellOnce<C, F> {
    pub(crate) flag: Liveness,
    pub(crate) cell: C,
    pub(crate) f: F,
}

#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
impl<M, C, F> RegisterIn<M> for ListenCellOnce<C, F>
where
    M: Mode,
    C: CellRef,
    F: FnOnce(&C::Value),
{
    fn register(self, runtime: &mut Runtime<M>, skip_stale: bool) -> Result<(), Stop> {
        runtime.listen_cell_once_queued(skip_stale, self.flag, self.cell, self.f)
    }
}

/// A cell once-listener tied to a remote's unit, whose call runs when the
/// unit is done. It registers nothing, and roots nothing.
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
pub(crate) struct TiedCell<C, F> {
    pub(crate) cell: C,
    pub(crate) f: F,
}

#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
impl<M, C, F> RegisterIn<M> for TiedCell<C, F>
where
    M: Mode,
    C: CellRef,
    F: FnOnce(&C::Value),
{
    fn register(self, runtime: &mut Runtime<M>, _skip_stale: bool) -> Result<(), Stop> {
        runtime.tied_cell(self.cell, self.f);
        Ok(())
    }
}

/// An anchor a `RemoteIo` asked for: the tokens its value's `Trace` found.
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
pub(crate) struct AnchorTokens {
    pub(crate) flag: Liveness,
    pub(crate) tokens: Vec<Token>,
}

#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
impl<M: Mode> RegisterIn<M> for AnchorTokens {
    fn register(self, runtime: &mut Runtime<M>, skip_stale: bool) -> Result<(), Stop> {
        runtime.anchor_queued(skip_stale, self.flag, self.tokens)
    }
}
