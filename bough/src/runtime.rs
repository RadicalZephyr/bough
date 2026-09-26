//! The I/O side (RFD 2, RFD 5, RFD 6, RFD 7).

#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
use alloc::boxed::Box;
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Deref;
use core::task::Waker;

use crate::Build;
use crate::cell::CellRef;
#[cfg(feature = "statistics")]
use crate::engine::Statistics;
#[cfg(any(feature = "std", feature = "critical-section"))]
use crate::engine::edge::Connection;
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
use crate::engine::edge::{Fault, Inbox, Start};
use crate::engine::{Cx, Entry, LISTENERS, TokenFault, part};
use crate::error::{PoisonedError, PumpError, SendError, TokenError, TransactionSendError};
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
use crate::error::{RemoteSendError, RemoteTransactionError};
use crate::guard::{Liveness, Released};
#[cfg(target_has_atomic = "ptr")]
use crate::mode::Threaded;
use crate::mode::{Accepts, Erase, Local, Mode};
use crate::source::Node;
use crate::token::{Input, Token};
use crate::trace::{Trace, Tracer};

/// A built graph: the only place transactions run, and the only holder of
/// the I/O API.
///
/// The graph is entered from exactly one place at a time. Graph code never
/// holds a `Runtime`; every public entry point checks the
/// transaction-in-progress flag, so a `Runtime` smuggled into graph code fails
/// at the entry. The same flag is the poison: only a transaction that
/// finishes clears it, so an entry that finds it set outside a transaction
/// reports `Poisoned`. That holds where a panic unwinds and where a panic is
/// a trap alike, since neither needs code to run on the way out (RFD 5).
///
/// `Runtime<Threaded>` is `Send` because every field is: the engine stores
/// each value, closure and chain in the mode's carrier, which is
/// `Box<dyn Any + Send>` there. No `unsafe impl` says so.
///
/// # Memory
///
/// Nodes live in an arena the graph owns, and a node is alive while a root
/// reaches it (RFD 3). There are three kinds of root: whatever the build
/// closure returned, every live [`Listener`], and every live [`Anchor`].
/// A node reaches what it depends on, the tokens in a stateful cell's
/// committed value (found through [`Trace`]), a switch's current inner,
/// the cells a chain snapshots or gates on, and what
/// [`Build::depends`](crate::Build::depends) declares. Collection frees
/// every node no root reaches, and a token naming a freed node is stale:
/// its next use is an error, never a read of another node. So a token I/O
/// code keeps, received as data from a listener, must be anchored or
/// listened to before the next transaction, or it may be lost. Collection
/// is automatic by default, and never runs inside a transaction; see
/// [`CollectionPolicy`].
pub struct Runtime<M: Mode = Local> {
    build: Build<M>,
    /// The build closure's return value, traced once: the permanent roots.
    roots: Vec<Token>,
    /// The anchored nodes, each with the liveness its anchor shares. A
    /// released anchor is taken out at the next collection.
    anchors: Vec<(u32, Liveness)>,
    /// The count of released guards, which every guard's state shares, so
    /// that a release needs no graph access.
    released: Released,
    /// `released` as the last collection found it.
    released_before: usize,
    /// Live nodes after the last collection, zero before the first.
    baseline: usize,
    policy: CollectionPolicy,
    /// `set_collect_after_every_transaction`.
    stress: bool,
    /// Operations on collected nodes dropped in release builds.
    stale_operations: u64,
}

/// Transaction zero: nothing is started, so the new-node phase runs every
/// node the closure built, dependencies first.
fn build_graph<M: Mode, R: Trace>(f: impl FnOnce(&mut Build<M>) -> R) -> (Runtime<M>, R) {
    let mut build = Build::<M>::new();
    let id = build.graph_id;
    build.begin();
    build.push_scope();
    let r = f(&mut build);
    // A `mem::swap` with another graph's build context is safe code; with
    // no `unsafe` in the engine it is a wrong-graph error, caught here.
    assert_eq!(
        build.graph_id, id,
        "bough: the build context was swapped for another graph's"
    );
    build.pop_scope();
    build.finish();
    let mut tracer = Tracer::new();
    r.trace(&mut tracer);
    let graph = Runtime {
        build,
        roots: tracer.visited,
        anchors: Vec::new(),
        released: Released::new(),
        released_before: 0,
        baseline: 0,
        policy: CollectionPolicy::Automatic,
        stress: false,
        stale_operations: 0,
    };
    (graph, r)
}

impl Runtime<Local> {
    /// Builds a `Local` graph. The closure gets the only [`Build`] context;
    /// whatever it returns is the edge of the graph and its permanent root
    /// set, which is why `R: Trace`.
    ///
    /// The build closure runs as transaction zero. Its child transactions,
    /// which a [`split`](crate::Source::split) or a
    /// [`defer`](crate::Source::defer) that fires in it starts, run before
    /// `build` returns.
    pub fn build<R: Trace>(f: impl FnOnce(&mut Build<Local>) -> R) -> (Runtime<Local>, R) {
        build_graph(f)
    }
}

#[cfg(target_has_atomic = "ptr")]
impl Runtime<Threaded> {
    /// Builds a `Threaded` graph, which is `Send`; every value and closure it
    /// stores must be `Send`.
    ///
    /// A separate constructor rather than a mode parameter on `build`, because
    /// a defaulted type parameter takes no part in inferring an associated
    /// function.
    pub fn build_threaded<R: Trace + Send>(
        f: impl FnOnce(&mut Build<Threaded>) -> R,
    ) -> (Runtime<Threaded>, R) {
        build_graph(f)
    }
}

/// What the same-thread handle needs of the graph it shares, which it
/// cannot borrow while the graph is busy.
#[cfg(all(feature = "std", target_has_atomic = "ptr"))]
impl Runtime<Local> {
    /// The inbox: its poison mirror, its guard and the driver's waker,
    /// read while the graph is busy.
    pub(crate) fn inbox(&self) -> Arc<Inbox> {
        self.build.edge.inbox.clone()
    }

    /// The count of released guards, for the liveness of a guard made while
    /// the graph is busy.
    pub(crate) fn released(&self) -> Released {
        self.released.clone()
    }

    /// [`listen`](Runtime::listen), with the liveness of a [`Listener`] the
    /// handle gave out before the graph could register it. A guard with no
    /// owner left was dropped first, and nothing registers.
    pub(crate) fn listen_flagged<S, F>(&mut self, flag: Liveness, source: S, f: F)
    where
        S: Node,
        S::Event: 'static,
        F: FnMut(S::Event) + 'static,
    {
        self.enter();
        if !flag.is_live() {
            return;
        }
        if let Some(i) = self.checked(source.node_token(), LISTEN) {
            self.attach_flag(i, f, call_stream::<Local, S, F>, flag);
        }
    }

    /// [`listen_cell`](Runtime::listen_cell), with a guard's liveness, as
    /// [`listen_flagged`](Runtime::listen_flagged). The first call runs now,
    /// when the listener is registered.
    pub(crate) fn listen_cell_flagged<C, F>(&mut self, flag: Liveness, cell: C, mut f: F)
    where
        C: CellRef,
        F: FnMut(&C::Value) + 'static,
    {
        self.enter();
        if !flag.is_live() {
            return;
        }
        let Some(i) = self.checked(cell.token(), LISTEN) else {
            return;
        };
        f(self.build.value::<C::Value>(i));
        self.attach_flag(i, f, call_cell::<Local, C::Value, F>, flag);
    }

    /// [`listen_steps`](Runtime::listen_steps), with a guard's liveness, as
    /// [`listen_flagged`](Runtime::listen_flagged).
    pub(crate) fn listen_steps_flagged<C, F>(&mut self, flag: Liveness, cell: C, f: F)
    where
        C: CellRef,
        F: FnMut(&C::Value) + 'static,
    {
        self.enter();
        if !flag.is_live() {
            return;
        }
        if let Some(i) = self.checked(cell.token(), LISTEN) {
            self.attach_flag(i, f, call_cell::<Local, C::Value, F>, flag);
        }
    }

    /// [`anchor`](Runtime::anchor), with the liveness of an [`Anchor`] the
    /// handle gave out, as [`listen_flagged`](Runtime::listen_flagged), and
    /// the tokens it traced when it was called.
    pub(crate) fn anchor_flagged(&mut self, flag: Liveness, tokens: Vec<Token>) {
        self.enter();
        if !flag.is_live() {
            return;
        }
        let nodes: Vec<u32> = tokens
            .into_iter()
            .filter_map(|token| self.checked(token, ANCHOR))
            .collect();
        for i in nodes {
            self.anchors.push((i, flag.clone()));
        }
    }
}

/// The handles the same-thread handle gives out before the graph can
/// register them.
#[cfg(all(feature = "std", target_has_atomic = "ptr"))]
impl Listener {
    /// A listener whose registration shares `flag`, now or later.
    pub(crate) fn from_flag(flag: Liveness) -> Self {
        Listener::new(Some(flag))
    }
}

#[cfg(all(feature = "std", target_has_atomic = "ptr"))]
impl Anchor {
    /// An anchor whose registration shares `flag`, now or later.
    pub(crate) fn from_flag(flag: Liveness) -> Self {
        Anchor::new(Some(flag))
    }
}

const POISONED: &str = "bough: the graph is poisoned: a panic escaped an earlier transaction";

/// What the operations on collected nodes that the semantics cannot
/// observe were asked to do, for the debug-build panic.
const SEND: &str = "a send to a collected input";
const LISTEN: &str = "a listener on a collected node";
const ANCHOR: &str = "an anchor on a collected node";

/// A stream listener's call: take the event from a linear stream, clone it
/// from a shared one.
fn call_stream<M, S, F>(f: &mut M::Carrier, b: &mut Build<M>, n: u32)
where
    M: Mode,
    S: Node,
    F: FnMut(S::Event) + 'static,
{
    if let Some(v) = S::pull_inner(&mut Cx { b }, n) {
        part::<M, F>(f)(v)
    }
}

/// A cell listener's call: the committed value, which after commit is the
/// value the step produced.
fn call_cell<M, A, F>(f: &mut M::Carrier, b: &mut Build<M>, n: u32)
where
    M: Mode,
    A: 'static,
    F: FnMut(&A) + 'static,
{
    let v = b.value::<A>(n);
    part::<M, F>(f)(v)
}

impl<M: Mode> Runtime<M> {
    /// Whether a transaction never finished. Every entry checks this, and
    /// one that finds it set mirrors it into the inbox, so that remote
    /// sends fail from then on (RFD 6).
    pub(crate) fn poisoned(&self) -> bool {
        let poisoned = self.build.in_tx;
        #[cfg(all(
            target_has_atomic = "ptr",
            any(feature = "std", feature = "critical-section")
        ))]
        if poisoned {
            self.build.edge.inbox.poison();
        }
        poisoned
    }

    /// The check every panicking entry makes first.
    fn enter(&self) {
        assert!(!self.poisoned(), "{POISONED}");
    }

    /// The checks of the `try_` entries that take a token: poison, graph,
    /// liveness.
    fn lookup(&self, token: Token) -> Result<u32, TokenError> {
        if self.poisoned() {
            return Err(TokenError::Poisoned);
        }
        self.build.lookup(token).map_err(|fault| match fault {
            TokenFault::Foreign => TokenError::ForeignGraph,
            TokenFault::Stale => TokenError::Stale,
        })
    }

    /// The token check of a panicking entry whose operation, on a collected
    /// node, the semantics cannot observe: a foreign token panics, and a
    /// stale one is [`stale_operation`](Runtime::stale_operation), `None`.
    fn checked(&mut self, token: Token, what: &str) -> Option<u32> {
        match self.build.lookup(token) {
            Ok(i) => Some(i),
            Err(TokenFault::Foreign) => panic!("bough: a token from another graph"),
            Err(TokenFault::Stale) => {
                self.stale_operation(what);
                None
            }
        }
    }

    /// Sending to a collected input, listening to a collected node or
    /// anchoring one has no effect the semantics can observe (RFD 5): a
    /// panic in a debug build and, following the integer-overflow
    /// precedent, a no-op in a release build, counted so that a release
    /// build can still report that it drops them.
    fn stale_operation(&mut self, what: &str) {
        if cfg!(debug_assertions) {
            panic!(
                "bough: {what}: its token is stale. A node no root reaches is collected; \
                 anchor or listen to what I/O code keeps a token of. In a release build this \
                 is a no-op that `Runtime::stale_operations` counts"
            );
        }
        self.stale_operations += 1;
    }

    /// Guards released since the last collection.
    fn released_since(&self) -> usize {
        self.released.count().wrapping_sub(self.released_before)
    }

    /// Runs a collection if one is due, as a transaction opens: under the
    /// automatic policy when the nodes allocated and the handles released
    /// since the last collection exceed the live count it left, and always
    /// under the stress setting. Before the transaction rather than after
    /// it, so that I/O code can anchor a token a listener handed it in the
    /// transaction before (RFD 2's receive, then wire).
    pub(crate) fn collect_if_due(&mut self) {
        let due = self.stress
            || (self.policy == CollectionPolicy::Automatic
                && self
                    .build
                    .store
                    .allocated
                    .saturating_add(self.released_since())
                    > self.baseline);
        if due {
            self.collect_now();
        }
    }

    /// Collects now, and starts counting toward the next one.
    fn collect_now(&mut self) {
        let released = self.released.count();
        self.build.collect(&self.roots, &mut self.anchors);
        self.released_before = released;
        self.baseline = self.build.store.live;
        self.build.store.allocated = 0;
    }

    /// Sends one value in a transaction of its own. Returns after the
    /// transaction's listeners have run, and after its child transactions,
    /// which a [`split`](crate::Source::split) or a
    /// [`defer`](crate::Source::defer) that fires starts, have run with
    /// theirs.
    ///
    /// Panics on a foreign token or a poisoned graph. Sending to a collected
    /// input is unobservable by the semantics: a panic in debug builds and a
    /// no-op in release builds, which [`stale_operations`](Runtime::stale_operations)
    /// counts.
    ///
    /// A collection that is due runs first, before the transaction opens.
    pub fn send<A: 'static>(&mut self, input: Input<A>, value: A)
    where
        M: Accepts<A>,
    {
        self.enter();
        self.collect_if_due();
        self.send_without_collecting(input, value);
    }

    /// [`send`](Runtime::send) without the collection that may run first. The
    /// same-thread handle's queue sends this way: its sends are the I/O
    /// code a transaction's listeners asked for, and no collection runs
    /// between a transaction and that code.
    pub(crate) fn send_without_collecting<A: 'static>(&mut self, input: Input<A>, value: A)
    where
        M: Accepts<A>,
    {
        self.enter();
        // The token is checked before the transaction opens, so a foreign
        // token is a panic that leaves the graph usable.
        let Some(i) = self.checked(input.token, SEND) else {
            return;
        };
        self.build.begin();
        self.build
            .fire_start(i, value)
            .expect("bough engine: the only send of a transaction is not a double send");
        self.build.finish();
    }

    /// [`send`](Runtime::send), returning the error instead of panicking.
    pub fn try_send<A: 'static>(&mut self, input: Input<A>, value: A) -> Result<(), SendError>
    where
        M: Accepts<A>,
    {
        if self.poisoned() {
            return Err(SendError::Poisoned);
        }
        self.collect_if_due();
        let i = self
            .build
            .lookup(input.token)
            .map_err(|fault| match fault {
                TokenFault::Foreign => SendError::ForeignGraph,
                TokenFault::Stale => SendError::Stale,
            })?;
        self.build.begin();
        self.build
            .fire_start(i, value)
            .expect("bough engine: the only send of a transaction is not a double send");
        self.build.finish();
        Ok(())
    }

    /// Several sends in one instant.
    ///
    /// The sends are simultaneous: nothing runs until `f` returns, so their
    /// order inside `f` does not matter. As with [`send`](Runtime::send), the
    /// transaction's listeners and its child transactions run before it
    /// returns. A panic inside `f`, including one from
    /// [`Transaction::send`], escapes the transaction and poisons the graph.
    /// A collection that is due runs first, before the transaction opens.
    pub fn transaction<R>(&mut self, f: impl FnOnce(&mut Transaction<'_, M>) -> R) -> R {
        self.enter();
        self.collect_if_due();
        self.transaction_without_collecting(f)
    }

    /// [`transaction`](Runtime::transaction) without the collection that may
    /// run first, for the same-thread handle's queue, as
    /// [`send_without_collecting`](Runtime::send_without_collecting).
    pub(crate) fn transaction_without_collecting<R>(
        &mut self,
        f: impl FnOnce(&mut Transaction<'_, M>) -> R,
    ) -> R {
        self.enter();
        self.build.begin();
        let r = f(&mut Transaction { graph: self });
        self.build.finish();
        r
    }

    /// [`transaction`](Runtime::transaction), returning the error instead of
    /// panicking.
    pub fn try_transaction<R>(
        &mut self,
        f: impl FnOnce(&mut Transaction<'_, M>) -> R,
    ) -> Result<R, PoisonedError> {
        if self.poisoned() {
            return Err(PoisonedError);
        }
        Ok(self.transaction(f))
    }

    /// Listens to a materialized node. A linear stream is moved in, so it can
    /// be listened to once; a shared stream any number of times. Listeners
    /// take the event by value, have no graph access, and run after commit
    /// on the thread that called `send`, `transaction` or `pump`.
    ///
    /// A chain cannot be listened to:
    ///
    /// ```compile_fail,E0277
    /// use bough::{Runtime, Source};
    ///
    /// let (mut graph, events) = Runtime::build(|b| b.input::<u32>().0);
    /// let _l = graph.listen(events.map(|n| n + 1), |n| println!("{n}")); // error: Map<..> is not a Node
    /// ```
    ///
    /// A linear stream cannot be listened to twice:
    ///
    /// ```compile_fail,E0382
    /// use bough::{Runtime, Source};
    ///
    /// let (mut graph, events) = Runtime::build(|b| b.input::<u32>().0);
    /// let _a = graph.listen(events, |n| println!("{n}"));
    /// let _b = graph.listen(events, |n| println!("{n}")); // error: use of moved value
    /// ```
    pub fn listen<S, F>(&mut self, source: S, f: F) -> Listener
    where
        S: Node,
        S::Event: 'static,
        F: FnMut(S::Event) + 'static,
        M: Accepts<F>,
    {
        self.enter();
        match self.checked(source.node_token(), LISTEN) {
            Some(i) => self.attach(i, f, call_stream::<M, S, F>),
            None => Listener::new(None),
        }
    }

    /// [`listen`](Runtime::listen), returning the error instead of panicking.
    pub fn try_listen<S, F>(&mut self, source: S, f: F) -> Result<Listener, TokenError>
    where
        S: Node,
        S::Event: 'static,
        F: FnMut(S::Event) + 'static,
        M: Accepts<F>,
    {
        let i = self.lookup(source.node_token())?;
        Ok(self.attach(i, f, call_stream::<M, S, F>))
    }

    /// Listens to a cell: fires once now with the current value, then on
    /// every step, with the value by reference. The I/O form of
    /// [`steps_with_current`](crate::Cell::steps_with_current), Sodium's
    /// `value`.
    ///
    /// The cell is a [`Cell`](crate::Cell) or a [`State`](crate::State):
    /// the listener reads the committed value after commit, which an
    /// in-place accumulator has then. The call at registration runs outside
    /// any transaction, so a panic in it leaves the graph usable. A step to
    /// an equal value is a step.
    pub fn listen_cell<C, F>(&mut self, cell: C, mut f: F) -> Listener
    where
        C: CellRef,
        F: FnMut(&C::Value) + 'static,
        M: Accepts<F>,
    {
        self.enter();
        let Some(i) = self.checked(cell.token(), LISTEN) else {
            return Listener::new(None);
        };
        f(self.build.value::<C::Value>(i));
        self.attach(i, f, call_cell::<M, C::Value, F>)
    }

    /// [`listen_cell`](Runtime::listen_cell), returning the error instead of
    /// panicking.
    pub fn try_listen_cell<C, F>(&mut self, cell: C, mut f: F) -> Result<Listener, TokenError>
    where
        C: CellRef,
        F: FnMut(&C::Value) + 'static,
        M: Accepts<F>,
    {
        let i = self.lookup(cell.token())?;
        f(self.build.value::<C::Value>(i));
        Ok(self.attach(i, f, call_cell::<M, C::Value, F>))
    }

    /// Listens to a cell's steps only, with the new value by reference, and
    /// nothing at registration. The I/O form of
    /// [`steps`](crate::Cell::steps), Sodium's `updates`. The cell is a
    /// [`Cell`](crate::Cell) or a [`State`](crate::State), as for
    /// [`listen_cell`](Runtime::listen_cell).
    pub fn listen_steps<C, F>(&mut self, cell: C, f: F) -> Listener
    where
        C: CellRef,
        F: FnMut(&C::Value) + 'static,
        M: Accepts<F>,
    {
        self.enter();
        match self.checked(cell.token(), LISTEN) {
            Some(i) => self.attach(i, f, call_cell::<M, C::Value, F>),
            None => Listener::new(None),
        }
    }

    /// [`listen_steps`](Runtime::listen_steps), returning the error instead of
    /// panicking.
    pub fn try_listen_steps<C, F>(&mut self, cell: C, f: F) -> Result<Listener, TokenError>
    where
        C: CellRef,
        F: FnMut(&C::Value) + 'static,
        M: Accepts<F>,
    {
        let i = self.lookup(cell.token())?;
        Ok(self.attach(i, f, call_cell::<M, C::Value, F>))
    }

    /// Registers a listener on node `i`. Its entry and its guard share a
    /// liveness; dispatch skips an entry whose guard has no owner left, and
    /// then drops it.
    fn attach<F: 'static>(
        &mut self,
        i: u32,
        f: F,
        call: fn(&mut M::Carrier, &mut Build<M>, u32),
    ) -> Listener
    where
        M: Accepts<F>,
    {
        let flag = Liveness::new(&self.released);
        self.attach_flag(i, f, call, flag.clone());
        Listener::new(Some(flag))
    }

    /// Registers a listener on node `i` with the liveness its guard shares.
    fn attach_flag<F: 'static>(
        &mut self,
        i: u32,
        f: F,
        call: fn(&mut M::Carrier, &mut Build<M>, u32),
        flag: Liveness,
    ) where
        M: Accepts<F>,
    {
        let store = &mut self.build.store;
        store.listeners[i as usize].push(Entry {
            flag,
            f: <M as Accepts<F>>::erase(Erase::Value(f)),
            call,
        });
        store.hot[i as usize].flags |= LISTENERS;
    }

    /// Anchors what I/O code wants to hold without listening to it: one of
    /// the three kinds of root. `value` is a token, or any value that holds
    /// tokens, such as the tuple or struct of tokens a
    /// [`construct`](crate::Source::construct) closure made; the anchor roots
    /// every token its [`Trace`] finds, as the build closure's return value
    /// roots its own. The [`Anchored`] it returns carries the value and reads
    /// as it. Dropping the last of its clones removes the root, and
    /// [`keep`](Anchored::keep) keeps it for the graph's life.
    ///
    /// A token a listener hands I/O code as data names a node that nothing
    /// may reach, such as an input a closure built. Anchor it after the
    /// transaction that delivered it and before the next one, when a
    /// collection may run:
    ///
    /// ```
    /// use std::cell::RefCell;
    /// use std::rc::Rc;
    ///
    /// use bough::{Runtime, Source};
    ///
    /// let (mut graph, (open_in, opened)) = Runtime::build(|b| {
    ///     let (open, open_in) = b.input::<u32>();
    ///     let opened = open.construct(b, |b, start| {
    ///         let (bumps, bumps_in) = b.input::<u32>();
    ///         (bumps_in, bumps.accumulate(b, start, |n, c| c + n))
    ///     });
    ///     (open_in, opened)
    /// });
    /// graph.set_collect_after_every_transaction(true); // a test setting
    /// let received = Rc::new(RefCell::new(Vec::new()));
    /// let log = received.clone();
    /// graph.listen(opened, move |counter| log.borrow_mut().push(counter)).keep();
    /// graph.send(open_in, 10); // receive
    /// let counter = received.borrow()[0];
    /// let counter = graph.anchor(counter); // the input and the count
    /// let (bumps_in, count) = *counter;
    /// graph.send(bumps_in, 5); // then use
    /// assert_eq!(*graph.sample(count), 15);
    /// ```
    ///
    /// Panics on a foreign token or a poisoned graph. Anchoring a collected
    /// node is a panic in a debug build and, in a release build, a no-op
    /// that [`stale_operations`](Runtime::stale_operations) counts: the anchor
    /// roots the value's other nodes.
    pub fn anchor<T: Trace>(&mut self, value: T) -> Anchored<T> {
        self.enter();
        let mut tracer = Tracer::new();
        value.trace(&mut tracer);
        // Every token is checked before any is rooted, so a panic leaves no
        // root behind that no handle could remove.
        let nodes: Vec<u32> = tracer
            .visited
            .into_iter()
            .filter_map(|token| self.checked(token, ANCHOR))
            .collect();
        let flag = Liveness::new(&self.released);
        for i in nodes {
            self.anchors.push((i, flag.clone()));
        }
        Anchored::new(value, Anchor::new(Some(flag)))
    }

    /// [`anchor`](Runtime::anchor), returning the error instead of panicking.
    /// A value with a stale or foreign token anchors nothing, and is dropped.
    pub fn try_anchor<T: Trace>(&mut self, value: T) -> Result<Anchored<T>, TokenError> {
        let mut tracer = Tracer::new();
        value.trace(&mut tracer);
        let start = self.anchors.len();
        let flag = Liveness::new(&self.released);
        for token in tracer.visited {
            match self.lookup(token) {
                Ok(i) => self.anchors.push((i, flag.clone())),
                Err(error) => {
                    self.anchors.truncate(start);
                    return Err(error);
                }
            }
        }
        Ok(Anchored::new(value, Anchor::new(Some(flag))))
    }

    /// The cell's current value, by reference. The cell is a
    /// [`Cell`](crate::Cell) or a [`State`](crate::State).
    ///
    /// The graph is borrowed shared, so two samples compose in one
    /// expression; nothing runs, and the value changes only at a commit,
    /// which needs `&mut self`. So a sampled reference cannot be held across
    /// a send:
    ///
    /// ```compile_fail,E0502
    /// use bough::{Runtime, Source};
    ///
    /// let (mut graph, (numbers_in, latest)) = Runtime::build(|b| {
    ///     let (numbers, numbers_in) = b.input::<u32>();
    ///     (numbers_in, numbers.hold(b, 0u32))
    /// });
    /// let before = graph.sample(latest);
    /// graph.send(numbers_in, 1); // error: graph is also borrowed as immutable
    /// assert_eq!(*before, 0);
    /// ```
    pub fn sample<C: CellRef>(&self, cell: C) -> &C::Value {
        self.enter();
        let i = self.build.check(cell.token());
        self.build.value::<C::Value>(i)
    }

    /// [`sample`](Runtime::sample), returning the error instead of panicking.
    pub fn try_sample<C: CellRef>(&self, cell: C) -> Result<&C::Value, TokenError> {
        let i = self.lookup(cell.token())?;
        Ok(self.build.value::<C::Value>(i))
    }

    /// Runs a collection now (RFD 3): frees every node that no root
    /// reaches, whatever the policy. The call the manual policy runs, at
    /// the end of a frame or wherever the driver chooses to pay.
    ///
    /// It marks from the roots, frees what it did not mark, and takes the
    /// freed nodes out of the lists of those that remain. It empties every
    /// stream's slot first: an event nobody consumed is dropped, so a token
    /// in it roots nothing. Nothing is counted, so a cycle, through values
    /// or through a loop, is collected like anything else. A freed node's
    /// slot is reused, oldest first, under a new generation, so every token
    /// naming the old node stays stale. The `Drop` of the values, events and
    /// closures it frees runs here; a panic in one poisons the graph.
    ///
    /// Panics on a poisoned graph.
    pub fn collect_garbage(&mut self) {
        self.enter();
        self.collect_now();
    }

    /// [`collect_garbage`](Runtime::collect_garbage), returning the error
    /// instead of panicking.
    pub fn try_collect_garbage(&mut self) -> Result<(), PoisonedError> {
        if self.poisoned() {
            return Err(PoisonedError);
        }
        self.collect_now();
        Ok(())
    }

    /// Chooses when collection runs. The default is
    /// [`Automatic`](CollectionPolicy::Automatic).
    pub fn set_collection_policy(&mut self, policy: CollectionPolicy) {
        self.policy = policy;
    }

    /// With `true`, collects as every transaction opens, whatever the
    /// policy, so that a closure capture that should have been declared
    /// with [`depends`](crate::Build::depends), or a token I/O code kept
    /// without anchoring it, is a stale-token error the first time the code
    /// runs rather than whenever the automatic policy happens to collect. A
    /// test setting: a transaction then costs a collection.
    ///
    /// The collection runs when the next transaction opens, which is after
    /// every transaction by the time another one runs, and never between a
    /// transaction's listeners and the I/O code they report to.
    pub fn set_collect_after_every_transaction(&mut self, enabled: bool) {
        self.stress = enabled;
    }

    /// The number of live nodes: how the no-leak requirement is asserted.
    /// Build, drive, drop the handles, collect, and compare.
    ///
    /// Every materializer creates one node, however long its chain;
    /// `input_cell` creates two, the input and the hold over it, and so do
    /// [`split`](crate::Source::split) and [`defer`](crate::Source::defer):
    /// the node that takes each event, and the one that emits it, or its
    /// elements, in the child transactions. A cell or state loop's forward
    /// is a node of its own besides its definition; a stream loop's forward
    /// is the one node its definition's chain is fused into.
    pub fn live_nodes(&self) -> usize {
        self.build.store.live
    }

    /// RFD 1's order shuffle: with a seed, each transaction evaluates
    /// independent nodes and dispatches listeners in an order drawn from
    /// the seed, so a test can show that nothing depends on either. `None`,
    /// the default, restores the plain order, which costs nothing.
    ///
    /// Values and each node's events are the same under every seed; only
    /// the interleaving of different nodes' listeners moves.
    pub fn set_shuffle_seed(&mut self, seed: Option<u64>) {
        self.build.s.shuffle = seed;
    }

    /// The per-phase counters since the graph was built: instants,
    /// evaluations, out-of-order pulls, commits, listener calls.
    #[cfg(feature = "statistics")]
    pub fn statistics(&self) -> Statistics {
        self.build.s.statistics
    }

    /// How many operations on collected nodes were dropped in release
    /// builds: sends to a collected input, listeners on a collected node
    /// and anchors on one, which a debug build panics on instead. Zero in a
    /// debug build.
    pub fn stale_operations(&self) -> u64 {
        self.stale_operations
    }

    /// Runs every pending input slot as a transaction of its own, in
    /// connection order, then every queued remote unit as one transaction
    /// each, in arrival order (RFD 6, RFD 7). Two slots are never
    /// simultaneous; a unit is exactly as simultaneous as its sends.
    ///
    /// Called by the driver from wherever it sits: a thread the waker wakes,
    /// a future's `poll`, or a bare-metal main loop. Latency is the distance
    /// from a send to the next pump. A slot written while the pump runs, by
    /// a listener or by another thread, is drained now if its turn has not
    /// come and at the next pump otherwise. The units are those queued when
    /// the pump reaches them; one queued later, by a listener feeding back
    /// or by another thread, waits for the next pump, whose wake it has
    /// already made, so a listener that always sends cannot keep a pump
    /// from returning. A collection that is due runs before each
    /// transaction opens, as for [`send`](Runtime::send).
    ///
    /// A unit runs as a transaction the driver opens, and its closure sends
    /// into it. A unit whose send fails is dropped whole, with none of its
    /// sends run, and the graph stays usable: the transaction closes
    /// without running.
    ///
    /// Panics on a poisoned graph; a panic in a unit's closure or in the
    /// transaction it runs poisons it. A send to an input collected before
    /// the pump, from a unit or a slot, is a send to a collected input: a
    /// panic in a debug build, and in a release build a no-op that
    /// [`stale_operations`](Runtime::stale_operations) counts, and the unit's
    /// other sends run. A stale slot is disconnected and its event dropped.
    /// A double send inside a unit, or a remote transaction's token from
    /// another graph, panics in both builds. A panic leaves the rest pending
    /// for the next pump.
    pub fn pump(&mut self) {
        self.pump_between(&mut |_| {});
    }

    /// [`pump`](Runtime::pump), running `between` after each slot's and each
    /// unit's transaction, before the next one opens and before the
    /// collection that may run first. The same-thread handle runs its
    /// queue there, so that a listener can wire what one unit built before
    /// the next unit's collection.
    pub(crate) fn pump_between(&mut self, between: &mut dyn FnMut(&mut Self)) {
        self.enter();
        if let Err(error) = self.pump_all(!cfg!(debug_assertions), between) {
            match error {
                PumpError::Stale => self.stale_operation(SEND),
                PumpError::DoubleSend => {
                    panic!("bough: a second send to a non-coalescing input in one remote unit")
                }
                PumpError::ForeignGraph => panic!("bough: a token from another graph"),
                PumpError::Poisoned => unreachable!("bough engine: pump checks the poison first"),
            }
        }
    }

    /// [`pump`](Runtime::pump), returning the error instead of panicking. The
    /// first slot or unit that fails is dropped, and the error returned;
    /// the rest stay pending for the next call.
    pub fn try_pump(&mut self) -> Result<(), PumpError> {
        if self.poisoned() {
            return Err(PumpError::Poisoned);
        }
        self.pump_all(false, &mut |_| {})
    }

    /// The slots, then the units. With `skip_stale`, the panicking pump's
    /// release build, a stale send is counted and skipped rather than
    /// returned.
    fn pump_all(
        &mut self,
        skip_stale: bool,
        between: &mut dyn FnMut(&mut Self),
    ) -> Result<(), PumpError> {
        // Without a lock there are no slots and no units to pump.
        let _ = (skip_stale, &between);
        #[cfg(any(feature = "std", feature = "critical-section"))]
        self.pump_slots(skip_stale, between)?;
        #[cfg(all(
            target_has_atomic = "ptr",
            any(feature = "std", feature = "critical-section")
        ))]
        self.pump_units(skip_stale, between)?;
        Ok(())
    }

    /// The units queued when the pump reached them, in arrival order, each
    /// as one transaction. A unit a listener queues runs at the next pump,
    /// so a listener that always sends cannot keep one pump from returning.
    #[cfg(all(
        target_has_atomic = "ptr",
        any(feature = "std", feature = "critical-section")
    ))]
    fn pump_units(
        &mut self,
        skip_stale: bool,
        between: &mut dyn FnMut(&mut Self),
    ) -> Result<(), PumpError> {
        let queued = self.build.edge.inbox.len();
        for _ in 0..queued {
            let Some(unit) = self.build.edge.inbox.pop() else {
                break;
            };
            self.collect_if_due();
            self.build.begin();
            let mut tx = RemoteTransaction {
                build: &mut self.build,
                skip_stale,
                skipped: 0,
                fault: None,
            };
            unit(&mut tx);
            let RemoteTransaction { skipped, fault, .. } = tx;
            self.stale_operations += skipped;
            if let Some(fault) = fault {
                self.build.cancel();
                return Err(match fault {
                    Fault::Stale => PumpError::Stale,
                    Fault::ForeignGraph => PumpError::ForeignGraph,
                    Fault::DoubleSend => PumpError::DoubleSend,
                });
            }
            self.build.finish();
            between(self);
        }
        Ok(())
    }

    /// Each pending slot, in connection order, as a transaction of its own.
    /// The event leaves the slot under its lock, and the transaction runs
    /// after the lock is released.
    #[cfg(any(feature = "std", feature = "critical-section"))]
    fn pump_slots(
        &mut self,
        skip_stale: bool,
        between: &mut dyn FnMut(&mut Self),
    ) -> Result<(), PumpError> {
        let mut k = 0;
        while k < self.build.edge.slots.len() {
            let Connection { input, slot } = self.build.edge.slots[k];
            let mut live = true;
            slot.drain(&mut |event| {
                self.collect_if_due();
                match self.build.lookup(input) {
                    Ok(i) => {
                        self.build.begin();
                        let fire = self.build.store.ops[i as usize].fire;
                        fire(&mut self.build, i, event)
                            .expect("bough engine: a slot's event is its transaction's only send");
                        self.build.finish();
                        between(self);
                    }
                    // `connect` checked the graph, so the input was collected.
                    Err(_) => live = false,
                }
            });
            if live {
                k += 1;
                continue;
            }
            self.build.edge.slots.remove(k);
            slot.disconnect();
            if !skip_stale {
                return Err(PumpError::Stale);
            }
            self.stale_operations += 1;
        }
        Ok(())
    }

    /// Registers the waker that a slot write or a remote send wakes, so the
    /// driver knows to pump: it reaches every connected slot, and a slot
    /// connected later.
    ///
    /// A driver that is a future stores `cx.waker().clone()` on each poll and
    /// returns pending after pumping; a thread driver builds one from an
    /// `Arc` through `alloc::task::Wake`; a bare-metal main loop that sleeps
    /// on the interrupt itself gives `Waker::noop()`. A waker that would
    /// wake the same task as the one registered changes nothing.
    pub fn set_waker(&mut self, waker: Waker) {
        // Without a lock nothing can wake the driver, so it keeps no waker.
        #[cfg(not(any(feature = "std", feature = "critical-section")))]
        drop(waker);
        #[cfg(any(feature = "std", feature = "critical-section"))]
        {
            let edge = &mut self.build.edge;
            if edge.waker.as_ref().is_some_and(|w| w.will_wake(&waker)) {
                return;
            }
            for connection in &edge.slots {
                connection.slot.set_waker(Some(waker.clone()));
            }
            #[cfg(target_has_atomic = "ptr")]
            edge.inbox.set_waker(waker.clone());
            edge.waker = Some(waker);
        }
    }

    /// An endpoint any thread uses to send into this graph: each send, or
    /// each remote transaction, is queued as one unit, which the driver runs
    /// as one transaction at its next [`pump`](Runtime::pump) (RFD 6).
    ///
    /// Every remote of a graph shares its one inbox, made with the graph.
    /// Panics on a poisoned graph.
    #[cfg(all(
        target_has_atomic = "ptr",
        any(feature = "std", feature = "critical-section")
    ))]
    pub fn remote(&self) -> Remote {
        self.enter();
        Remote {
            inbox: self.build.edge.inbox.clone(),
        }
    }

    /// [`remote`](Runtime::remote), returning the error instead of panicking.
    #[cfg(all(
        target_has_atomic = "ptr",
        any(feature = "std", feature = "critical-section")
    ))]
    pub fn try_remote(&self) -> Result<Remote, PoisonedError> {
        if self.poisoned() {
            return Err(PoisonedError);
        }
        Ok(Remote {
            inbox: self.build.edge.inbox.clone(),
        })
    }
}

/// When collection runs. It never runs inside a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionPolicy {
    /// Amortized: as a transaction opens, when the nodes allocated plus the
    /// handles released since the last collection exceed the number of
    /// nodes that collection left alive. Garbage is made by unrooting as
    /// much as by allocating, so a graph that only drops listeners still
    /// collects, and a graph that neither allocates nor drops a handle
    /// never pays. The first transaction after the build collects what the
    /// build closure built and did not root.
    Automatic,
    /// Only on [`Runtime::collect_garbage`]: for a frame loop that collects at
    /// the end of a frame, or a high-rate loop that chooses when it pays.
    Manual,
}

/// Several sends in one instant; the only I/O-side use of the word.
pub struct Transaction<'g, M: Mode> {
    graph: &'g mut Runtime<M>,
}

impl<M: Mode> Transaction<'_, M> {
    /// Sends one value in this transaction.
    ///
    /// Panics on a foreign token and on a second send to a non-coalescing
    /// input. The panic escapes the transaction, so it poisons the graph.
    /// A send to a collected input is unobservable by the semantics: a
    /// panic in a debug build, which poisons the graph too, and in a release
    /// build a no-op that
    /// [`Runtime::stale_operations`](Runtime::stale_operations) counts.
    pub fn send<A: 'static>(&mut self, input: Input<A>, value: A)
    where
        M: Accepts<A>,
    {
        match self.try_send(input, value) {
            Ok(()) => {}
            Err(TransactionSendError::DoubleSend) => {
                panic!("bough: a second send to a non-coalescing input in one transaction")
            }
            Err(TransactionSendError::ForeignGraph) => panic!("bough: a token from another graph"),
            Err(TransactionSendError::Stale) => self.graph.stale_operation(SEND),
        }
    }

    /// [`send`](Transaction::send), returning the error instead of panicking.
    /// After an error the transaction goes on without the refused value.
    pub fn try_send<A: 'static>(
        &mut self,
        input: Input<A>,
        value: A,
    ) -> Result<(), TransactionSendError>
    where
        M: Accepts<A>,
    {
        let build = &mut self.graph.build;
        let i = build.lookup(input.token).map_err(|fault| match fault {
            TokenFault::Foreign => TransactionSendError::ForeignGraph,
            TokenFault::Stale => TransactionSendError::Stale,
        })?;
        build
            .fire_start(i, value)
            .map_err(|_| TransactionSendError::DoubleSend)
    }
}

/// A listener handle. Dropping it unlistens; it borrows nothing from the
/// graph and shares a count of its owners with its node's entry instead, so
/// it can be dropped inside a listener, and on any thread where the target
/// has pointer atomics. Every mode returns the same type.
///
/// A live listener is a root: its node, and everything the node reaches,
/// stays alive. Dropping the last one lets collection free them; a linear
/// stream is consumed by `listen`, so once its listener is dropped nothing
/// can observe it again. The drop counts as a released root for the
/// automatic policy, through the shared count, with no graph access.
pub struct Listener {
    /// The handle's share of the state its node's entry holds; `None` once
    /// kept.
    alive: Option<Liveness>,
}

impl Listener {
    fn new(alive: Option<Liveness>) -> Self {
        Listener { alive }
    }

    /// Stops listening now, the same as dropping the handle.
    pub fn unlisten(self) {
        drop(self);
    }

    /// Keeps listening for the life of the graph, without a handle to hold.
    /// The handle gives up its share without releasing it, so the count
    /// of its owners never reaches zero.
    pub fn keep(mut self) {
        self.alive = None;
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        if let Some(flag) = self.alive.take() {
            flag.release();
        }
    }
}

/// The handle that keeps a node alive from I/O code without listening to
/// it, one of the three kinds of root, from [`Runtime::anchor`]. Dropping it
/// removes the root, and the node is collected at a later collection if
/// nothing else reaches it. It borrows nothing from the graph, and shares a
/// count of its owners with its entries, as a [`Listener`] does.
///
/// Not `Pin`, which is an unrelated concept in `std::pin`, and not `Root`,
/// which is the concept this is one kind of.
pub struct Anchor {
    /// The handle's share of the state its entries hold; `None` once kept.
    alive: Option<Liveness>,
}

impl Anchor {
    fn new(alive: Option<Liveness>) -> Self {
        Anchor { alive }
    }

    /// Another owner of the same root.
    fn add_owner(&self) -> Self {
        Anchor::new(self.alive.as_ref().map(Liveness::add_owner))
    }

    /// Removes the root now, the same as dropping the handle.
    pub fn unanchor(self) {
        drop(self);
    }

    /// Keeps the root for the life of the graph, without a handle to hold.
    /// The handle gives up its share without releasing it, so the count
    /// of its owners never reaches zero.
    pub fn keep(mut self) {
        self.alive = None;
    }
}

impl Drop for Anchor {
    fn drop(&mut self) {
        if let Some(flag) = self.alive.take() {
            flag.release();
        }
    }
}

/// A value I/O code holds, with the [`Anchor`] that roots the tokens it
/// holds, from [`Runtime::anchor`]. It reads as the value, through `Deref`.
/// Its clones share one root, which goes when the last of them drops.
/// [`into_parts`](Anchored::into_parts) splits it into the value and the
/// anchor, and [`keep`](Anchored::keep) keeps the root for the graph's life
/// and returns the value.
///
/// It isn't [`Trace`], so graph state can't hold one: a root inside graph
/// state could keep itself alive through a cycle. A stream can carry one to
/// the edge, but a hold of one doesn't compile:
///
/// ```compile_fail,E0277
/// use bough::{Anchored, Input, Runtime, Source};
///
/// let (_graph, _) = Runtime::build(|b| {
///     let (rows, _rows_in) = b.input::<Anchored<Input<u32>>>();
///     let _latest = rows.map(Some).hold(b, None); // error: Anchored is not Trace
/// });
/// ```
pub struct Anchored<T> {
    value: T,
    anchor: Anchor,
}

impl<T> Anchored<T> {
    pub(crate) fn new(value: T, anchor: Anchor) -> Self {
        Anchored { value, anchor }
    }

    /// The value, and the anchor that keeps its root.
    pub fn into_parts(self) -> (T, Anchor) {
        (self.value, self.anchor)
    }

    /// Keeps the root for the life of the graph, without an anchor to hold,
    /// and returns the value.
    pub fn keep(self) -> T {
        self.anchor.keep();
        self.value
    }
}

impl<T> Deref for Anchored<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.value
    }
}

/// Another owner of the same root.
impl<T: Clone> Clone for Anchored<T> {
    fn clone(&self) -> Self {
        Anchored::new(self.value.clone(), self.anchor.add_owner())
    }
}

/// A `Send + Clone` endpoint for sending into a graph from any thread
/// (RFD 6).
///
/// A send pushes a unit into the graph's inbox and wakes the driver; it
/// never blocks on the graph, and allocates on the sending thread. Each
/// unit is one transaction, run by [`Runtime::pump`] in arrival order: a
/// [`transaction`](Remote::transaction) makes its sends simultaneous, and
/// two units are never merged. A remote works with a `Local` graph; what it
/// carries must be `Send`. The graph's poison is mirrored into the inbox by
/// the first entry that finds it, so sends fail from then on, and a
/// dropped graph closes it. Dropping a remote does nothing.
///
/// ```
/// use std::cell::RefCell;
/// use std::rc::Rc;
/// use std::thread;
///
/// use bough::{Runtime, Source};
///
/// let (mut graph, (numbers_in, total)) = Runtime::build(|b| {
///     let (numbers, numbers_in) = b.input::<u32>();
///     (numbers_in, numbers.accumulate(b, 0u32, |n, t| t + n))
/// });
/// let heard = Rc::new(RefCell::new(Vec::new())); // a Local graph keeps its Rc
/// let sink = heard.clone();
/// graph.listen_steps(total, move |t| sink.borrow_mut().push(*t)).keep();
///
/// let remote = graph.remote();
/// thread::spawn(move || {
///     remote.send(numbers_in, 1); // one unit
///     remote.transaction(move |tx| tx.send(numbers_in, 2)); // another
/// })
/// .join()
/// .unwrap();
///
/// graph.pump(); // the driver: each unit is one transaction
/// assert_eq!(*heard.borrow(), [1, 3]);
/// ```
///
/// What a remote carries crosses threads, so it must be `Send`:
///
/// ```compile_fail,E0277
/// use std::rc::Rc;
///
/// use bough::Runtime;
///
/// let (graph, shared_in) = Runtime::build(|b| b.input::<Rc<u32>>().1);
/// graph.remote().send(shared_in, Rc::new(1)); // error: Rc is not Send
/// ```
///
/// A remote send from graph code is an error in both builds,
/// `InsideTransaction`: I/O from inside FRP logic, which a closure can do
/// because a remote is `Send + Clone + 'static`. The guard is a thread
/// token the driver stores in the inbox while its graph code runs:
/// evaluation and commit, `accumulate_mut`'s function, a `construct`
/// closure, a split's iterator, in every child instant too. It is cleared
/// before the listeners run, so a listener's remote send queues a later
/// transaction, the sanctioned way for I/O to feed back; so does the
/// closure of a transaction, which is I/O code, and a send from any other
/// thread. It needs a thread id and exists under `std`; on bare metal it
/// is documented and unchecked (RFD 7). It knows its own graph only: graph
/// code sending through another graph's remote queues there. A panic that
/// escapes graph code leaves the token behind, so until an entry finds the
/// poison, a remote send from that thread reports `InsideTransaction`.
///
/// `Remote` holds an `Arc` and its inbox a lock, so it exists where the
/// target has pointer atomics and there is a lock: under `std`, or with the
/// `critical-section` feature. On a Cortex-M0 the path from an interrupt
/// handler into the graph is an [`InputSlot`](crate::InputSlot). On wasm32
/// it exists and is the path for DOM callbacks, which may run inside a
/// transaction's listener and so must not send directly.
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
#[derive(Clone)]
pub struct Remote {
    inbox: Arc<Inbox>,
}

#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
impl Remote {
    /// Queues one value as a unit of its own, and wakes the driver.
    ///
    /// Panics on a token from another graph, on a poisoned graph, and on the
    /// driver thread inside a transaction. A send to a dropped graph cannot
    /// be observed: a panic in a debug build and a no-op in a release
    /// build. Whether the input is collected, or was by the time the driver
    /// pumps, is graph knowledge, found at [`Runtime::pump`].
    pub fn send<A: Send + 'static>(&self, input: Input<A>, value: A) {
        match self.try_send(input, value) {
            Ok(()) => {}
            Err(RemoteSendError::ForeignGraph) => panic!("bough: a token from another graph"),
            Err(RemoteSendError::InsideTransaction) => inside_transaction(),
            Err(RemoteSendError::Poisoned) => panic!("{POISONED}"),
            Err(RemoteSendError::GraphDropped) => dropped_graph(),
        }
    }

    /// [`send`](Remote::send), returning the error instead of panicking.
    pub fn try_send<A: Send + 'static>(
        &self,
        input: Input<A>,
        value: A,
    ) -> Result<(), RemoteSendError> {
        if self.inbox.is_poisoned() {
            return Err(RemoteSendError::Poisoned);
        }
        if input.token.graph != self.inbox.graph {
            return Err(RemoteSendError::ForeignGraph);
        }
        if self.inbox.inside() {
            return Err(RemoteSendError::InsideTransaction);
        }
        self.inbox
            .push(Box::new(move |tx: &mut RemoteTransaction<'_>| {
                tx.send(input, value)
            }))
            .map_err(|_| RemoteSendError::GraphDropped)
    }

    /// Queues several sends as one unit, and so one transaction: `f` runs
    /// on the driver, at its next pump, with a [`RemoteTransaction`] whose
    /// sends are simultaneous. The closure is I/O code: it has no graph
    /// access, and its order of sends does not matter.
    ///
    /// Panics on a poisoned graph and on the driver thread inside a
    /// transaction; a dropped graph is a panic in a debug build and a no-op
    /// in a release build.
    pub fn transaction<F>(&self, f: F)
    where
        F: FnOnce(&mut RemoteTransaction<'_>) + Send + 'static,
    {
        match self.try_transaction(f) {
            Ok(()) => {}
            Err(RemoteTransactionError::InsideTransaction) => inside_transaction(),
            Err(RemoteTransactionError::Poisoned) => panic!("{POISONED}"),
            Err(RemoteTransactionError::GraphDropped) => dropped_graph(),
        }
    }

    /// [`transaction`](Remote::transaction), returning the error instead of
    /// panicking.
    pub fn try_transaction<F>(&self, f: F) -> Result<(), RemoteTransactionError>
    where
        F: FnOnce(&mut RemoteTransaction<'_>) + Send + 'static,
    {
        if self.inbox.is_poisoned() {
            return Err(RemoteTransactionError::Poisoned);
        }
        if self.inbox.inside() {
            return Err(RemoteTransactionError::InsideTransaction);
        }
        self.inbox
            .push(Box::new(f))
            .map_err(|_| RemoteTransactionError::GraphDropped)
    }
}

/// The panic of a remote send from graph code.
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
fn inside_transaction() {
    panic!(
        "bough: a remote send from graph code, on the driver thread while a transaction \
         runs: send from I/O code, such as a listener, which queues a later transaction"
    )
}

/// A remote send to a dropped graph: nothing can observe it, so it follows
/// the rule for a send to a collected input, a debug-build panic and a
/// release no-op, with no graph left to count it.
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
fn dropped_graph() {
    if cfg!(debug_assertions) {
        panic!(
            "bough: a remote send to a dropped graph: nothing drains its inbox. In a release \
             build this is a no-op"
        );
    }
}

/// The sends of one remote unit, run on the driver inside the transaction
/// it opened for the unit, so they are simultaneous.
///
/// Whether an input is collected or coalesces is graph knowledge, so a
/// failed send here is found at [`Runtime::pump`], which drops the whole unit
/// and reports it; the closure's later sends are ignored.
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
pub struct RemoteTransaction<'a> {
    /// The driver's build context, with the mode out of sight.
    build: &'a mut dyn Start,
    /// The panicking pump's release build: a stale send is skipped and
    /// counted rather than a fault.
    skip_stale: bool,
    skipped: u64,
    /// The first send that failed; the unit is dropped.
    fault: Option<Fault>,
}

#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
impl RemoteTransaction<'_> {
    /// Sends one value in this unit.
    pub fn send<A: Send + 'static>(&mut self, input: Input<A>, value: A) {
        if self.fault.is_some() {
            return;
        }
        let mut event = Some(value);
        match self.build.start(input.token, &mut event) {
            Ok(()) => {}
            Err(Fault::Stale) if self.skip_stale => self.skipped += 1,
            Err(fault) => self.fault = Some(fault),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::engine::slot;
    use crate::{Local, Runtime, Source};

    /// Claim 1, read off the data plane: a linear consumer moves the event
    /// out of its dependency's slot, and a shared slot keeps its event for
    /// every consumer.
    #[test]
    fn a_linear_consumer_empties_the_slot_and_a_shared_slot_keeps_its_event() {
        // The consumers are returned, so that they are roots: a consumer no
        // root reaches is collected at the first send, and takes nothing.
        let (mut graph, (linear_in, shared_in, shared, _consumers)) = Runtime::build(|b| {
            let (linear, linear_in) = b.input::<u32>();
            let latest = linear.hold(b, 0u32);
            let (events, shared_in) = b.input::<u32>();
            let shared = events.share(b);
            let plus = shared.map(|x| x + 1).hold(b, 0u32);
            let same = shared.hold(b, 0u32);
            (linear_in, shared_in, shared, [latest, plus, same])
        });
        let data = |graph: &Runtime, index: u32| {
            *slot::<Local, u32>(&graph.build.store.data[index as usize])
        };
        graph.send(linear_in, 5);
        assert_eq!(data(&graph, linear_in.token.index), None);
        graph.send(shared_in, 7);
        // The shared node took its input's event, since its own chain reads
        // a linear stream, and both of its consumers cloned from it.
        assert_eq!(data(&graph, shared_in.token.index), None);
        assert_eq!(data(&graph, shared.token.index), Some(7));
    }
}
