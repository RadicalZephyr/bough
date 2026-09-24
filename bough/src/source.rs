//! Streams as chains and nodes (RFD 4).
//!
//! Adapters transform events and take no context; each returns its adapter
//! type, which is itself a [`Source`]. A chain is a linear sequence of
//! adapters with no materializer. Materializers take the build context and
//! create exactly one node from a chain, fusing its adapters into that
//! node's closure. A chain is linear: it is consumed by the first
//! materializer, and using it twice is a compile error.
//!
//! ```
//! use bough::{Graph, Source};
//!
//! let (graph, total) = Graph::build(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let (limit, _limit_in) = b.input_cell(10u32);
//!     numbers
//!         .map(|n| n * 2)                 // an adapter: no node, no context
//!         .filter(|n| *n > 2)             // another adapter
//!         .snapshot(limit, |n, l| n.min(*l))
//!         .hold(b, 0u32)                  // one node for the whole chain
//! });
//! ```
//!
//! A chain used twice does not compile:
//!
//! ```compile_fail,E0382
//! use bough::{Graph, Source};
//!
//! let (graph, _) = Graph::build(|b| {
//!     let (numbers, _in) = b.input::<u32>();
//!     let doubled = numbers.map(|n| n * 2);
//!     let a = doubled.hold(b, 0u32);
//!     let b2 = doubled.hold(b, 0u32); // error: use of moved value
//! });
//! ```
//!
//! A chain runs only inside its node. The hidden method that pulls it takes
//! a context graph code cannot name or construct:
//!
//! ```compile_fail,E0433
//! use bough::{Graph, Source};
//!
//! let (graph, _) = Graph::build(|b| {
//!     let (mut numbers, _in) = b.input::<u32>();
//!     let _ = numbers.pull(&mut bough::Cx::new(b)); // error: no `Cx` in `bough`
//! });
//! ```

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::Build;
use crate::cell::CellRef;
use crate::engine::nodes::cell::{AccumulateNode, HoldNode, InPlaceNode, ScanNode};
use crate::engine::nodes::stream::{ChainNode, MergeNode};
use crate::engine::{COMMITS, Cx, Data, Kind, NodeOps};
use crate::mode::{Accepts, Erase, Mode};
use crate::token::{Cell, Shared, State, Stream, Token};
use crate::trace::Trace;

pub(crate) mod sealed {
    /// Implemented by the two node types and the adapter types.
    pub trait Sealed {}
}

/// Anything that yields events: a materialized node or a chain of adapters.
///
/// The event is an associated type, the way `Iterator` has `Item`: an adapter
/// type such as [`Map`] cannot implement a generic `Source<A>`, because `A`
/// would appear only in its bounds. `Source<Event = Click>` reads as "a source
/// of click events".
///
/// Sealed, and `'static`: a materializer stores the chain itself in its node
/// and runs it through the hidden methods, so every adapter must be the
/// crate's own.
pub trait Source: Sized + 'static + sealed::Sealed {
    /// The type of each event.
    type Event;

    /// The one node this chain reads events from: its dependency.
    #[doc(hidden)]
    fn dependency(&self) -> Token;

    /// The cells the chain reads with `snapshot` and `gate`: reach for
    /// collection, never dependencies, since a cell is read as it was
    /// before the instant.
    #[doc(hidden)]
    fn read_cells(&self, visit: &mut dyn FnMut(Token));

    /// Runs the fused chain for this instant: reads the dependency's slot,
    /// taking the event from a linear stream and cloning it from a shared
    /// one, and applies every adapter. `None` when nothing fires.
    #[doc(hidden)]
    fn pull<M: Mode>(&mut self, cx: &mut Cx<'_, M>) -> Option<Self::Event>;

    // ----- adapters: no node, no context -----

    /// Transforms each event, taking it by value.
    fn map<B, F>(self, f: F) -> Map<Self, F>
    where
        F: Fn(Self::Event) -> B + 'static,
    {
        Map { source: self, f }
    }

    /// Keeps the events the predicate accepts.
    fn filter<P>(self, predicate: P) -> Filter<Self, P>
    where
        P: Fn(&Self::Event) -> bool + 'static,
    {
        Filter {
            source: self,
            predicate,
        }
    }

    /// Maps and filters in one step. Sodium's `filterOptional` is
    /// `filter_map(|o| o)`.
    fn filter_map<B, F>(self, f: F) -> FilterMap<Self, F>
    where
        F: Fn(Self::Event) -> Option<B> + 'static,
    {
        FilterMap { source: self, f }
    }

    /// Replaces each event with a clone of one value.
    fn map_to<B>(self, value: B) -> MapTo<Self, B>
    where
        B: Clone + 'static,
    {
        MapTo {
            source: self,
            value,
        }
    }

    /// Combines each event with the value the cell had at the start of the
    /// transaction. The cell is a [`Cell`] or a [`State`].
    fn snapshot<C, B, F>(self, cell: C, f: F) -> Snapshot<Self, C, F>
    where
        C: CellRef,
        F: Fn(Self::Event, &C::Value) -> B + 'static,
    {
        Snapshot {
            source: self,
            cell,
            f,
        }
    }

    /// Keeps the events during which the cell is `true`. The cell is a
    /// [`Cell`] or a [`State`].
    fn gate<C>(self, cell: C) -> Gate<Self, C>
    where
        C: CellRef<Value = bool>,
    {
        Gate { source: self, cell }
    }

    /// Keeps only the first event.
    ///
    /// Its state lives in the fused chain and is set during evaluation. A
    /// chain runs at most once per transaction, so that cannot be told from
    /// setting it at commit.
    fn once(self) -> Once<Self> {
        Once {
            source: self,
            done: false,
        }
    }

    // ----- materializers: one node, build context -----

    /// A cell that holds the latest event, starting at `initial`.
    ///
    /// The hold is the chain's sole consumer and moves the event into its
    /// committed value at commit, so no `Clone` is needed.
    fn hold<M>(self, build: &mut Build<M>, initial: Self::Event) -> Cell<Self::Event>
    where
        M: Mode + Accepts<Self> + Accepts<Self::Event>,
        Self::Event: Trace + 'static,
    {
        let (dependency, cells) = build.chain_reach(&self);
        let data = Data::Cell(<M as Accepts<Self::Event>>::erase(Erase::Cell(initial)));
        let parts: Box<[M::Carrier]> = Box::new([<M as Accepts<Self>>::erase(Erase::Value(self))]);
        let ops = &<HoldNode<Self> as NodeOps<M>>::OPS;
        let n = build.materialize(Kind::Hold, data, parts, ops, &[dependency], COMMITS);
        build.set_reach(n, cells);
        Cell::from_token(build.token(n))
    }

    /// Sodium's `accum`: `hold initial (snapshot f self cell)`, with `f`
    /// reading the state by reference and returning the new state.
    ///
    /// The cell being snapshotted is the result itself, so `f` reads the
    /// accumulator's value from before the instant, as every reader in the
    /// transaction does.
    fn accumulate<M, S, F>(self, build: &mut Build<M>, initial: S, f: F) -> Cell<S>
    where
        M: Mode + Accepts<Self> + Accepts<S> + Accepts<F>,
        S: Trace + 'static,
        F: Fn(Self::Event, &S) -> S + 'static,
    {
        let (dependency, cells) = build.chain_reach(&self);
        let data = Data::Cell(<M as Accepts<S>>::erase(Erase::Cell(initial)));
        let parts: Box<[M::Carrier]> = Box::new([
            <M as Accepts<Self>>::erase(Erase::Value(self)),
            <M as Accepts<F>>::erase(Erase::Value(f)),
        ]);
        let ops = &<AccumulateNode<Self, S, F> as NodeOps<M>>::OPS;
        let n = build.materialize(Kind::Hold, data, parts, ops, &[dependency], COMMITS);
        build.set_reach(n, cells);
        Cell::from_token(build.token(n))
    }

    /// In-place accumulation: `f` mutates the state at commit, after every
    /// reader in the transaction has seen the previous state.
    ///
    /// Observationally the same as [`accumulate`](Source::accumulate), and a
    /// `Vec` accumulator becomes a push. The result is a [`State`], which
    /// every cell reader accepts and which has no stream view, since the new
    /// state does not exist until commit. The event waits for commit in the
    /// node, so the mode must accept its type.
    fn accumulate_mut<M, S, F>(self, build: &mut Build<M>, initial: S, f: F) -> State<S>
    where
        M: Mode + Accepts<Self> + Accepts<S> + Accepts<F> + Accepts<Self::Event>,
        S: Trace + 'static,
        F: FnMut(Self::Event, &mut S) + 'static,
        Self::Event: 'static,
    {
        let (dependency, cells) = build.chain_reach(&self);
        let data = Data::InPlace {
            state: <M as Accepts<S>>::erase(Erase::Value(initial)),
            pending: <M as Accepts<Self::Event>>::erase(Erase::Slot),
        };
        let parts: Box<[M::Carrier]> = Box::new([
            <M as Accepts<Self>>::erase(Erase::Value(self)),
            <M as Accepts<F>>::erase(Erase::Value(f)),
        ]);
        let ops = &<InPlaceNode<Self, S, F> as NodeOps<M>>::OPS;
        let n = build.materialize(Kind::InPlace, data, parts, ops, &[dependency], COMMITS);
        build.set_reach(n, cells);
        State::from_token(build.token(n))
    }

    /// Sodium's `collect`, `Iterator::scan`: a running state and an output
    /// per event. The output goes in the node's slot, so the mode must
    /// accept its type.
    ///
    /// The state is private to the node and is updated when the node runs,
    /// at most once per transaction, so `f` always reads the state from
    /// before the instant, as the semantics' snapshot of a hold would.
    fn scan<M, S, B, F>(self, build: &mut Build<M>, initial: S, f: F) -> Stream<B>
    where
        M: Mode + Accepts<Self> + Accepts<S> + Accepts<F> + Accepts<B>,
        S: Trace + 'static,
        B: 'static,
        F: Fn(Self::Event, &S) -> (B, S) + 'static,
    {
        let (dependency, cells) = build.chain_reach(&self);
        let data = Data::Slot(<M as Accepts<B>>::erase(Erase::Slot));
        let parts: Box<[M::Carrier]> = Box::new([
            <M as Accepts<Self>>::erase(Erase::Value(self)),
            <M as Accepts<F>>::erase(Erase::Value(f)),
            <M as Accepts<S>>::erase(Erase::Value(initial)),
        ]);
        let ops = &<ScanNode<Self, S, B, F> as NodeOps<M>>::OPS;
        let n = build.materialize(Kind::Stream, data, parts, ops, &[dependency], 0);
        build.set_reach(n, cells);
        Stream::from_token(build.token(n))
    }

    /// Explicit fan-out: a stream with any number of consumers, each of which
    /// clones the event. This and `map_to` are the only places `Clone` is
    /// required of a stream's events.
    fn share<M>(self, build: &mut Build<M>) -> Shared<Self::Event>
    where
        M: Mode + Accepts<Self> + Accepts<Self::Event>,
        Self::Event: Clone + 'static,
    {
        Shared::from_token(chain_node(self, build))
    }

    /// Materializes a chain as a linear stream with an identity of its own,
    /// so it can be stored in a value or returned from build.
    ///
    /// The node's slot keeps an event nobody consumed between transactions,
    /// so the mode must accept the event type; a `Threaded` graph refuses a
    /// stream of `Rc`s here:
    ///
    /// ```compile_fail,E0277
    /// use bough::{Graph, Source};
    /// use std::rc::Rc;
    ///
    /// let (_graph, _) = Graph::build_threaded(|b| {
    ///     let (numbers, _numbers_in) = b.input::<u32>();
    ///     let _stream = numbers.map(Rc::new).node(b); // error: Rc is not Send
    /// });
    /// ```
    fn node<M>(self, build: &mut Build<M>) -> Stream<Self::Event>
    where
        M: Mode + Accepts<Self> + Accepts<Self::Event>,
        Self::Event: 'static,
    {
        Stream::from_token(chain_node(self, build))
    }

    /// Merges two streams; `f` combines simultaneous events, with this
    /// stream's event on the left. Both inputs move through.
    fn merge<M, T, F>(self, build: &mut Build<M>, other: T, f: F) -> Stream<Self::Event>
    where
        M: Mode + Accepts<Self> + Accepts<T> + Accepts<F> + Accepts<Self::Event>,
        T: Source<Event = Self::Event>,
        F: Fn(Self::Event, Self::Event) -> Self::Event + 'static,
        Self::Event: 'static,
    {
        let f = <M as Accepts<F>>::erase(Erase::Value(f));
        merge_node::<M, Self, T, F>(self, build, other, f)
    }

    /// Merges two streams, this stream winning when both fire.
    fn or_else<M, T>(self, build: &mut Build<M>, other: T) -> Stream<Self::Event>
    where
        M: Mode + Accepts<Self> + Accepts<T> + Accepts<Self::Event>,
        T: Source<Event = Self::Event>,
        Self::Event: 'static,
    {
        // An engine-made function pointer is `Send` whatever the event type.
        let left: fn(Self::Event, Self::Event) -> Self::Event = |left, _| left;
        merge_node::<M, Self, T, fn(Self::Event, Self::Event) -> Self::Event>(
            self,
            build,
            other,
            M::erase_send(left),
        )
    }

    /// Emits each element of an event in its own child transaction,
    /// which runs after this one and before the next external one.
    fn split<M>(self, build: &mut Build<M>) -> Stream<<Self::Event as IntoIterator>::Item>
    where
        M: Mode + Accepts<Self>,
        Self::Event: IntoIterator + 'static,
        <Self::Event as IntoIterator>::Item: 'static,
    {
        todo!()
    }

    /// Emits each event in a child transaction of its own.
    fn defer<M>(self, build: &mut Build<M>) -> Stream<Self::Event>
    where
        M: Mode + Accepts<Self>,
        Self::Event: 'static,
    {
        todo!()
    }

    /// The semantics' `Execute`: runs `f` at each event with a fresh
    /// build context, so graph can be constructed at runtime. Its results
    /// are ordinary events: tokens on their way to a hold and a
    /// [`Cell::switch_stream`] or [`Cell::switch_cell`], or plain values.
    fn construct<M, B, F>(self, build: &mut Build<M>, f: F) -> Stream<B>
    where
        M: Mode + Accepts<Self> + Accepts<F>,
        B: 'static,
        F: FnMut(&mut Build<M>, Self::Event) -> B + 'static,
    {
        todo!()
    }
}

/// The node of `node` and `share`: the chain, fused into one program.
fn chain_node<S, M>(chain: S, build: &mut Build<M>) -> Token
where
    S: Source,
    M: Mode + Accepts<S> + Accepts<S::Event>,
    S::Event: 'static,
{
    let (dependency, cells) = build.chain_reach(&chain);
    let data = Data::Slot(<M as Accepts<S::Event>>::erase(Erase::Slot));
    let parts: Box<[M::Carrier]> = Box::new([<M as Accepts<S>>::erase(Erase::Value(chain))]);
    let ops = &<ChainNode<S> as NodeOps<M>>::OPS;
    let n = build.materialize(Kind::Stream, data, parts, ops, &[dependency], 0);
    build.set_reach(n, cells);
    build.token(n)
}

/// The node of `merge` and `or_else`: two chains and an erased function.
fn merge_node<M, S, T, F>(
    left: S,
    build: &mut Build<M>,
    right: T,
    f: M::Carrier,
) -> Stream<S::Event>
where
    M: Mode + Accepts<S> + Accepts<T> + Accepts<S::Event>,
    S: Source,
    T: Source<Event = S::Event>,
    F: Fn(S::Event, S::Event) -> S::Event + 'static,
    S::Event: 'static,
{
    let (l, mut cells) = build.chain_reach(&left);
    let (r, more) = build.chain_reach(&right);
    cells.extend(more);
    let data = Data::Slot(<M as Accepts<S::Event>>::erase(Erase::Slot));
    let parts: Box<[M::Carrier]> = Box::new([
        <M as Accepts<S>>::erase(Erase::Value(left)),
        <M as Accepts<T>>::erase(Erase::Value(right)),
        f,
    ]);
    let ops = &<MergeNode<S, T, F> as NodeOps<M>>::OPS;
    let n = build.materialize(Kind::Stream, data, parts, ops, &[l, r], 0);
    build.set_reach(n, cells);
    Stream::from_token(build.token(n))
}

impl<M: Mode> Build<M> {
    /// Checks a chain's dependency and the cells it reads, and returns
    /// their indices. A foreign or stale token is a build-time panic.
    pub(crate) fn chain_reach<S: Source>(&self, chain: &S) -> (u32, Vec<u32>) {
        let dependency = self.check(chain.dependency());
        let mut cells = Vec::new();
        chain.read_cells(&mut |t| cells.push(self.check(t)));
        (dependency, cells)
    }

    /// Records what a new node keeps alive beyond its dependencies.
    pub(crate) fn set_reach(&mut self, n: u32, cells: Vec<u32>) {
        if !cells.is_empty() {
            self.store.cold[n as usize].reach = cells;
        }
    }
}

/// A materialized node: what [`Graph::listen`](crate::Graph::listen) accepts.
/// Adapter types do not implement it, so a chain cannot be listened to.
///
/// Sealed, with hidden items: a listener reads a node the way its type
/// says, taking the event from a linear stream and cloning it from a
/// shared one, and a switch needs to know which of the two it holds.
pub trait Node: Source {
    /// A linear stream, whose one consumer takes the event.
    #[doc(hidden)]
    const LINEAR: bool;

    /// The node this token names.
    #[doc(hidden)]
    fn node_token(&self) -> Token;

    /// Reads the event of the node at `index` the way this type does: take
    /// for a linear stream, clone for a shared one.
    #[doc(hidden)]
    fn pull_inner<M: Mode>(cx: &mut Cx<'_, M>, index: u32) -> Option<Self::Event>;
}

impl<A> sealed::Sealed for Stream<A> {}
impl<A: 'static> Source for Stream<A> {
    type Event = A;
    fn dependency(&self) -> Token {
        self.token
    }
    fn read_cells(&self, _visit: &mut dyn FnMut(Token)) {}
    fn pull<M: Mode>(&mut self, cx: &mut Cx<'_, M>) -> Option<A> {
        cx.take::<A>(self.token.index)
    }
}
impl<A: 'static> Node for Stream<A> {
    const LINEAR: bool = true;
    fn node_token(&self) -> Token {
        self.token
    }
    fn pull_inner<M: Mode>(cx: &mut Cx<'_, M>, index: u32) -> Option<A> {
        cx.take::<A>(index)
    }
}

impl<A> sealed::Sealed for Shared<A> {}
impl<A: Clone + 'static> Source for Shared<A> {
    type Event = A;
    fn dependency(&self) -> Token {
        self.token
    }
    fn read_cells(&self, _visit: &mut dyn FnMut(Token)) {}
    fn pull<M: Mode>(&mut self, cx: &mut Cx<'_, M>) -> Option<A> {
        cx.cloned::<A>(self.token.index)
    }
}
impl<A: Clone + 'static> Node for Shared<A> {
    const LINEAR: bool = false;
    fn node_token(&self) -> Token {
        self.token
    }
    fn pull_inner<M: Mode>(cx: &mut Cx<'_, M>, index: u32) -> Option<A> {
        cx.cloned::<A>(index)
    }
}

/// The adapter returned by [`Source::map`].
pub struct Map<S, F> {
    source: S,
    f: F,
}
impl<S, F> sealed::Sealed for Map<S, F> {}
impl<S: Source, B, F: Fn(S::Event) -> B + 'static> Source for Map<S, F> {
    type Event = B;
    fn dependency(&self) -> Token {
        self.source.dependency()
    }
    fn read_cells(&self, visit: &mut dyn FnMut(Token)) {
        self.source.read_cells(visit)
    }
    fn pull<M: Mode>(&mut self, cx: &mut Cx<'_, M>) -> Option<B> {
        self.source.pull(cx).map(&self.f)
    }
}

/// The adapter returned by [`Source::filter`].
pub struct Filter<S, P> {
    source: S,
    predicate: P,
}
impl<S, P> sealed::Sealed for Filter<S, P> {}
impl<S: Source, P: Fn(&S::Event) -> bool + 'static> Source for Filter<S, P> {
    type Event = S::Event;
    fn dependency(&self) -> Token {
        self.source.dependency()
    }
    fn read_cells(&self, visit: &mut dyn FnMut(Token)) {
        self.source.read_cells(visit)
    }
    fn pull<M: Mode>(&mut self, cx: &mut Cx<'_, M>) -> Option<S::Event> {
        self.source.pull(cx).filter(|e| (self.predicate)(e))
    }
}

/// The adapter returned by [`Source::filter_map`].
pub struct FilterMap<S, F> {
    source: S,
    f: F,
}
impl<S, F> sealed::Sealed for FilterMap<S, F> {}
impl<S: Source, B, F: Fn(S::Event) -> Option<B> + 'static> Source for FilterMap<S, F> {
    type Event = B;
    fn dependency(&self) -> Token {
        self.source.dependency()
    }
    fn read_cells(&self, visit: &mut dyn FnMut(Token)) {
        self.source.read_cells(visit)
    }
    fn pull<M: Mode>(&mut self, cx: &mut Cx<'_, M>) -> Option<B> {
        self.source.pull(cx).and_then(&self.f)
    }
}

/// The adapter returned by [`Source::map_to`].
pub struct MapTo<S, B> {
    source: S,
    value: B,
}
impl<S, B> sealed::Sealed for MapTo<S, B> {}
impl<S: Source, B: Clone + 'static> Source for MapTo<S, B> {
    type Event = B;
    fn dependency(&self) -> Token {
        self.source.dependency()
    }
    fn read_cells(&self, visit: &mut dyn FnMut(Token)) {
        self.source.read_cells(visit)
    }
    fn pull<M: Mode>(&mut self, cx: &mut Cx<'_, M>) -> Option<B> {
        self.source.pull(cx).map(|_| self.value.clone())
    }
}

/// The adapter returned by [`Source::snapshot`]. `C` is the type of the
/// cell token it reads, a [`Cell`] or a [`State`].
pub struct Snapshot<S, C, F> {
    source: S,
    cell: C,
    f: F,
}
impl<S, C, F> sealed::Sealed for Snapshot<S, C, F> {}
impl<S, C, B, F> Source for Snapshot<S, C, F>
where
    S: Source,
    C: CellRef,
    F: Fn(S::Event, &C::Value) -> B + 'static,
{
    type Event = B;
    fn dependency(&self) -> Token {
        self.source.dependency()
    }
    fn read_cells(&self, visit: &mut dyn FnMut(Token)) {
        visit(self.cell.token());
        self.source.read_cells(visit)
    }
    fn pull<M: Mode>(&mut self, cx: &mut Cx<'_, M>) -> Option<B> {
        let a = self.source.pull(cx)?;
        // The value before the instant: a read, not a dependency.
        Some((self.f)(a, cx.sample::<C::Value>(self.cell.token().index)))
    }
}

/// The adapter returned by [`Source::gate`]. `C` is the type of the cell
/// token it reads, a [`Cell`] or a [`State`] of `bool`.
pub struct Gate<S, C> {
    source: S,
    cell: C,
}
impl<S, C> sealed::Sealed for Gate<S, C> {}
impl<S: Source, C: CellRef<Value = bool>> Source for Gate<S, C> {
    type Event = S::Event;
    fn dependency(&self) -> Token {
        self.source.dependency()
    }
    fn read_cells(&self, visit: &mut dyn FnMut(Token)) {
        visit(self.cell.token());
        self.source.read_cells(visit)
    }
    fn pull<M: Mode>(&mut self, cx: &mut Cx<'_, M>) -> Option<S::Event> {
        // The source is pulled whether or not the gate is open, so a `once`
        // inside it takes its first event even when the gate drops it.
        let a = self.source.pull(cx)?;
        (*cx.sample::<bool>(self.cell.token().index)).then_some(a)
    }
}

/// The adapter returned by [`Source::once`].
pub struct Once<S> {
    source: S,
    done: bool,
}
impl<S> sealed::Sealed for Once<S> {}
impl<S: Source> Source for Once<S> {
    type Event = S::Event;
    fn dependency(&self) -> Token {
        self.source.dependency()
    }
    fn read_cells(&self, visit: &mut dyn FnMut(Token)) {
        self.source.read_cells(visit)
    }
    fn pull<M: Mode>(&mut self, cx: &mut Cx<'_, M>) -> Option<S::Event> {
        if self.done {
            return None;
        }
        let a = self.source.pull(cx)?;
        self.done = true;
        Some(a)
    }
}
