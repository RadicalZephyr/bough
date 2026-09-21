//! The I/O side (RFD 2, RFD 5, RFD 6).

use std::marker::PhantomData;
use std::sync::Arc;

use crate::Build;
use crate::error::{
    InsideTransactionError, PoisonedError, PumpError, RemoteSendError, SendError, TokenError,
    TransactionSendError,
};
use crate::mode::{Accepts, Local, Mode, Threaded};
use crate::source::Node;
use crate::token::{Cell, Input, TokenRef};
use crate::trace::Trace;

/// A built graph: the only place transactions run, and the only holder of
/// the I/O API.
///
/// The graph is entered from exactly one place at a time. Graph code never
/// holds a `Graph`; every public entry point checks that no transaction is in
/// progress, so a `Graph` smuggled into graph code fails at the entry.
pub struct Graph<M: Mode = Local> {
    mode: PhantomData<M>,
}

impl Graph<Local> {
    /// Builds a `Local` graph. The closure gets the only [`Build`] context;
    /// whatever it returns is the edge of the graph and its permanent root
    /// set, which is why `R: Trace`.
    ///
    /// The build closure runs as transaction zero.
    pub fn build<R: Trace>(f: impl FnOnce(&mut Build<Local>) -> R) -> (Graph<Local>, R) {
        todo!()
    }
}

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
        todo!()
    }
}

impl<M: Mode> Graph<M> {
    /// Sends one value in a transaction of its own, then runs its child
    /// transactions and its listeners before returning.
    ///
    /// Panics on a foreign token, a poisoned graph, or a second send to a
    /// non-coalescing input in one transaction. Sending to a collected input
    /// is unobservable by the semantics: a panic in debug builds and a
    /// counted no-op in release builds.
    pub fn send<A: 'static>(&mut self, input: Input<A>, value: A)
    where
        M: Accepts<A>,
    {
        todo!()
    }

    /// [`send`](Graph::send), returning the error instead of panicking.
    pub fn try_send<A: 'static>(&mut self, input: Input<A>, value: A) -> Result<(), SendError>
    where
        M: Accepts<A>,
    {
        todo!()
    }

    /// Several sends in one instant.
    pub fn transaction<R>(&mut self, f: impl FnOnce(&mut Transaction<'_, M>) -> R) -> R {
        todo!()
    }

    /// [`transaction`](Graph::transaction), returning the error instead of
    /// panicking.
    pub fn try_transaction<R>(
        &mut self,
        f: impl FnOnce(&mut Transaction<'_, M>) -> R,
    ) -> Result<R, PoisonedError> {
        todo!()
    }

    /// Listens to a materialized node. A linear stream is moved in, so it can
    /// be listened to once; a shared stream any number of times. Listeners
    /// take the occurrence by value, have no graph access, and run after
    /// commit.
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
    pub fn listen<S, F>(&mut self, source: S, f: F) -> Listener
    where
        S: Node,
        S::Item: 'static,
        F: FnMut(S::Item) + 'static,
        M: Accepts<F>,
    {
        todo!()
    }

    /// [`listen`](Graph::listen), returning the error instead of panicking.
    pub fn try_listen<S, F>(&mut self, source: S, f: F) -> Result<Listener, TokenError>
    where
        S: Node,
        S::Item: 'static,
        F: FnMut(S::Item) + 'static,
        M: Accepts<F>,
    {
        todo!()
    }

    /// Listens to a cell: fires once now with the current value, then on
    /// every step, with the value by reference.
    pub fn listen_cell<A, F>(&mut self, cell: Cell<A>, f: F) -> Listener
    where
        A: 'static,
        F: FnMut(&A) + 'static,
        M: Accepts<F>,
    {
        todo!()
    }

    /// [`listen_cell`](Graph::listen_cell), returning the error instead of
    /// panicking.
    pub fn try_listen_cell<A, F>(&mut self, cell: Cell<A>, f: F) -> Result<Listener, TokenError>
    where
        A: 'static,
        F: FnMut(&A) + 'static,
        M: Accepts<F>,
    {
        todo!()
    }

    /// Roots a node the I/O world wants to hold without listening to it.
    pub fn pin(&mut self, token: &impl TokenRef) -> Pin {
        todo!()
    }

    /// [`pin`](Graph::pin), returning the error instead of panicking.
    pub fn try_pin(&mut self, token: &impl TokenRef) -> Result<Pin, TokenError> {
        todo!()
    }

    /// The cell's current value, by reference.
    pub fn sample<A: 'static>(&self, cell: Cell<A>) -> &A {
        todo!()
    }

    /// [`sample`](Graph::sample), returning the error instead of panicking.
    pub fn try_sample<A: 'static>(&self, cell: Cell<A>) -> Result<&A, TokenError> {
        todo!()
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
    pub fn live_nodes(&self) -> usize {
        todo!()
    }

    /// How many operations on collected nodes were dropped in release builds.
    pub fn stale_operations(&self) -> u64 {
        todo!()
    }

    /// Runs every pending remote send, each as its own transaction in arrival
    /// order (RFD 6).
    pub fn pump(&mut self) {
        todo!()
    }

    /// [`pump`](Graph::pump), returning the error instead of panicking.
    pub fn try_pump(&mut self) -> Result<(), PumpError> {
        todo!()
    }

    /// Registers the function a [`Remote`] calls after enqueueing, so the
    /// driver knows to pump.
    pub fn set_waker(&mut self, waker: Arc<dyn Fn() + Send + Sync>) {
        todo!()
    }

    /// A handle other threads use to send into this graph.
    pub fn remote(&self) -> Remote {
        todo!()
    }

    /// [`remote`](Graph::remote), returning the error instead of panicking.
    pub fn try_remote(&self) -> Result<Remote, PoisonedError> {
        todo!()
    }
}

/// When garbage collection runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionPolicy {
    /// After a transaction, when the nodes allocated since the last
    /// collection exceed the live count.
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
    pub fn send<A: 'static>(&mut self, input: Input<A>, value: A)
    where
        M: Accepts<A>,
    {
        todo!()
    }

    /// [`send`](Transaction::send), returning the error instead of panicking.
    pub fn try_send<A: 'static>(
        &mut self,
        input: Input<A>,
        value: A,
    ) -> Result<(), TransactionSendError>
    where
        M: Accepts<A>,
    {
        todo!()
    }
}

/// A listener handle. Dropping it unlistens; it borrows nothing from the
/// graph.
pub struct Listener {
    alive: Arc<()>,
}

impl Listener {
    /// Stops listening now, the same as dropping the handle.
    pub fn unlisten(self) {
        todo!()
    }

    /// Keeps listening for the life of the graph, without a handle to hold.
    pub fn keep(self) {
        todo!()
    }
}

/// A root on a node the I/O world holds without listening to it. Dropping
/// it unpins.
pub struct Pin {
    alive: Arc<()>,
}

impl Pin {
    /// Removes the root now, the same as dropping the handle.
    pub fn unpin(self) {
        todo!()
    }

    /// Keeps the root for the life of the graph.
    pub fn keep(self) {
        todo!()
    }
}

/// A `Send + Clone` handle for sending into a graph from any thread (RFD 6).
///
/// A send enqueues and wakes the driver; it never blocks on the graph. Each
/// remote send is its own transaction, in arrival order, run by
/// [`Graph::pump`]. A remote send from the driver thread while a transaction
/// is evaluating is an error: I/O from inside graph code.
#[derive(Clone)]
pub struct Remote {
    inbox: Arc<()>,
}

impl Remote {
    /// Enqueues one value as a transaction of its own.
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

    /// Enqueues several sends as one transaction; `f` runs on the driver.
    pub fn transaction<F>(&self, f: F)
    where
        F: FnOnce(&mut RemoteTransaction) + Send + 'static,
    {
        todo!()
    }

    /// [`transaction`](Remote::transaction), returning the error instead of
    /// panicking.
    pub fn try_transaction<F>(&self, f: F) -> Result<(), InsideTransactionError>
    where
        F: FnOnce(&mut RemoteTransaction) + Send + 'static,
    {
        todo!()
    }
}

/// Several remote sends in one instant. Whether an input coalesces is graph
/// knowledge, so a double send here is discovered at [`Graph::pump`].
pub struct RemoteTransaction {
    pending: Vec<Box<dyn FnOnce() + Send>>,
}

impl RemoteTransaction {
    /// Sends one value in this transaction.
    pub fn send<A: Send + 'static>(&mut self, input: Input<A>, value: A) {
        todo!()
    }
}
