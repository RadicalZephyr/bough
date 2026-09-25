// SPDX-License-Identifier: MPL-2.0

//! The I/O side (RFD 2, RFD 5, RFD 6, RFD 7).

#[cfg(target_has_atomic = "ptr")]
use alloc::boxed::Box;
#[cfg(target_has_atomic = "ptr")]
use alloc::sync::Arc;
#[cfg(target_has_atomic = "ptr")]
use alloc::vec::Vec;
use core::marker::PhantomData;
use core::task::Waker;

use crate::Build;
use crate::error::{PoisonedError, PumpError, SendError, TokenError, TransactionSendError};
#[cfg(target_has_atomic = "ptr")]
use crate::error::{RemoteSendError, RemoteTransactionError};
#[cfg(target_has_atomic = "ptr")]
use crate::mode::Threaded;
use crate::mode::{Accepts, Local, Mode};
use crate::source::Node;
use crate::token::{Cell, Input, TokenRef};
use crate::trace::Trace;

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
        todo!()
    }
}

impl<M: Mode> Graph<M> {
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
        todo!()
    }

    /// [`listen`](Graph::listen), returning the error instead of panicking.
    pub fn try_listen<S, F>(&mut self, source: S, f: F) -> Result<Listener<M>, TokenError>
    where
        S: Node,
        S::Event: 'static,
        F: FnMut(S::Event) + 'static,
        M: Accepts<F>,
    {
        todo!()
    }

    /// Listens to a cell: fires once now with the current value, then on
    /// every step, with the value by reference. The I/O form of
    /// [`steps_with_current`](Cell::steps_with_current), Sodium's `value`.
    pub fn listen_cell<A, F>(&mut self, cell: Cell<A>, f: F) -> Listener<M>
    where
        A: 'static,
        F: FnMut(&A) + 'static,
        M: Accepts<F>,
    {
        todo!()
    }

    /// [`listen_cell`](Graph::listen_cell), returning the error instead of
    /// panicking.
    pub fn try_listen_cell<A, F>(&mut self, cell: Cell<A>, f: F) -> Result<Listener<M>, TokenError>
    where
        A: 'static,
        F: FnMut(&A) + 'static,
        M: Accepts<F>,
    {
        todo!()
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
        todo!()
    }

    /// [`listen_steps`](Graph::listen_steps), returning the error instead of
    /// panicking.
    pub fn try_listen_steps<A, F>(&mut self, cell: Cell<A>, f: F) -> Result<Listener<M>, TokenError>
    where
        A: 'static,
        F: FnMut(&A) + 'static,
        M: Accepts<F>,
    {
        todo!()
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
/// graph and shares a flag with its node instead, so it can be dropped inside
/// a listener. The flag's type is the mode's, a counted cell in `Local` and
/// an atomic in `Threaded`, which is why the handle carries the mode; the
/// parameter is defaulted, so `Local` code never writes it.
pub struct Listener<M: Mode = Local> {
    alive: M::Flag,
}

impl<M: Mode> Listener<M> {
    /// Stops listening now, the same as dropping the handle.
    pub fn unlisten(self) {
        todo!()
    }

    /// Keeps listening for the life of the graph, without a handle to hold.
    pub fn keep(self) {
        todo!()
    }
}

/// The handle that keeps a node alive from I/O code without listening to
/// it, one of the three kinds of root. Dropping it removes the root. It
/// carries the mode for the same reason a [`Listener`] does.
///
/// Not `Pin`, which is an unrelated concept in `std::pin`, and not `Root`,
/// which is the concept this is one kind of.
pub struct Anchor<M: Mode = Local> {
    alive: M::Flag,
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
