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
use crate::engine::edge::{Fault, Start};
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
use crate::engine::edge::{Inbox, RemoteCall, Unit};
use crate::engine::{Cx, Entry, LISTENERS, TokenFault, part};
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
use crate::error::IoError;
use crate::error::{PoisonedError, PumpError, SendError, TokenError, TransactionSendError};
use crate::guard::{Liveness, before};
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
use crate::io::{AnchorTokens, Listen, ListenCell, Registration, Roots, Waiting};
use crate::io::{Io, IoQueue};
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
/// reaches it (RFD 3). There are two kinds of root: a live guard, a
/// [`Listener`] or an [`Anchor`], which an [`Anchored`] holds; and a
/// registration waiting in a handle's queue, which keeps the tokens it
/// names until the pump runs it. What the build closure returned comes back anchored, like
/// anything else.
/// A node reaches what it depends on, the tokens in a stateful cell's
/// committed value (found through [`Trace`]), a switch's current inner,
/// the cells a chain snapshots or gates on, and what
/// [`Build::depends`](crate::Build::depends) declares. Collection frees
/// every node no root reaches, and a token naming a freed node is stale:
/// its next use is an error, never a read of another node. So a token that
/// leaves a transaction as data lives only while the graph reaches it or it
/// left anchored: a construct that sends a row out anchors it with
/// [`Build::anchor`](crate::Build::anchor). Collection is automatic by
/// default, runs after each whole unit, and never inside a transaction; see
/// [`CollectionPolicy`].
pub struct Runtime<M: Mode = Local> {
    build: Build<M>,
    /// The build context's count of released guards as the last
    /// collection found it.
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
fn build_graph<M: Mode, R: Trace>(f: impl FnOnce(&mut Build<M>) -> R) -> (Runtime<M>, Anchored<R>) {
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
    let edge = build.anchor(r);
    let mut graph = Runtime {
        build,
        released_before: 0,
        baseline: 0,
        policy: CollectionPolicy::Automatic,
        stress: false,
        stale_operations: 0,
    };
    // Transaction zero is a unit like any other: a collection that is due
    // runs after it, and frees what the build made that nothing reaches.
    graph.collect_if_due();
    (graph, edge)
}

impl Runtime<Local> {
    /// Builds a `Local` graph. The closure gets the only [`Build`] context;
    /// whatever it returns is the edge of the graph, which comes back
    /// [`Anchored`], so `R: Trace`. [`keep`](Anchored::keep) it to hold the
    /// edge for the graph's life, or drop it to let collection free what
    /// only the edge reached.
    ///
    /// The build closure runs as transaction zero. Its child transactions,
    /// which a [`split`](crate::Source::split) or a
    /// [`defer`](crate::Source::defer) that fires in it starts, run before
    /// `build` returns, and so does the collection that is due after it, as
    /// after any unit: what the build made that nothing reaches is gone
    /// before I/O code sees the runtime.
    pub fn build<R: Trace>(
        f: impl FnOnce(&mut Build<Local>) -> R,
    ) -> (Runtime<Local>, Anchored<R>) {
        build_graph(f)
    }

    /// A handle for I/O code that can't hold the runtime, such as a GTK
    /// signal handler: every call through it queues for the next
    /// [`pump`](Runtime::pump). Every `Io` of a runtime shares its one
    /// queue, made with the runtime, so this takes `&self`.
    pub fn io(&self) -> Io {
        Io::new(&self.build.io)
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
    ) -> (Runtime<Threaded>, Anchored<R>) {
        build_graph(f)
    }
}

const POISONED: &str = "bough: the graph is poisoned: a panic escaped an earlier transaction";

/// Why a pump stopped: the error [`Runtime::try_pump`] returns, and what the
/// refused operation was, for the panic [`Runtime::pump`] makes of a stale
/// token.
pub(crate) type Stop = (PumpError, &'static str);

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
    /// one that finds it set mirrors it into the inbox and the `Io`s'
    /// queue, so that remote sends and `Io` calls fail from then on (RFD 6).
    pub(crate) fn poisoned(&self) -> bool {
        let poisoned = self.build.in_tx;
        if poisoned {
            #[cfg(all(
                target_has_atomic = "ptr",
                any(feature = "std", feature = "critical-section")
            ))]
            self.build.edge.inbox.poison();
            self.build.io.poison();
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
        self.build
            .released
            .count()
            .wrapping_sub(self.released_before)
    }

    /// Runs a collection if one is due, after a whole unit, its children
    /// included: under the automatic policy when the nodes allocated and the
    /// guards released since the last collection exceed the live count it
    /// left, and always under the stress setting. After the unit rather than
    /// before the next one, so that what a unit made and nothing anchored is
    /// gone by the time I/O code sees it (RFD 3: anchor it at the edge).
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
        let released = self.build.released.count();
        self.build.collect();
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
    /// A collection that is due runs after it, once its child transactions
    /// have run.
    pub fn send<A: 'static>(&mut self, input: Input<A>, value: A)
    where
        M: Accepts<A>,
    {
        self.enter();
        // The token is checked before the transaction opens, so a foreign
        // token is a panic that leaves the graph usable.
        if let Some(i) = self.checked(input.token, SEND) {
            self.build.begin();
            self.build
                .fire_start(i, value)
                .expect("bough engine: the only send of a transaction is not a double send");
            self.build.finish();
        }
        self.collect_if_due();
    }

    /// [`send`](Runtime::send), returning the error instead of panicking.
    pub fn try_send<A: 'static>(&mut self, input: Input<A>, value: A) -> Result<(), SendError>
    where
        M: Accepts<A>,
    {
        if self.poisoned() {
            return Err(SendError::Poisoned);
        }
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
        self.collect_if_due();
        Ok(())
    }

    /// Several sends in one instant.
    ///
    /// The sends are simultaneous: nothing runs until `f` returns, so their
    /// order inside `f` does not matter. As with [`send`](Runtime::send), the
    /// transaction's listeners and its child transactions run before it
    /// returns. A panic inside `f`, including one from
    /// [`Transaction::send`], escapes the transaction and poisons the graph.
    /// A collection that is due runs after it, once its child transactions
    /// have run.
    pub fn transaction<R>(&mut self, f: impl FnOnce(&mut Transaction<'_, M>) -> R) -> R {
        self.enter();
        self.build.begin();
        let r = f(&mut Transaction { graph: self });
        self.build.finish();
        self.collect_if_due();
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
    /// let (mut graph, edge) = Runtime::build(|b| b.input::<u32>().0);
    /// let events = edge.keep();
    /// let _l = graph.listen(events.map(|n| n + 1), |n| println!("{n}")); // error: Map<..> is not a Node
    /// ```
    ///
    /// A linear stream cannot be listened to twice:
    ///
    /// ```compile_fail,E0382
    /// use bough::{Runtime, Source};
    ///
    /// let (mut graph, edge) = Runtime::build(|b| b.input::<u32>().0);
    /// let events = edge.keep();
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
        let flag = Liveness::new(&self.build.released);
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

    /// The node a call an `Io` queued names, looked up at the pump. A
    /// foreign token stops the pump, and so does a stale one, unless
    /// `skip_stale`, which counts it and skips the call.
    fn queued_lookup(
        &mut self,
        skip_stale: bool,
        token: Token,
        what: &'static str,
    ) -> Result<Option<u32>, Stop> {
        match self.build.lookup(token) {
            Ok(i) => Ok(Some(i)),
            Err(TokenFault::Foreign) => Err((PumpError::ForeignGraph, what)),
            Err(TokenFault::Stale) if skip_stale => {
                self.stale_operations += 1;
                Ok(None)
            }
            Err(TokenFault::Stale) => Err((PumpError::Stale, what)),
        }
    }

    /// [`listen`](Runtime::listen), as an `Io` asked for it, at the pump:
    /// the entry shares the liveness of the guard the `Io` handed out, and
    /// isn't made if that guard has gone.
    pub(crate) fn listen_queued<S, F>(
        &mut self,
        skip_stale: bool,
        flag: Liveness,
        source: S,
        f: F,
    ) -> Result<(), Stop>
    where
        S: Node,
        S::Event: 'static,
        F: FnMut(S::Event) + 'static,
        M: Accepts<F>,
    {
        if !flag.is_live() {
            return Ok(());
        }
        if let Some(i) = self.queued_lookup(skip_stale, source.node_token(), LISTEN)? {
            self.attach_flag(i, f, call_stream::<M, S, F>, flag);
        }
        Ok(())
    }

    /// [`listen_cell`](Runtime::listen_cell), as an `Io` asked for it, at
    /// the pump, as [`listen_queued`](Runtime::listen_queued) says. The
    /// first call runs here, with the value at the pump.
    pub(crate) fn listen_cell_queued<C, F>(
        &mut self,
        skip_stale: bool,
        flag: Liveness,
        cell: C,
        mut f: F,
    ) -> Result<(), Stop>
    where
        C: CellRef,
        F: FnMut(&C::Value) + 'static,
        M: Accepts<F>,
    {
        if !flag.is_live() {
            return Ok(());
        }
        if let Some(i) = self.queued_lookup(skip_stale, cell.token(), LISTEN)? {
            f(self.build.value::<C::Value>(i));
            self.attach_flag(i, f, call_cell::<M, C::Value, F>, flag);
        }
        Ok(())
    }

    /// [`listen_steps`](Runtime::listen_steps), as an `Io` asked for it, at
    /// the pump, as [`listen_queued`](Runtime::listen_queued) says.
    pub(crate) fn listen_steps_queued<C, F>(
        &mut self,
        skip_stale: bool,
        flag: Liveness,
        cell: C,
        f: F,
    ) -> Result<(), Stop>
    where
        C: CellRef,
        F: FnMut(&C::Value) + 'static,
        M: Accepts<F>,
    {
        if !flag.is_live() {
            return Ok(());
        }
        if let Some(i) = self.queued_lookup(skip_stale, cell.token(), LISTEN)? {
            self.attach_flag(i, f, call_cell::<M, C::Value, F>, flag);
        }
        Ok(())
    }

    /// [`anchor`](Runtime::anchor), as an `Io` asked for it, at the pump,
    /// for the tokens its value's [`Trace`] found when it was asked for.
    /// Every token is checked before any is rooted, so a token that stops
    /// the pump anchors nothing; a stale one that `skip_stale` skips leaves
    /// the rest rooted, as a release build's `anchor` does.
    pub(crate) fn anchor_queued(
        &mut self,
        skip_stale: bool,
        flag: Liveness,
        tokens: Vec<Token>,
    ) -> Result<(), Stop> {
        if !flag.is_live() {
            return Ok(());
        }
        let mut nodes = Vec::with_capacity(tokens.len());
        for token in tokens {
            if let Some(i) = self.queued_lookup(skip_stale, token, ANCHOR)? {
                nodes.push(i);
            }
        }
        for i in nodes {
            self.build.anchors.push((i, flag.clone()));
        }
        Ok(())
    }

    /// Anchors what I/O code wants to hold without listening to it: one of
    /// the two kinds of root. `value` is a token, or any value that holds
    /// tokens, such as the tuple or struct of tokens a
    /// [`construct`](crate::Source::construct) closure made; the anchor roots
    /// every token its [`Trace`] finds, as [`build`](Runtime::build) does
    /// for the build closure's return value. The [`Anchored`] it returns carries the value and reads
    /// as it. Dropping the last of its clones removes the root, and
    /// [`keep`](Anchored::keep) keeps it for the graph's life.
    ///
    /// The build's edge is anchored as a whole. To let part of it go and keep
    /// the rest, anchor the part to keep, then drop the edge:
    ///
    /// ```
    /// use bough::{Runtime, Source, TokenError};
    ///
    /// let (mut graph, edge) = Runtime::build(|b| {
    ///     let (n, n_in) = b.input::<u32>();
    ///     let n = n.share(b);
    ///     (n_in, n.hold(b, 0u32), n.map(|v| v * 2).hold(b, 0u32))
    /// });
    /// let (n_in, latest, doubled) = *edge;
    /// let _kept = graph.anchor((n_in, latest)); // the part to keep
    /// drop(edge);
    /// graph.collect_garbage(); // what only the edge reached goes
    /// graph.send(n_in, 1);
    /// assert_eq!(*graph.sample(latest), 1);
    /// assert_eq!(graph.try_sample(doubled).err(), Some(TokenError::Stale));
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
        let flag = Liveness::new(&self.build.released);
        for i in nodes {
            self.build.anchors.push((i, flag.clone()));
        }
        Anchored::new(value, Anchor::new(Some(flag)))
    }

    /// [`anchor`](Runtime::anchor), returning the error instead of panicking.
    /// A value with a stale or foreign token anchors nothing, and is dropped.
    pub fn try_anchor<T: Trace>(&mut self, value: T) -> Result<Anchored<T>, TokenError> {
        let mut tracer = Tracer::new();
        value.trace(&mut tracer);
        let start = self.build.anchors.len();
        let flag = Liveness::new(&self.build.released);
        for token in tracer.visited {
            match self.lookup(token) {
                Ok(i) => self.build.anchors.push((i, flag.clone())),
                Err(error) => {
                    self.build.anchors.truncate(start);
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
    /// let (mut graph, edge) = Runtime::build(|b| {
    ///     let (numbers, numbers_in) = b.input::<u32>();
    ///     (numbers_in, numbers.hold(b, 0u32))
    /// });
    /// let (numbers_in, latest) = edge.keep();
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

    /// With `true`, collects after every transaction, whatever the policy,
    /// so that a closure capture that should have been declared with
    /// [`depends`](crate::Build::depends), or a token that left a transaction
    /// without an anchor, is a stale-token error the first time the code
    /// runs rather than whenever the automatic policy happens to collect. A
    /// test setting: a transaction then costs a collection.
    ///
    /// The collection runs after the whole unit, its children and listeners
    /// included, and never inside it.
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

    /// Runs every pending input slot as a transaction of its own, and the
    /// calls both handles, an [`Io`] and a [`RemoteIo`], made before the
    /// pump began, in the order they were made: a send or a transaction as
    /// one transaction, and a registration (RFD 6, RFD 7). After each whole
    /// unit, children included, the highest-priority pending slot runs
    /// next, and a call runs only when no slot is pending. Two slots are
    /// never simultaneous; a unit is exactly as simultaneous as its sends.
    ///
    /// Called by the driver from wherever it sits: a thread the waker wakes,
    /// a future's `poll`, or a bare-metal main loop. Latency is the distance
    /// from a send to the next pump. A slot written while the pump runs, by
    /// a listener, an interrupt or another thread, pre-empts what's next,
    /// unless it has drained in this pump already: each slot drains at most
    /// once per pump. A call made after the pump began waits for the next
    /// pump, whose wake it has already made. So a listener that always
    /// sends, or always writes a slot, cannot keep a pump from returning. A
    /// collection that is due runs after each unit, as for
    /// [`send`](Runtime::send).
    ///
    /// A unit, a remote's or an `Io`'s, runs as a transaction the driver
    /// opens, and its closure sends into it. A unit whose send fails is
    /// dropped whole, with none of its sends run, and the graph stays
    /// usable: the transaction closes without running.
    ///
    /// Panics on a poisoned graph; a panic in a unit's closure or in the
    /// transaction it runs poisons it. A send to an input collected before
    /// the pump, from a unit or a slot, is a send to a collected input: a
    /// panic in a debug build, and in a release build a no-op that
    /// [`stale_operations`](Runtime::stale_operations) counts, and the unit's
    /// other sends run. A stale slot is disconnected and its event dropped.
    /// A double send inside a unit, or a token from another graph in one,
    /// panics in both builds. A panic leaves the rest pending for the next
    /// pump.
    pub fn pump(&mut self) {
        self.enter();
        if let Err((error, what)) = self.pump_all(!cfg!(debug_assertions)) {
            match error {
                PumpError::Stale => self.stale_operation(what),
                PumpError::DoubleSend => {
                    panic!("bough: a second send to a non-coalescing input in one queued unit")
                }
                PumpError::ForeignGraph => panic!("bough: a token from another graph"),
                PumpError::Poisoned => unreachable!("bough engine: pump checks the poison first"),
            }
        }
    }

    /// [`pump`](Runtime::pump), returning the error instead of panicking. The
    /// first slot, unit or call that fails is dropped, and the error
    /// returned; the rest stay pending for the next call.
    pub fn try_pump(&mut self) -> Result<(), PumpError> {
        if self.poisoned() {
            return Err(PumpError::Poisoned);
        }
        self.pump_all(false).map_err(|(error, _)| error)
    }

    /// One unit at a time: the highest-priority pending slot, if there is
    /// one, and a call otherwise. With `skip_stale`, the panicking pump's
    /// release build, a stale send is counted and skipped rather than
    /// returned.
    fn pump_all(&mut self, skip_stale: bool) -> Result<(), Stop> {
        let limit = self.begin_pump();
        loop {
            #[cfg(any(feature = "std", feature = "critical-section"))]
            if self.pump_slot(skip_stale)? {
                continue;
            }
            if !self.pump_call(limit, skip_stale)? {
                return Ok(());
            }
        }
    }

    /// A pump begins: it takes the next serial, so each slot drains once in
    /// it, and the next call through either handle wakes the driver again.
    /// Returns the stamp the next call would take; the pump runs the calls
    /// stamped before it. Where there is an inbox, it reads the stamp
    /// under the lock a remote call stamps and wakes under.
    fn begin_pump(&mut self) -> usize {
        #[cfg(any(feature = "std", feature = "critical-section"))]
        {
            self.build.edge.pumps += 1;
        }
        self.build.io.begin_pump();
        #[cfg(all(
            target_has_atomic = "ptr",
            any(feature = "std", feature = "critical-section")
        ))]
        {
            self.build.edge.inbox.begin_pump(&self.build.stamps)
        }
        #[cfg(not(all(
            target_has_atomic = "ptr",
            any(feature = "std", feature = "critical-section")
        )))]
        {
            self.build.stamps.next()
        }
    }

    /// The oldest call either handle made before `limit`: a unit as one
    /// transaction, a registration, or an `Io`'s call. False if there is
    /// none. A call made during the pump, by a listener feeding back or by
    /// another thread, waits for the next pump, so a listener that always
    /// sends cannot keep one pump from returning.
    fn pump_call(&mut self, limit: usize, skip_stale: bool) -> Result<bool, Stop> {
        let io = self.build.io.front().filter(|&stamp| before(stamp, limit));
        #[cfg(all(
            target_has_atomic = "ptr",
            any(feature = "std", feature = "critical-section")
        ))]
        let remote = self
            .build
            .edge
            .inbox
            .front()
            .filter(|&stamp| before(stamp, limit));
        #[cfg(not(all(
            target_has_atomic = "ptr",
            any(feature = "std", feature = "critical-section")
        )))]
        let remote: Option<usize> = None;
        let io_first = match (io, remote) {
            (None, None) => return Ok(false),
            (Some(io), Some(remote)) => before(io, remote),
            (io, _) => io.is_some(),
        };
        if io_first {
            let call = self
                .build
                .io
                .pop()
                .expect("bough engine: the front call is there to pop");
            call(self, skip_stale)?;
            return Ok(true);
        }
        #[cfg(all(
            target_has_atomic = "ptr",
            any(feature = "std", feature = "critical-section")
        ))]
        self.pump_remote(skip_stale)?;
        Ok(true)
    }

    /// The remote call at the front of the inbox: a unit as one
    /// transaction, or a registration.
    #[cfg(all(
        target_has_atomic = "ptr",
        any(feature = "std", feature = "critical-section")
    ))]
    fn pump_remote(&mut self, skip_stale: bool) -> Result<(), Stop> {
        match self.build.edge.inbox.pop() {
            Some(RemoteCall::Unit(unit)) => self.run_unit(skip_stale, unit),
            Some(RemoteCall::Register(registration)) => M::register(self, registration, skip_stale),
            None => unreachable!("bough engine: the front call is there to pop"),
        }
    }

    /// Runs one unit, a remote's or an `Io`'s, as one transaction. A unit
    /// whose send fails is dropped whole: its transaction closes without
    /// running.
    pub(crate) fn run_unit(
        &mut self,
        skip_stale: bool,
        unit: impl FnOnce(&mut IoTransaction<'_>),
    ) -> Result<(), Stop> {
        self.build.begin();
        let mut tx = IoTransaction {
            build: &mut self.build,
            skip_stale,
            skipped: 0,
            fault: None,
        };
        unit(&mut tx);
        let IoTransaction { skipped, fault, .. } = tx;
        self.stale_operations += skipped;
        if let Some(fault) = fault {
            self.build.cancel();
            let error = match fault {
                Fault::Stale => PumpError::Stale,
                Fault::ForeignGraph => PumpError::ForeignGraph,
                Fault::DoubleSend => PumpError::DoubleSend,
            };
            return Err((error, SEND));
        }
        self.build.finish();
        self.collect_if_due();
        Ok(())
    }

    /// The highest-priority pending slot that hasn't drained in this pump,
    /// as a transaction of its own. False if there is none. The event leaves
    /// the slot under its lock, and the transaction runs after the lock is
    /// released.
    #[cfg(any(feature = "std", feature = "critical-section"))]
    fn pump_slot(&mut self, skip_stale: bool) -> Result<bool, Stop> {
        let pump = self.build.edge.pumps;
        let Some(k) = self
            .build
            .edge
            .slots
            .iter()
            .position(|connection| connection.drained != pump && connection.slot.pending())
        else {
            return Ok(false);
        };
        // Marked as it's picked, so the pump picks each slot at most once,
        // and before its transaction runs, which may connect a slot ahead
        // of it and move it.
        self.build.edge.slots[k].drained = pump;
        let Connection { input, slot, .. } = self.build.edge.slots[k];
        let mut live = true;
        slot.drain(&mut |event| match self.build.lookup(input) {
            Ok(i) => {
                self.build.begin();
                let fire = self.build.store.ops[i as usize].fire;
                fire(&mut self.build, i, event)
                    .expect("bough engine: a slot's event is its transaction's only send");
                self.build.finish();
                self.collect_if_due();
            }
            // `connect` checked the graph, so the input was collected.
            Err(_) => live = false,
        });
        if !live {
            self.build.edge.slots.remove(k);
            slot.disconnect();
            if !skip_stale {
                return Err((PumpError::Stale, SEND));
            }
            self.stale_operations += 1;
        }
        Ok(true)
    }

    /// Registers the waker that a slot write, a remote send or a call
    /// through an [`Io`] wakes, so the driver knows to pump: it reaches every
    /// connected slot, and a slot connected later.
    ///
    /// A driver that is a future stores `cx.waker().clone()` on each poll and
    /// returns pending after pumping; a thread driver builds one from an
    /// `Arc` through `alloc::task::Wake`; a bare-metal main loop that sleeps
    /// on the interrupt itself gives `Waker::noop()`. A waker that would
    /// wake the same task as the one registered changes nothing.
    pub fn set_waker(&mut self, waker: Waker) {
        let Build { edge, io, .. } = &mut self.build;
        if edge.waker.as_ref().is_some_and(|w| w.will_wake(&waker)) {
            return;
        }
        #[cfg(any(feature = "std", feature = "critical-section"))]
        for connection in &edge.slots {
            connection.slot.set_waker(Some(waker.clone()));
        }
        #[cfg(all(
            target_has_atomic = "ptr",
            any(feature = "std", feature = "critical-section")
        ))]
        edge.inbox.set_waker(waker.clone());
        io.set_waker(&waker);
        edge.waker = Some(waker);
    }

    /// A handle for I/O code on any thread: every call through it queues
    /// for the next [`pump`](Runtime::pump), as an [`Io`]'s does (RFD 6).
    /// Every `RemoteIo` of a runtime shares its one inbox, made with the
    /// runtime, so this takes `&self`.
    #[cfg(all(
        target_has_atomic = "ptr",
        any(feature = "std", feature = "critical-section")
    ))]
    pub fn remote_io(&self) -> RemoteIo {
        RemoteIo {
            inbox: self.build.edge.inbox.clone(),
        }
    }
}

/// When collection runs. It never runs inside a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionPolicy {
    /// Amortized: after each whole unit, when the nodes allocated plus the
    /// handles released since the last collection exceed the number of
    /// nodes that collection left alive. Garbage is made by unrooting as
    /// much as by allocating, so a graph that only drops listeners still
    /// collects, and a graph that neither allocates nor drops a handle
    /// never pays. The build is a unit too: what its closure built and
    /// nothing reaches goes before `build` returns.
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
    pub(crate) fn new(alive: Option<Liveness>) -> Self {
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
/// it, one of the two kinds of root, from [`Anchored::into_parts`]. Dropping it
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
    pub(crate) fn new(alive: Option<Liveness>) -> Self {
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
/// let (_graph, edge) = Runtime::build(|b| {
///     let (rows, _rows_in) = b.input::<Anchored<Input<u32>>>();
///     let _latest = rows.map(Some).hold(b, None); // error: Anchored is not Trace
/// });
/// edge.keep();
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

/// A handle for I/O code on any thread: a `Send + Sync + Clone` endpoint
/// whose calls queue for the driver's next [`pump`](Runtime::pump), as an
/// [`Io`]'s do on the runtime's own thread (RFD 6). From
/// [`Runtime::remote_io`].
///
/// A call pushes a unit into the runtime's inbox, and wakes the driver
/// unless another remote call has since the last pump began. It never
/// blocks on the runtime, and allocates once on the calling thread. Each
/// unit is one transaction, and the pump runs them in the order they were
/// made, among an `Io`'s calls too: a
/// [`transaction`](RemoteIo::transaction) makes its sends simultaneous, and
/// two units are never merged. A `RemoteIo` works with a `Local` runtime;
/// what it carries must be `Send`. The runtime's poison is mirrored into
/// the inbox by the first entry that finds it, so calls fail from then on,
/// and a dropped runtime closes it. Dropping a `RemoteIo` does nothing.
///
/// It can listen and anchor too, as an `Io` can. The guard comes back at
/// once, the registration runs on the driver at the pump, and dropping the
/// guard first cancels it; a listener runs on the driver's thread, so it
/// must be `Send`. A waiting registration keeps the tokens it names alive
/// until the pump runs it, as an `Io`'s does, and a waiting send or
/// transaction keeps nothing alive.
///
/// ```
/// use std::cell::RefCell;
/// use std::rc::Rc;
/// use std::thread;
///
/// use bough::{Runtime, Source};
///
/// let (mut graph, edge) = Runtime::build(|b| {
///     let (numbers, numbers_in) = b.input::<u32>();
///     (numbers_in, numbers.accumulate(b, 0u32, |n, t| t + n))
/// });
/// let (numbers_in, total) = edge.keep();
/// let heard = Rc::new(RefCell::new(Vec::new())); // a Local runtime keeps its Rc
/// let sink = heard.clone();
/// graph.listen_steps(total, move |t| sink.borrow_mut().push(*t)).keep();
///
/// let remote = graph.remote_io();
/// thread::spawn(move || {
///     remote.send(numbers_in, 1).unwrap(); // one unit
///     remote.transaction(move |tx| tx.send(numbers_in, 2)).unwrap(); // another
/// })
/// .join()
/// .unwrap();
///
/// graph.pump(); // the driver: each unit is one transaction
/// assert_eq!(*heard.borrow(), [1, 3]);
/// ```
///
/// What a `RemoteIo` carries crosses threads, so it must be `Send`:
///
/// ```compile_fail,E0277
/// use std::rc::Rc;
///
/// use bough::Runtime;
///
/// let (graph, edge) = Runtime::build(|b| b.input::<Rc<u32>>().1);
/// let shared_in = edge.keep();
/// graph.remote_io().send(shared_in, Rc::new(1)).unwrap(); // error: Rc is not Send
/// ```
///
/// A call checks, in the order an `Io`'s does, that the runtime hasn't
/// dropped, isn't poisoned and isn't running graph code on this thread,
/// and that the tokens the call names are the runtime's own. A
/// transaction's closure hides its tokens, so a foreign one in it is found
/// at the pump.
///
/// A call from graph code is refused with [`IoError::FromGraphCode`]: I/O
/// from inside FRP logic, which a closure can do because a `RemoteIo` is
/// `Send + Clone + 'static`. The check is a thread token the driver stores
/// in the inbox while its graph code runs: evaluation and commit,
/// `accumulate_mut`'s function, a `construct` closure, a split's iterator,
/// in every child instant too. It is cleared before the listeners run, so
/// a listener's call queues a later transaction, the sanctioned way for
/// I/O to feed back; so does the closure of a transaction, which is I/O
/// code, and a call from any other thread. It needs a thread id and exists
/// under `std`; on bare metal it is documented and unchecked (RFD 7). It
/// knows its own runtime only: graph code calling through another
/// runtime's `RemoteIo` queues there. A panic that escapes graph code
/// leaves the token behind, so until an entry finds the poison, a call
/// from that thread reports `FromGraphCode`.
///
/// `RemoteIo` holds an `Arc` and its inbox a lock, so it exists where the
/// target has pointer atomics and there is a lock: under `std`, or with the
/// `critical-section` feature. On a Cortex-M0 the path from an interrupt
/// handler into the runtime is an [`InputSlot`](crate::InputSlot). On
/// wasm32 it exists, though a DOM callback runs on the runtime's thread and
/// can hold an [`Io`].
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
#[derive(Clone)]
pub struct RemoteIo {
    inbox: Arc<Inbox>,
}

#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
impl RemoteIo {
    /// Queues one value as a unit of its own, and wakes the driver.
    ///
    /// A waiting send keeps nothing alive, as an [`Io`]'s doesn't. Whether
    /// the input is collected by the time the driver pumps is graph
    /// knowledge, found at the pump.
    pub fn send<A: Send + 'static>(&self, input: Input<A>, value: A) -> Result<(), IoError> {
        self.unit(
            &[input.token],
            Box::new(move |tx: &mut IoTransaction<'_>| tx.send(input, value)),
        )
    }

    /// Queues several sends as one unit, and so one transaction: `f` runs
    /// on the driver, at its next pump, with an [`IoTransaction`] whose
    /// sends are simultaneous. The closure is I/O code: it has no graph
    /// access, and its order of sends does not matter. Like a send, it
    /// keeps nothing alive while it waits.
    pub fn transaction<F>(&self, f: F) -> Result<(), IoError>
    where
        F: FnOnce(&mut IoTransaction<'_>) + Send + 'static,
    {
        self.unit(&[], Box::new(f))
    }

    /// Listens to a materialized node, as [`Io::listen`] does: the
    /// [`Listener`] comes back now, the registration runs on the driver at
    /// the next pump, and dropping the listener first cancels it. The
    /// listener runs on the driver's thread, so it must be `Send`.
    pub fn listen<S, F>(&self, source: S, f: F) -> Result<Listener, IoError>
    where
        S: Node + Send,
        S::Event: 'static,
        F: FnMut(S::Event) + Send + 'static,
    {
        let token = source.node_token();
        let flag = self.register(Roots::One(token), |flag| Listen { flag, source, f })?;
        Ok(Listener::new(Some(flag)))
    }

    /// Listens to a cell, as [`Io::listen_cell`] does: the first call, with
    /// the current value, runs on the driver at the next pump.
    pub fn listen_cell<C, F>(&self, cell: C, f: F) -> Result<Listener, IoError>
    where
        C: CellRef + Send,
        F: FnMut(&C::Value) + Send + 'static,
    {
        let token = cell.token();
        let flag = self.register(Roots::One(token), |flag| ListenCell {
            flag,
            cell,
            f,
            now: true,
        })?;
        Ok(Listener::new(Some(flag)))
    }

    /// Listens to a cell's steps, as [`Io::listen_steps`] does.
    pub fn listen_steps<C, F>(&self, cell: C, f: F) -> Result<Listener, IoError>
    where
        C: CellRef + Send,
        F: FnMut(&C::Value) + Send + 'static,
    {
        let token = cell.token();
        let flag = self.register(Roots::One(token), |flag| ListenCell {
            flag,
            cell,
            f,
            now: false,
        })?;
        Ok(Listener::new(Some(flag)))
    }

    /// Anchors what `value` holds, as [`Io::anchor`] does. Only the tokens
    /// cross to the driver, so the value needn't be `Send`; the
    /// [`Anchored`] that carries it stays on this thread, or goes where
    /// `T` can.
    pub fn anchor<T: Trace>(&self, value: T) -> Result<Anchored<T>, IoError> {
        let mut tracer = Tracer::new();
        value.trace(&mut tracer);
        let tokens = tracer.visited;
        let roots = Roots::Many(tokens.clone());
        let flag = self.register(roots, |flag| AnchorTokens { flag, tokens })?;
        Ok(Anchored::new(value, Anchor::new(Some(flag))))
    }

    /// The checks a call makes before it queues: the runtime has dropped,
    /// is poisoned, or runs graph code on this thread, or a token the call
    /// names is another runtime's.
    fn check(&self, tokens: &[Token]) -> Result<(), IoError> {
        if self.inbox.is_closed() {
            return Err(IoError::Gone);
        }
        if self.inbox.is_poisoned() {
            return Err(IoError::Poisoned);
        }
        if self.inbox.inside() {
            return Err(IoError::FromGraphCode);
        }
        if tokens.iter().any(|token| token.graph != self.inbox.graph) {
            return Err(IoError::ForeignGraph);
        }
        Ok(())
    }

    /// Queues a unit that names `tokens`, unless the checks refuse it or
    /// the runtime has dropped since. It keeps none of them alive.
    fn unit(&self, tokens: &[Token], unit: Unit) -> Result<(), IoError> {
        self.check(tokens)?;
        self.inbox
            .push(Waiting::new(RemoteCall::Unit(unit), Roots::None, None))
            .map_err(|_| IoError::Gone)
    }

    /// Makes the liveness a guard shares with its registration, and queues
    /// the registration `make` builds with it. The pump skips it if the
    /// guard has gone.
    fn register<R: Registration + 'static>(
        &self,
        roots: Roots,
        make: impl FnOnce(Liveness) -> R,
    ) -> Result<Liveness, IoError> {
        self.check(roots.as_slice())?;
        let flag = Liveness::new(&self.inbox.released);
        let registration = Box::new(make(flag.clone()));
        let waiting = Waiting::new(
            RemoteCall::Register(registration),
            roots,
            Some(flag.clone()),
        );
        self.inbox.push(waiting).map_err(|_| IoError::Gone)?;
        Ok(flag)
    }
}

/// The sends of one unit a handle queued, an [`Io`]'s or a
/// [`RemoteIo`]'s, run on the driver inside the transaction it opened for
/// the unit, so they are simultaneous.
///
/// Whether an input is collected or coalesces is graph knowledge, so a
/// failed send here is found at [`Runtime::pump`], which drops the whole unit
/// and reports it; the closure's later sends are ignored.
pub struct IoTransaction<'a> {
    /// The driver's build context, with the mode out of sight.
    build: &'a mut dyn Start,
    /// The panicking pump's release build: a stale send is skipped and
    /// counted rather than a fault.
    skip_stale: bool,
    skipped: u64,
    /// The first send that failed; the unit is dropped.
    fault: Option<Fault>,
}

impl IoTransaction<'_> {
    /// Sends one value in this unit. The value needn't be `Send`: the
    /// closure that sends it runs on the driver.
    pub fn send<A: 'static>(&mut self, input: Input<A>, value: A) {
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
        let (mut graph, edge) = Runtime::build(|b| {
            let (linear, linear_in) = b.input::<u32>();
            let latest = linear.hold(b, 0u32);
            let (events, shared_in) = b.input::<u32>();
            let shared = events.share(b);
            let plus = shared.map(|x| x + 1).hold(b, 0u32);
            let same = shared.hold(b, 0u32);
            (linear_in, shared_in, shared, [latest, plus, same])
        });
        let (linear_in, shared_in, shared, _consumers) = edge.keep();
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
