//! The I/O side (RFD 2, RFD 5, RFD 6, RFD 7).

#[cfg(target_has_atomic = "ptr")]
use alloc::boxed::Box;
#[cfg(target_has_atomic = "ptr")]
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::task::Waker;

use crate::Build;
#[cfg(feature = "statistics")]
use crate::engine::Statistics;
use crate::engine::{Cx, Entry, LISTENERS, TokenFault, part};
use crate::error::{PoisonedError, PumpError, SendError, TokenError, TransactionSendError};
#[cfg(target_has_atomic = "ptr")]
use crate::error::{RemoteSendError, RemoteTransactionError};
#[cfg(target_has_atomic = "ptr")]
use crate::mode::Threaded;
use crate::mode::{Accepts, Erase, FlagOps, Local, Mode};
use crate::source::Node;
use crate::token::{Cell, Input, Token, TokenRef};
use crate::trace::{Trace, Tracer};

/// A built graph: the only place transactions run, and the only holder of
/// the I/O API.
///
/// The graph is entered from exactly one place at a time. Graph code never
/// holds a `Graph`; every public entry point checks the
/// transaction-in-progress flag, so a `Graph` smuggled into graph code fails
/// at the entry. The same flag is the poison: only a transaction that
/// finishes clears it, so an entry that finds it set outside a transaction
/// reports `Poisoned`. That holds where a panic unwinds and where a panic is
/// a trap alike, since neither needs code to run on the way out (RFD 5).
///
/// `Graph<Threaded>` is `Send` because every field is: the engine stores
/// each value, closure and chain in the mode's carrier, which is
/// `Box<dyn Any + Send>` there. No `unsafe impl` says so.
pub struct Graph<M: Mode = Local> {
    build: Build<M>,
    /// The build closure's return value, traced once: the permanent roots.
    roots: Vec<Token>,
}

/// Transaction zero: nothing is started, so the new-node phase runs every
/// node the closure built, dependencies first.
fn build_graph<M: Mode, R: Trace>(f: impl FnOnce(&mut Build<M>) -> R) -> (Graph<M>, R) {
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
    let graph = Graph {
        build,
        roots: tracer.visited,
    };
    (graph, r)
}

impl Graph<Local> {
    /// Builds a `Local` graph. The closure gets the only [`Build`] context;
    /// whatever it returns is the edge of the graph and its permanent root
    /// set, which is why `R: Trace`.
    ///
    /// The build closure runs as transaction zero.
    pub fn build<R: Trace>(f: impl FnOnce(&mut Build<Local>) -> R) -> (Graph<Local>, R) {
        build_graph(f)
    }
}

#[cfg(target_has_atomic = "ptr")]
impl Graph<Threaded> {
    /// Builds a `Threaded` graph, which is `Send`; every value and closure it
    /// stores must be `Send`.
    ///
    /// A separate constructor rather than a mode parameter on `build`, because
    /// a defaulted type parameter takes no part in inferring an associated
    /// function.
    pub fn build_threaded<R: Trace + Send>(
        f: impl FnOnce(&mut Build<Threaded>) -> R,
    ) -> (Graph<Threaded>, R) {
        build_graph(f)
    }
}

const POISONED: &str = "bough: the graph is poisoned: a panic escaped an earlier transaction";

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

impl<M: Mode> Graph<M> {
    /// Whether a transaction never finished. Every entry checks this.
    fn poisoned(&self) -> bool {
        self.build.in_tx
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

    /// Sends one value in a transaction of its own, then runs its child
    /// transactions and its listeners before returning.
    ///
    /// Panics on a foreign token or a poisoned graph. Sending to a collected
    /// input is unobservable by the semantics: a panic in debug builds and a
    /// counted no-op in release builds.
    pub fn send<A: 'static>(&mut self, input: Input<A>, value: A)
    where
        M: Accepts<A>,
    {
        self.enter();
        // The token is checked before the transaction opens, so a foreign
        // token is a panic that leaves the graph usable.
        let i = self.build.check(input.token);
        self.build.begin();
        self.build
            .fire_start(i, value)
            .expect("bough engine: the only send of a transaction is not a double send");
        self.build.finish();
    }

    /// [`send`](Graph::send), returning the error instead of panicking.
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
        Ok(())
    }

    /// Several sends in one instant.
    ///
    /// The sends are simultaneous: nothing runs until `f` returns, so their
    /// order inside `f` does not matter. A panic inside `f`, including one
    /// from [`Transaction::send`], escapes the transaction and poisons the
    /// graph.
    pub fn transaction<R>(&mut self, f: impl FnOnce(&mut Transaction<'_, M>) -> R) -> R {
        self.enter();
        self.build.begin();
        let r = f(&mut Transaction { graph: self });
        self.build.finish();
        r
    }

    /// [`transaction`](Graph::transaction), returning the error instead of
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
    /// use bough::{Graph, Source};
    ///
    /// let (mut graph, events) = Graph::build(|b| b.input::<u32>().0);
    /// let _l = graph.listen(events.map(|n| n + 1), |n| println!("{n}")); // error: Map<..> is not a Node
    /// ```
    ///
    /// A linear stream cannot be listened to twice:
    ///
    /// ```compile_fail,E0382
    /// use bough::{Graph, Source};
    ///
    /// let (mut graph, events) = Graph::build(|b| b.input::<u32>().0);
    /// let _a = graph.listen(events, |n| println!("{n}"));
    /// let _b = graph.listen(events, |n| println!("{n}")); // error: use of moved value
    /// ```
    pub fn listen<S, F>(&mut self, source: S, f: F) -> Listener<M>
    where
        S: Node,
        S::Event: 'static,
        F: FnMut(S::Event) + 'static,
        M: Accepts<F>,
    {
        self.enter();
        let i = self.build.check(source.node_token());
        self.attach(i, f, call_stream::<M, S, F>)
    }

    /// [`listen`](Graph::listen), returning the error instead of panicking.
    pub fn try_listen<S, F>(&mut self, source: S, f: F) -> Result<Listener<M>, TokenError>
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
    /// [`steps_with_current`](Cell::steps_with_current), Sodium's `value`.
    ///
    /// The call at registration runs outside any transaction, so a panic in
    /// it leaves the graph usable. A step to an equal value is a step.
    pub fn listen_cell<A, F>(&mut self, cell: Cell<A>, mut f: F) -> Listener<M>
    where
        A: 'static,
        F: FnMut(&A) + 'static,
        M: Accepts<F>,
    {
        self.enter();
        let i = self.build.check(cell.token);
        f(self.build.value::<A>(i));
        self.attach(i, f, call_cell::<M, A, F>)
    }

    /// [`listen_cell`](Graph::listen_cell), returning the error instead of
    /// panicking.
    pub fn try_listen_cell<A, F>(
        &mut self,
        cell: Cell<A>,
        mut f: F,
    ) -> Result<Listener<M>, TokenError>
    where
        A: 'static,
        F: FnMut(&A) + 'static,
        M: Accepts<F>,
    {
        let i = self.lookup(cell.token)?;
        f(self.build.value::<A>(i));
        Ok(self.attach(i, f, call_cell::<M, A, F>))
    }

    /// Listens to a cell's steps only, with the new value by reference, and
    /// nothing at registration. The I/O form of [`steps`](Cell::steps),
    /// Sodium's `updates`.
    pub fn listen_steps<A, F>(&mut self, cell: Cell<A>, f: F) -> Listener<M>
    where
        A: 'static,
        F: FnMut(&A) + 'static,
        M: Accepts<F>,
    {
        self.enter();
        let i = self.build.check(cell.token);
        self.attach(i, f, call_cell::<M, A, F>)
    }

    /// [`listen_steps`](Graph::listen_steps), returning the error instead of
    /// panicking.
    pub fn try_listen_steps<A, F>(&mut self, cell: Cell<A>, f: F) -> Result<Listener<M>, TokenError>
    where
        A: 'static,
        F: FnMut(&A) + 'static,
        M: Accepts<F>,
    {
        let i = self.lookup(cell.token)?;
        Ok(self.attach(i, f, call_cell::<M, A, F>))
    }

    /// Registers a listener on node `i`. Its entry and its handle share a
    /// flag; dispatch skips an entry whose flag is cleared and then drops it.
    fn attach<F: 'static>(
        &mut self,
        i: u32,
        f: F,
        call: fn(&mut M::Carrier, &mut Build<M>, u32),
    ) -> Listener<M>
    where
        M: Accepts<F>,
    {
        let flag = M::Flag::live();
        let store = &mut self.build.store;
        store.listeners[i as usize].push(Entry {
            flag: flag.clone(),
            f: <M as Accepts<F>>::erase(Erase::Value(f)),
            call,
        });
        store.hot[i as usize].flags |= LISTENERS;
        Listener { alive: Some(flag) }
    }

    /// Anchors a node that I/O code wants to hold without listening to it,
    /// returning the handle that keeps it alive.
    pub fn anchor(&mut self, token: &impl TokenRef) -> Anchor<M> {
        todo!()
    }

    /// [`anchor`](Graph::anchor), returning the error instead of panicking.
    pub fn try_anchor(&mut self, token: &impl TokenRef) -> Result<Anchor<M>, TokenError> {
        todo!()
    }

    /// The cell's current value, by reference.
    ///
    /// The graph is borrowed shared, so two samples compose in one
    /// expression; nothing runs, and the value changes only at a commit,
    /// which needs `&mut self`. So a sampled reference cannot be held across
    /// a send:
    ///
    /// ```compile_fail,E0502
    /// use bough::{Graph, Source};
    ///
    /// let (mut graph, (numbers_in, latest)) = Graph::build(|b| {
    ///     let (numbers, numbers_in) = b.input::<u32>();
    ///     (numbers_in, numbers.hold(b, 0u32))
    /// });
    /// let before = graph.sample(latest);
    /// graph.send(numbers_in, 1); // error: graph is also borrowed as immutable
    /// assert_eq!(*before, 0);
    /// ```
    pub fn sample<A: 'static>(&self, cell: Cell<A>) -> &A {
        self.enter();
        let i = self.build.check(cell.token);
        self.build.value::<A>(i)
    }

    /// [`sample`](Graph::sample), returning the error instead of panicking.
    pub fn try_sample<A: 'static>(&self, cell: Cell<A>) -> Result<&A, TokenError> {
        let i = self.lookup(cell.token)?;
        Ok(self.build.value::<A>(i))
    }

    /// Runs a garbage collection now (RFD 3).
    pub fn collect_garbage(&mut self) {
        todo!()
    }

    /// [`collect_garbage`](Graph::collect_garbage), returning the error
    /// instead of panicking.
    pub fn try_collect_garbage(&mut self) -> Result<(), PoisonedError> {
        todo!()
    }

    /// Chooses when collection runs. The default is automatic and amortized.
    pub fn set_collection_policy(&mut self, policy: CollectionPolicy) {
        todo!()
    }

    /// Collects after every transaction, so a stale-token bug surfaces
    /// deterministically. A test affordance.
    pub fn set_collect_after_every_transaction(&mut self, enabled: bool) {
        todo!()
    }

    /// The number of live nodes: how the no-leak requirement is asserted.
    ///
    /// Every materializer creates one node, however long its chain;
    /// `input_cell` creates two, the input and the hold over it.
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

    /// How many operations on collected nodes were dropped in release builds.
    pub fn stale_operations(&self) -> u64 {
        todo!()
    }

    /// Runs every pending input slot as a transaction of its own, in
    /// connection order, then every queued remote unit as one transaction
    /// each, in arrival order (RFD 6, RFD 7). Two slots are never
    /// simultaneous; a unit is exactly as simultaneous as its sends.
    ///
    /// Called by the driver from wherever it sits: a thread the waker wakes,
    /// a future's `poll`, or a bare-metal main loop. Latency is the distance
    /// from a send to the next pump.
    pub fn pump(&mut self) {
        todo!()
    }

    /// [`pump`](Graph::pump), returning the error instead of panicking.
    pub fn try_pump(&mut self) -> Result<(), PumpError> {
        todo!()
    }

    /// Registers the waker that a slot write or a remote send wakes, so the
    /// driver knows to pump.
    ///
    /// A driver that is a future stores `cx.waker().clone()` on each poll and
    /// returns pending after pumping; a thread driver builds one from an
    /// `Arc` through `alloc::task::Wake`; a bare-metal main loop that sleeps
    /// on the interrupt itself gives `Waker::noop()`.
    pub fn set_waker(&mut self, waker: Waker) {
        todo!()
    }

    /// An endpoint other threads use to send into this graph.
    #[cfg(target_has_atomic = "ptr")]
    pub fn remote(&self) -> Remote {
        todo!()
    }

    /// [`remote`](Graph::remote), returning the error instead of panicking.
    #[cfg(target_has_atomic = "ptr")]
    pub fn try_remote(&self) -> Result<Remote, PoisonedError> {
        todo!()
    }
}

/// When garbage collection runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionPolicy {
    /// After a transaction, when the nodes allocated plus the roots released
    /// since the last collection exceed the live count.
    Automatic,
    /// Only on [`Graph::collect_garbage`].
    Manual,
}

/// Several sends in one instant; the only I/O-side use of the word.
pub struct Transaction<'g, M: Mode> {
    graph: &'g mut Graph<M>,
}

impl<M: Mode> Transaction<'_, M> {
    /// Sends one value in this transaction.
    ///
    /// Panics on a foreign token and on a second send to a non-coalescing
    /// input. The panic escapes the transaction, so it poisons the graph.
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
            Err(TransactionSendError::Stale) => {
                panic!("bough: a stale token: its node was collected")
            }
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
/// graph and shares a flag with its node instead, so it can be dropped inside
/// a listener. The flag's type is the mode's, a counted cell in `Local` and
/// an atomic in `Threaded`, which is why the handle carries the mode; the
/// parameter is defaulted, so `Local` code never writes it.
pub struct Listener<M: Mode = Local> {
    /// The flag shared with the node's entry; `None` once kept.
    alive: Option<M::Flag>,
}

impl<M: Mode> Listener<M> {
    /// Stops listening now, the same as dropping the handle.
    pub fn unlisten(self) {
        drop(self);
    }

    /// Keeps listening for the life of the graph, without a handle to hold.
    /// The handle gives up its share of the flag without clearing it.
    pub fn keep(mut self) {
        self.alive = None;
    }
}

impl<M: Mode> Drop for Listener<M> {
    fn drop(&mut self) {
        if let Some(flag) = self.alive.take() {
            flag.clear();
        }
    }
}

/// The handle that keeps a node alive from I/O code without listening to
/// it, one of the three kinds of root. Dropping it removes the root. It
/// carries the mode for the same reason a [`Listener`] does.
///
/// Not `Pin`, which is an unrelated concept in `std::pin`, and not `Root`,
/// which is the concept this is one kind of.
pub struct Anchor<M: Mode = Local> {
    /// The flag shared with the anchored node; `None` once kept.
    alive: Option<M::Flag>,
}

impl<M: Mode> Anchor<M> {
    /// Removes the root now, the same as dropping the handle.
    pub fn unanchor(self) {
        todo!()
    }

    /// Keeps the root for the life of the graph.
    pub fn keep(self) {
        todo!()
    }
}

/// A `Send + Clone` endpoint for sending into a graph from any thread
/// (RFD 6).
///
/// A send pushes a unit into the inbox and wakes the driver; it never blocks
/// on the graph. Each unit is one transaction, run by [`Graph::pump`] in
/// arrival order. A remote send from the driver thread while a transaction
/// runs is an error, `InsideTransaction`: I/O from inside graph code; the
/// check needs a thread id and exists under `std`. The inbox mirrors the
/// graph's poison, so sends fail once the graph is poisoned.
///
/// `Remote` is an `Arc`, so it exists where the target has pointer atomics.
/// On a Cortex-M0 the path from an interrupt handler into the graph is an
/// [`InputSlot`](crate::InputSlot). On wasm32 it exists and is the path for
/// DOM callbacks, which may run inside a transaction's listener and so must
/// not send directly.
#[cfg(target_has_atomic = "ptr")]
#[derive(Clone)]
pub struct Remote {
    inbox: Arc<()>,
}

#[cfg(target_has_atomic = "ptr")]
impl Remote {
    /// Enqueues one value as a unit of its own.
    pub fn send<A: Send + 'static>(&self, input: Input<A>, value: A) {
        todo!()
    }

    /// [`send`](Remote::send), returning the error instead of panicking.
    pub fn try_send<A: Send + 'static>(
        &self,
        input: Input<A>,
        value: A,
    ) -> Result<(), RemoteSendError> {
        todo!()
    }

    /// Enqueues several sends as one unit, and so one transaction; `f` runs
    /// on the driver.
    pub fn transaction<F>(&self, f: F)
    where
        F: FnOnce(&mut RemoteTransaction) + Send + 'static,
    {
        todo!()
    }

    /// [`transaction`](Remote::transaction), returning the error instead of
    /// panicking.
    pub fn try_transaction<F>(&self, f: F) -> Result<(), RemoteTransactionError>
    where
        F: FnOnce(&mut RemoteTransaction) + Send + 'static,
    {
        todo!()
    }
}

/// Several remote sends in one unit. Whether an input coalesces is graph
/// knowledge, so a double send here is discovered at [`Graph::pump`].
#[cfg(target_has_atomic = "ptr")]
pub struct RemoteTransaction {
    pending: Vec<Box<dyn FnOnce() + Send>>,
}

#[cfg(target_has_atomic = "ptr")]
impl RemoteTransaction {
    /// Sends one value in this unit.
    pub fn send<A: Send + 'static>(&mut self, input: Input<A>, value: A) {
        todo!()
    }
}
