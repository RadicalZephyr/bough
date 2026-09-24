//! The build context (RFD 2).

use alloc::boxed::Box;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::cell::CellRef;
use crate::engine::edge::Edge;
#[cfg(any(feature = "std", feature = "critical-section"))]
use crate::engine::edge::{Connection, Drain};
use crate::engine::nodes::cell::{ConstantNode, HoldNode};
use crate::engine::nodes::stream::{CoalescingInput, SlotNode};
use crate::engine::{COMMITS, Data, Kind, NodeOps, Ops, Sched, Store, Tx};
use crate::mode::{Accepts, Erase, Local, Mode};
#[cfg(any(feature = "std", feature = "critical-section"))]
use crate::slot::InputSlot;
use crate::source::Source;
use crate::token::{Cell, Input, State, Stream, Token, TokenRef};
use crate::trace::Trace;

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

/// The context every node-creating operation requires.
///
/// It exists inside [`Graph::build`](crate::Graph::build) and inside
/// [`Source::construct`] closures, and nowhere else. It has no `send` and no
/// `listen`; I/O lives on [`Graph`](crate::Graph).
///
/// It is the engine's core itself: a `Graph` owns one, and the build closure
/// borrows it. So it has no lifetime parameter and no public constructor.
pub struct Build<M: Mode = Local> {
    pub(crate) graph_id: u32,
    pub(crate) store: Store<M>,
    /// The serial of the running instant.
    pub(crate) tx: Tx,
    /// The transaction-in-progress flag, which is also the poison.
    pub(crate) in_tx: bool,
    pub(crate) s: Sched,
    /// The I/O edge: connected slots, the inbox and the driver's waker.
    pub(crate) edge: Edge,
}

impl<M: Mode> Build<M> {
    pub(crate) fn new() -> Self {
        let graph_id = next_graph_id();
        Build {
            graph_id,
            store: Store::new(),
            tx: 0,
            in_tx: false,
            s: Sched::default(),
            edge: Edge::new(graph_id),
        }
    }

    /// The hold of an input cell. Its chain is the input's own stream token,
    /// which is `Send` whatever `A` is, so it is erased through
    /// `erase_send` and needs no `Accepts<Stream<A>>` bound.
    fn hold_input<A>(&mut self, stream: Stream<A>, initial: A) -> Cell<A>
    where
        A: Trace + 'static,
        M: Accepts<A>,
    {
        let input = self.check(stream.token);
        let data = Data::Cell(<M as Accepts<A>>::erase(Erase::Cell(initial)));
        let parts: Box<[M::Carrier]> = Box::new([M::erase_send(stream)]);
        let ops = &<HoldNode<Stream<A>> as NodeOps<M>>::OPS;
        let n = self.materialize(Kind::Hold, data, parts, ops, &[input], COMMITS);
        Cell::from_token(self.token(n))
    }
}

impl<M: Mode> Build<M> {
    /// A stream driven from I/O code, and the token that drives it.
    ///
    /// The input's slot is created here and keeps an event nobody consumed
    /// until the next send, so the mode must accept the event type.
    pub fn input<A: 'static>(&mut self) -> (Stream<A>, Input<A>)
    where
        M: Accepts<A>,
    {
        let data = Data::Slot(<M as Accepts<A>>::erase(Erase::Slot));
        let ops = &<SlotNode<A> as NodeOps<M>>::OPS;
        let n = self.materialize(Kind::Input, data, Box::new([]), ops, &[], 0);
        let t = self.token(n);
        (Stream::from_token(t), Input::from_token(t))
    }

    /// An input that may be sent more than once in a transaction; `f`
    /// combines the values, first send on the left.
    pub fn input_coalescing<A, F>(&mut self, f: F) -> (Stream<A>, Input<A>)
    where
        A: 'static,
        F: Fn(A, A) -> A + 'static,
        M: Accepts<A> + Accepts<F>,
    {
        let data = Data::Slot(<M as Accepts<A>>::erase(Erase::Slot));
        let parts: Box<[M::Carrier]> = Box::new([<M as Accepts<F>>::erase(Erase::Value(f))]);
        let ops = &<CoalescingInput<A, F> as NodeOps<M>>::OPS;
        let n = self.materialize(Kind::Input, data, parts, ops, &[], 0);
        let t = self.token(n);
        (Stream::from_token(t), Input::from_token(t))
    }

    /// A cell driven from I/O code: a hold over an input, two nodes.
    pub fn input_cell<A>(&mut self, initial: A) -> (Cell<A>, Input<A>)
    where
        A: Trace + 'static,
        M: Accepts<A>,
    {
        let (stream, input) = self.input::<A>();
        (self.hold_input(stream, initial), input)
    }

    /// A cell driven from I/O code whose input coalesces.
    pub fn input_cell_coalescing<A, F>(&mut self, initial: A, f: F) -> (Cell<A>, Input<A>)
    where
        A: Trace + 'static,
        F: Fn(A, A) -> A + 'static,
        M: Accepts<A> + Accepts<F>,
    {
        let (stream, input) = self.input_coalescing::<A, F>(f);
        (self.hold_input(stream, initial), input)
    }

    /// A cell that never changes.
    pub fn constant<A>(&mut self, value: A) -> Cell<A>
    where
        A: Trace + 'static,
        M: Accepts<A>,
    {
        let data = Data::Cell(<M as Accepts<A>>::erase(Erase::Cell(value)));
        let ops = &<ConstantNode<A> as NodeOps<M>>::OPS;
        let n = self.materialize(Kind::Constant, data, Box::new([]), ops, &[], 0);
        Cell::from_token(self.token(n))
    }

    /// A stream that never fires.
    ///
    /// Its node stores nothing: a reader checks the stamp before the slot,
    /// and this node's stamp never matches.
    pub fn never<A: 'static>(&mut self) -> Stream<A> {
        let n = self.materialize(
            Kind::Never,
            Data::Empty,
            Box::new([]),
            &Ops::<M>::DEFAULT,
            &[],
            0,
        );
        Stream::from_token(self.token(n))
    }

    /// Declares a cell loop: a forward token usable anywhere, and the closer
    /// that later defines it. Once closed, the forward token is its
    /// definition: it reads through to the definition's value and steps
    /// when the definition steps.
    ///
    /// ```
    /// use bough::{Graph, Source};
    ///
    /// let (mut graph, (ticks_in, count)) = Graph::build(|b| {
    ///     let (count, count_loop) = b.cell_loop::<u32>();      // declare
    ///     let (ticks, ticks_in) = b.input::<()>();
    ///     let next = ticks.snapshot(count, |_, n| n + 1).hold(b, 0u32);
    ///     count_loop.close(b, next);                             // close
    ///     (ticks_in, count)
    /// });
    /// graph.send(ticks_in, ());
    /// graph.send(ticks_in, ());
    /// assert_eq!(*graph.sample(count), 2);
    /// ```
    ///
    /// A loop is legal when the dependency graph stays acyclic, which
    /// [`close`](CellLoop::close) checks. Reading a cell's value with
    /// `snapshot`, `gate` or `sample` is not a dependency, since the read
    /// sees the value from before the instant. Neither is a
    /// `switch_stream`'s selection, nor [`depends`](Build::depends); and
    /// the output of a `split` or a `defer` fires in a later child instant,
    /// so it does not depend on the input. Every other use of a cell is a
    /// dependency: `map_cell`, `lift`, the stream views `steps` and
    /// `steps_with_current`, and a `switch_cell`'s outer and the inner it
    /// selects. So a definition
    /// may reach its own forward token only through those reads, as `next`
    /// does above through a snapshot. A hold does not delay its stream
    /// views, so closing with `hold(merge(ticks, forward.steps(b).map(f)))`
    /// is refused, and so is closing with `forward.map_cell(f)` or with a
    /// `lift` over the forward: each defines the cell's value at an instant
    /// in terms of itself at that instant. The panic names the nodes of the
    /// cycle.
    ///
    /// A loop must close in the scope that declared it: the build closure,
    /// or one run of a `construct` closure. A loop still open when that
    /// scope ends is a panic there, and so is sampling the forward token
    /// before the loop is closed, since there is no value to return. A
    /// definition that is a [`State`] closes a
    /// [`state_loop`](Build::state_loop) instead.
    pub fn cell_loop<A: 'static>(&mut self) -> (Cell<A>, CellLoop<A>) {
        let token = self.loop_node();
        (
            Cell::from_token(token),
            CellLoop {
                token,
                event: PhantomData,
            },
        )
    }

    /// Declares a loop whose definition is a [`State`]: the state of an
    /// in-place accumulator, or a read-through cell over one. The forward
    /// token is a `State` too, so no stream view of it can exist, which
    /// such a definition could not give a value after the instant to. It
    /// may also close with a [`Cell`], giving a forward that only reads.
    ///
    /// ```
    /// use bough::{Graph, Source};
    ///
    /// let (mut graph, (names_in, members)) = Graph::build(|b| {
    ///     let (members, members_loop) = b.state_loop::<Vec<String>>();
    ///     let (names, names_in) = b.input::<String>();
    ///     // A name joins once: the snapshot reads the members before the instant.
    ///     let joined = names
    ///         .snapshot(members, |name, m| (!m.contains(&name)).then_some(name))
    ///         .filter_map(|name| name)
    ///         .accumulate_mut(b, Vec::new(), |name, m: &mut Vec<String>| m.push(name));
    ///     members_loop.close(b, joined);
    ///     (names_in, members)
    /// });
    /// graph.send(names_in, "ada".to_string());
    /// graph.send(names_in, "ada".to_string());
    /// graph.send(names_in, "grace".to_string());
    /// assert_eq!(*graph.sample(members), ["ada", "grace"]);
    /// ```
    ///
    /// The rule is the one [`cell_loop`](Build::cell_loop) states: the
    /// dependency graph stays acyclic, and a read of a cell's value from
    /// before the instant is not a dependency.
    pub fn state_loop<A: 'static>(&mut self) -> (State<A>, StateLoop<A>) {
        let token = self.loop_node();
        (
            State::from_token(token),
            StateLoop {
                token,
                event: PhantomData,
            },
        )
    }

    /// Declares a stream loop: a linear forward stream, and the closer that
    /// later defines it with any chain. Close fuses the chain into the
    /// forward's own node, so a stream loop adds no node to its definition.
    ///
    /// ```
    /// use bough::{Graph, Source};
    ///
    /// // A running total, fed back through a hold that a snapshot reads.
    /// let (mut graph, (numbers_in, total)) = Graph::build(|b| {
    ///     let (sums, sums_loop) = b.stream_loop::<u32>();
    ///     let total = sums.hold(b, 0u32);
    ///     let (numbers, numbers_in) = b.input::<u32>();
    ///     sums_loop.close(b, numbers.snapshot(total, |n, t| n + t));
    ///     (numbers_in, total)
    /// });
    /// graph.send(numbers_in, 2);
    /// graph.send(numbers_in, 3);
    /// assert_eq!(*graph.sample(total), 5);
    /// ```
    ///
    /// The rule is the one [`cell_loop`](Build::cell_loop) states: the
    /// dependency graph stays acyclic, and a read of a cell's value from
    /// before the instant is not a dependency. So the definition above may
    /// snapshot a hold of the forward, and a loop through such a read needs
    /// no `split` or `defer`. A loop through a `split` or a `defer` is legal
    /// too, since their output fires in a later child instant. A chain
    /// whose events come from the forward itself, or from anything that
    /// depends on it, a hold's `steps` included, is refused at close.
    pub fn stream_loop<A: 'static>(&mut self) -> (Stream<A>, StreamLoop<A>) {
        let token = self.stream_loop_node();
        (
            Stream::from_token(token),
            StreamLoop {
                token,
                event: PhantomData,
            },
        )
    }

    /// Declares that `node` keeps every token in `on` alive, for a closure
    /// that captures tokens that are not upstream of its own node (RFD 3).
    /// One declaration per closure; the slice is heterogeneous, and takes
    /// references, so a linear stream is not consumed.
    ///
    /// A node is alive while a root reaches it: the build closure's return
    /// value, a live listener, or a live [`Anchor`](crate::Anchor). What a
    /// node reaches is what it depends on, the tokens in a stateful cell's
    /// committed value, and what this declares. A closure's captures are
    /// invisible to the collector, so a capture of a node that is not
    /// upstream of the closure's own node, a backward or an unrelated one,
    /// must be declared here, or the node is collected when nothing else
    /// reaches it and the closure's next use of the token is a stale-token
    /// error. Here the map emits a cell nothing else names:
    ///
    /// ```
    /// use bough::{Graph, Source};
    ///
    /// let (mut graph, (pick_in, shown)) = Graph::build(|b| {
    ///     let english = b.constant("hello".to_string());
    ///     let french = b.constant("bonjour".to_string());
    ///     let (pick, pick_in) = b.input::<bool>();
    ///     let language = pick
    ///         .map(move |fr| if fr { french } else { english })
    ///         .hold(b, english);
    ///     // The hold names `english` in its value, but `french` only in
    ///     // its closure until the first pick.
    ///     b.depends(&language, &[&french, &english]);
    ///     (pick_in, language.switch_cell(b))
    /// });
    /// graph.set_collect_after_every_transaction(true); // a test setting
    /// graph.send(pick_in, true);
    /// assert_eq!(graph.sample(shown), "bonjour");
    /// ```
    ///
    /// A capture upstream of the node needs nothing, since the node reaches
    /// it anyway. A declaration is reach, not a dependency: it orders no
    /// evaluation and can close no cycle. It panics on a stale or foreign
    /// token, as every build-time use of one does.
    pub fn depends(&mut self, node: &impl TokenRef, on: &[&dyn TokenRef]) {
        let n = self.check(node.token());
        for token in on {
            let d = self.check(token.token());
            self.store.cold[n as usize].reach.push(d);
        }
    }

    /// Connects an [`InputSlot`] to an input, so that
    /// [`pump`](crate::Graph::pump) drains it (RFD 7).
    ///
    /// Callable more than once for one input, one slot per producer; the
    /// driver drains slots in connection order, each pending one as a
    /// transaction of its own, so two slots are never simultaneous, even
    /// on one input. The slot's fold and the input's coalescing function
    /// are independent: the fold combines a burst between two pumps, the
    /// coalescing function combines two sends inside one transaction, which
    /// slots never cause.
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
    pub fn connect<A: Send + 'static>(&mut self, input: Input<A>, slot: &'static InputSlot<A>) {
        self.check(input.token);
        assert!(
            Drain::connect(slot, self.graph_id, self.edge.waker.as_ref()),
            "bough: an input slot connected twice: a slot feeds one input of one graph; \
             give a second producer or a second input a slot of its own"
        );
        self.edge.slots.push(Connection {
            input: input.token,
            slot,
        });
    }
}

/// The closer of a cell loop, from [`Build::cell_loop`], which states the
/// rule its [`close`](CellLoop::close) checks: the dependency graph stays
/// acyclic, and a read of a cell's value from before the instant is not a
/// dependency.
///
/// Consumed by `close`, so a loop cannot close twice:
///
/// ```compile_fail,E0382
/// use bough::{Graph, Source};
///
/// let (_graph, _) = Graph::build(|b| {
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
/// use bough::{Graph, Source};
///
/// let (graph, _) = Graph::build(|b| {
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
    /// Defines the loop: the forward token becomes `definition`. The
    /// forward's node depends on the definition from now on, steps when it
    /// steps, and reads through to its value before and after each instant,
    /// so a stream view of the forward carries the definition's steps.
    ///
    /// Panics if the loop was declared in another scope, and if the new
    /// dependency would close a cycle, naming the cycle's nodes. The
    /// forward and the definition become one cell, so a loop of linear
    /// streams panics too if each has a
    /// [`switch_stream`](Cell::switch_stream).
    pub fn close<M: Mode>(self, build: &mut Build<M>, definition: Cell<A>) {
        build.close_cell_loop(self.token, definition.token);
    }
}

/// The closer of a state loop, from [`Build::state_loop`], which checks
/// the rule [`Build::cell_loop`] states. Consumed by
/// [`close`](StateLoop::close), so a loop cannot close twice.
///
/// Its forward is a [`State`], which has no stream view:
///
/// ```compile_fail,E0599
/// use bough::{Graph, Source};
///
/// let (_graph, _) = Graph::build(|b| {
///     let (members, _members_loop) = b.state_loop::<Vec<String>>();
///     let _joins = members.steps(b); // error: no method named `steps` found for struct `State`
/// });
/// ```
///
/// A cell loop's forward is a [`Cell`], which has, so a cell loop closes
/// only with a `Cell`:
///
/// ```compile_fail,E0308
/// use bough::{Graph, Source};
///
/// let (_graph, _) = Graph::build(|b| {
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
    /// Defines the loop: the forward token becomes `definition`, a
    /// [`State`] or a [`Cell`]. As with [`CellLoop::close`], the forward's
    /// node depends on the definition from now on and reads through to it.
    ///
    /// Panics if the loop was declared in another scope, and if the new
    /// dependency would close a cycle, naming the cycle's nodes.
    pub fn close<M, C>(self, build: &mut Build<M>, definition: C)
    where
        M: Mode,
        C: CellRef<Value = A>,
    {
        build.close_cell_loop(self.token, definition.token());
    }
}

/// The closer of a stream loop, from [`Build::stream_loop`]. Its
/// [`close`](StreamLoop::close) checks the rule [`Build::cell_loop`]
/// states: the dependency graph stays acyclic, and a read of a cell's
/// value from before the instant is not a dependency. Consumed by `close`,
/// so a loop cannot close twice.
pub struct StreamLoop<A> {
    token: Token,
    event: PhantomData<fn() -> A>,
}

impl<A: 'static> StreamLoop<A> {
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
    /// use bough::{Graph, Source};
    /// use std::rc::Rc;
    ///
    /// let (_graph, _) = Graph::build_threaded(|b| {
    ///     let (_counts, counts_loop) = b.stream_loop::<Rc<u32>>();
    ///     let (numbers, _numbers_in) = b.input::<u32>();
    ///     counts_loop.close(b, numbers.map(Rc::new)); // error: Rc is not Send
    /// });
    /// ```
    ///
    /// Panics if the loop was declared in another scope, and if the chain's
    /// source is the forward or depends on it, naming the cycle's nodes.
    pub fn close<M, S>(self, build: &mut Build<M>, definition: S)
    where
        M: Mode + Accepts<S> + Accepts<A>,
        S: Source<Event = A>,
    {
        build.close_stream_loop(self.token, definition);
    }
}
