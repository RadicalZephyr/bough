//! The build context (RFD 2).

use alloc::vec::Vec;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::builder::{Builder, Stores};
use crate::cell::CellRef;
use crate::engine::boundary::Boundary;
#[cfg(any(feature = "std", feature = "critical-section"))]
use crate::engine::boundary::{Connection, Drain};
use crate::engine::nodes::stream::ChainNode;
use crate::engine::{Sched, Store, Tx};
use crate::guard::{Liveness, Released, Stamps};
use crate::io::IoQueue;
use crate::mode::{Erase, Local, Mode};
#[cfg(any(feature = "std", feature = "critical-section"))]
use crate::slot::InputSlot;
use crate::source::Source;
#[cfg(any(feature = "std", feature = "critical-section"))]
use crate::token::Input;
use crate::token::{Cell, Token};

/// The next graph id. Ids start at 1, so a token that names no graph never
/// validates.
static NEXT_GRAPH: AtomicU32 = AtomicU32::new(1);

#[cfg(target_has_atomic = "32")]
fn next_graph_id() -> u32 {
    NEXT_GRAPH.fetch_add(1, Ordering::Relaxed)
}

/// A Cortex-M0 has 32-bit atomic loads and stores and no read-modify-write,
/// so the id is a load and a store. Two graphs built at once, one of them
/// from an interrupt handler, could share an id there.
#[cfg(not(target_has_atomic = "32"))]
fn next_graph_id() -> u32 {
    let id = NEXT_GRAPH.load(Ordering::Relaxed);
    NEXT_GRAPH.store(id.wrapping_add(1), Ordering::Relaxed);
    id
}

/// The engine's own builder, for a runtime in mode `M`: it stores
/// whatever that mode accepts.
///
/// It exists inside [`Runtime::build`](crate::Runtime::build) and inside
/// [`Source::construct`] closures, and nowhere else. It has no `send` and no
/// `listen`; I/O lives on [`Runtime`](crate::Runtime).
///
/// It holds the whole graph: a `Runtime` owns one, and the build closure
/// borrows it. So it has no lifetime parameter and no public constructor.
pub struct BuildContext<M: Mode = Local> {
    pub(crate) graph_id: u32,
    pub(crate) store: Store<M>,
    /// The serial of the running instant.
    pub(crate) tx: Tx,
    /// The transaction-in-progress flag, which is also the poison.
    pub(crate) in_tx: bool,
    pub(crate) s: Sched,
    /// The I/O boundary: connected slots, the inbox and the driver's waker.
    pub(crate) boundary: Boundary,
    /// The queue the runtime's `Io`s share, in `Local`.
    pub(crate) io: M::IoQueue,
    /// The anchored nodes, each with the liveness its anchor shares. A
    /// released anchor is taken out at the next collection.
    pub(crate) anchors: Vec<(u32, Liveness)>,
    /// The count of released guards, which every guard's state shares, so
    /// that a release needs no graph access.
    pub(crate) released: Released,
    /// What every call a handle queues takes its stamp from.
    pub(crate) stamps: Stamps,
    /// The state every spent listener's entry shares, which has no owner.
    pub(crate) ownerless: Liveness,
}

impl<M: Mode> BuildContext<M> {
    pub(crate) fn new() -> Self {
        let graph_id = next_graph_id();
        let released = Released::new();
        let stamps = Stamps::new();
        BuildContext {
            graph_id,
            store: Store::new(),
            tx: 0,
            in_tx: false,
            s: Sched::default(),
            boundary: Boundary::new(graph_id, &released, &stamps),
            io: M::IoQueue::new(graph_id, &released, &stamps),
            anchors: Vec::new(),
            ownerless: Liveness::ownerless(&released),
            released,
            stamps,
        }
    }
}

impl<M: Mode> BuildContext<M> {
    /// Connects an [`InputSlot`] to an input at `priority`, so that
    /// [`pump`](crate::Runtime::pump) drains it (RFD 7).
    ///
    /// The pump drains higher priorities first, the order RTIC gives its
    /// tasks and the opposite of the NVIC's numbers, and equal priorities
    /// in connection order. It drains each pending slot as a transaction
    /// of its own, and each at most once per pump, so two slots are never
    /// simultaneous, even on one input. A slot written while the pump runs
    /// drains next, after the unit running then, unless it has drained in
    /// that pump already. Callable more than once for one
    /// input, one slot per producer. The slot's fold and the input's
    /// coalescing function are independent: the fold combines a burst
    /// between two pumps, the coalescing function combines two sends inside
    /// one transaction, which slots never cause.
    ///
    /// A slot feeds one input of one graph, and panics if it is connected
    /// already; the graph disconnects it when it is dropped. A connection
    /// is not a root: the input lives while something else reaches it, and
    /// an event for an input collected since is a stale send, found at
    /// `pump`, which drops the slot.
    ///
    /// The slot exists where a lock for it does: under `std`, and with the
    /// `critical-section` feature.
    #[cfg(any(feature = "std", feature = "critical-section"))]
    pub fn connect<A: Send + 'static>(
        &mut self,
        input: Input<A>,
        slot: &'static InputSlot<A>,
        priority: u8,
    ) {
        self.check(input.token);
        assert!(
            Drain::connect(slot, self.graph_id, self.boundary.waker.as_ref()),
            "bough: an input slot connected twice: a slot feeds one input of one graph; \
             give a second producer or a second input a slot of its own"
        );
        let slots = &mut self.boundary.slots;
        let at = slots
            .iter()
            .position(|connection| connection.priority < priority)
            .unwrap_or(slots.len());
        slots.insert(
            at,
            Connection {
                input: input.token,
                slot,
                priority,
                drained: 0,
            },
        );
    }
}

/// The closer of a cell loop, from [`Builder::cell_loop`](crate::Builder::cell_loop), which states the
/// rule its [`close`](CellLoop::close) checks: the dependency graph stays
/// acyclic, and a read of a cell's value from before the instant is not a
/// dependency.
///
/// Consumed by `close`, so a loop cannot close twice:
///
/// ```compile_fail,E0382
/// use bough::prelude::*;
///
/// let _ = Runtime::local().build(|b| {
///     let (count, count_loop) = b.cell_loop::<u32>();
///     let (ticks, _ticks_in) = b.input::<()>();
///     let next = ticks.snapshot(count, |_, n| n + 1).hold(b, 0u32);
///     count_loop.close(b, next);
///     count_loop.close(b, next); // error: use of moved value: `count_loop`
/// });
/// ```
///
/// It cannot be used from inside a `construct` closure either:
///
/// ```compile_fail,E0507
/// use bough::prelude::*;
///
/// let mut graph = Runtime::local();
/// let _ = graph.build(|b| {
///     let (events, _in) = b.input::<u32>();
///     let (forward, closer) = b.cell_loop::<u32>();
///     let _out = events.construct(b, move |b, n| {
///         let (s, _) = b.input::<u32>();
///         closer.close(b, s.hold(b, n)); // error: cannot move out of a captured variable in an FnMut closure
///     });
/// });
/// ```
pub struct CellLoop<A> {
    token: Token,
    event: PhantomData<fn() -> A>,
}

impl<A: 'static> CellLoop<A> {
    /// The closer of the loop whose forward node `token` names.
    pub(crate) fn new(token: Token) -> Self {
        CellLoop {
            token,
            event: PhantomData,
        }
    }

    /// Defines the loop: the forward token becomes `definition`. The
    /// forward's node depends on the definition from now on, steps when it
    /// steps, and reads through to its value before and after each instant,
    /// so a stream view of the forward carries the definition's steps.
    ///
    /// Panics if the loop was declared in another scope, and if the new
    /// dependency would close a cycle, naming the cycle's nodes. The
    /// forward and the definition become one cell, so a loop of affine
    /// streams panics too if each has a
    /// [`switch_stream`](Cell::switch_stream).
    pub fn close<C: Builder>(self, builder: &mut C, definition: Cell<A>) {
        builder.close_cell(self.token, definition.token);
    }
}

/// The closer of a state loop, from [`Builder::state_loop`](crate::Builder::state_loop), which checks
/// the rule [`Builder::cell_loop`](crate::Builder::cell_loop) states. Consumed by
/// [`close`](StateLoop::close), so a loop cannot close twice.
///
/// Its forward is a [`State`](crate::State), which has no stream view:
///
/// ```compile_fail,E0599
/// use bough::prelude::*;
///
/// let _ = Runtime::local().build(|b| {
///     let (members, _members_loop) = b.state_loop::<Vec<String>>();
///     let _joins = members.steps(b); // error: no method named `steps` found for struct `State`
/// });
/// ```
///
/// A cell loop's forward is a [`Cell`], which has, so a cell loop closes
/// only with a `Cell`:
///
/// ```compile_fail,E0308
/// use bough::prelude::*;
///
/// let _ = Runtime::local().build(|b| {
///     let (_members, members_loop) = b.cell_loop::<Vec<String>>();
///     let (names, _names_in) = b.input::<String>();
///     let joined = names.accumulate_mut(b, Vec::new(), |name, m: &mut Vec<String>| m.push(name));
///     members_loop.close(b, joined); // error: expected `Cell<Vec<String>>`, found `State<Vec<String>>`
/// });
/// ```
pub struct StateLoop<A> {
    token: Token,
    event: PhantomData<fn() -> A>,
}

impl<A: 'static> StateLoop<A> {
    /// The closer of the loop whose forward node `token` names.
    pub(crate) fn new(token: Token) -> Self {
        StateLoop {
            token,
            event: PhantomData,
        }
    }

    /// Defines the loop: the forward token becomes `definition`, a
    /// [`State`](crate::State) or a [`Cell`]. As with [`CellLoop::close`], the forward's
    /// node depends on the definition from now on and reads through to it.
    ///
    /// Panics if the loop was declared in another scope, and if the new
    /// dependency would close a cycle, naming the cycle's nodes.
    pub fn close<X, C>(self, builder: &mut X, definition: C)
    where
        X: Builder,
        C: CellRef<Value = A>,
    {
        builder.close_cell(self.token, definition.token());
    }
}

/// The closer of a stream loop, from [`Builder::stream_loop`](crate::Builder::stream_loop). Its
/// [`close`](StreamLoop::close) checks the rule [`Builder::cell_loop`](crate::Builder::cell_loop)
/// states: the dependency graph stays acyclic, and a read of a cell's
/// value from before the instant is not a dependency. Consumed by `close`,
/// so a loop cannot close twice.
pub struct StreamLoop<A> {
    token: Token,
    event: PhantomData<fn() -> A>,
}

impl<A: 'static> StreamLoop<A> {
    /// The closer of the loop whose forward node `token` names.
    pub(crate) fn new(token: Token) -> Self {
        StreamLoop {
            token,
            event: PhantomData,
        }
    }

    /// Defines the loop: the forward stream becomes `definition`. The
    /// chain is fused into the forward's node, which depends on the chain's
    /// source from now on and reads the cells it snapshots or gates on as
    /// they were before each instant.
    ///
    /// The node's slot is created here and keeps an event nobody consumed
    /// between transactions, so the mode must accept the event type; a
    /// `Threaded` graph refuses a loop of `Rc`s here:
    ///
    /// ```compile_fail,E0277
    /// use bough::prelude::*;
    /// use std::rc::Rc;
    ///
    /// let _ = Runtime::threaded().build(|b| {
    ///     let (_counts, counts_loop) = b.stream_loop::<Rc<u32>>();
    ///     let (numbers, _numbers_in) = b.input::<u32>();
    ///     counts_loop.close(b, numbers.map(Rc::new)); // error: Rc is not Send
    /// });
    /// ```
    ///
    /// Panics if the loop was declared in another scope, and if the chain's
    /// source is the forward or depends on it, naming the cycle's nodes.
    pub fn close<C, S>(self, builder: &mut C, definition: S)
    where
        C: Builder + Stores<S> + Stores<A>,
        S: Source<Event = A>,
    {
        let (dependency, cells) = builder.reach_of(&definition);
        let slot = <C as Stores<A>>::erase(Erase::Slot);
        let chain = <C as Stores<S>>::erase(Erase::Value(definition));
        builder.close_stream::<ChainNode<S>>(self.token, dependency, cells, slot, chain);
    }
}
