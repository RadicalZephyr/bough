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
//! use bough::{Runtime, Source};
//!
//! let (graph, edge) = Runtime::build(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let (limit, _limit_in) = b.input_cell(10u32);
//!     numbers
//!         .map(|n| n * 2)                 // an adapter: no node, no context
//!         .filter(|n| *n > 2)             // another adapter
//!         .snapshot(limit, |n, l| n.min(*l))
//!         .hold(b, 0u32)                  // one node for the whole chain
//! });
//! let total = edge.keep();
//! ```
//!
//! A chain used twice does not compile:
//!
//! ```compile_fail,E0382
//! use bough::{Runtime, Source};
//!
//! let (graph, edge) = Runtime::build(|b| {
//!     let (numbers, _in) = b.input::<u32>();
//!     let doubled = numbers.map(|n| n * 2);
//!     let a = doubled.hold(b, 0u32);
//!     let b2 = doubled.hold(b, 0u32); // error: use of moved value
//! });
//! edge.keep();
//! ```
//!
//! A chain runs only inside its node. The hidden method that pulls it takes
//! a context graph code cannot name or construct:
//!
//! ```compile_fail,E0433
//! use bough::{Runtime, Source};
//!
//! let (graph, edge) = Runtime::build(|b| {
//!     let (mut numbers, _in) = b.input::<u32>();
//!     let _ = numbers.pull(&mut bough::Cx::new(b)); // error: no `Cx` in `bough`
//! });
//! edge.keep();
//! ```

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::Build;
use crate::cell::CellRef;
use crate::engine::nodes::cell::{AccumulateNode, HoldNode, InPlaceNode, ScanNode};
use crate::engine::nodes::construct::ConstructNode;
use crate::engine::nodes::split::{DeferNode, SplitNode};
use crate::engine::nodes::stream::{ChainNode, MergeNode, SlotNode};
use crate::engine::{COMMITS, Cx, Data, Kind, NodeOps};
use crate::mode::{Accepts, Erase, Mode};
use crate::token::{Cell, Shared, State, Stream, Token};
use crate::trace::{Trace, Tracer};

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
///
/// A chain is [`Trace`]. It visits the tokens its adapters hold, the cells
/// `snapshot` and `gate` read and the value of `map_to`, and ends at its
/// source token. A materializer
/// traces the chain once, when it builds the node: what the trace finds
/// besides the dependency is the node's reach (RFD 3), since the chain never
/// changes after it is built. A closure is opaque to the trace, so a token a
/// closure captures is declared with [`Build::depends`].
pub trait Source: Sized + 'static + sealed::Sealed + Trace {
    /// The type of each event.
    type Event;

    /// The one node this chain reads events from: its dependency.
    #[doc(hidden)]
    fn dependency(&self) -> Token;

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
    ///
    /// The chain keeps the value for as long as its node lives, so the value
    /// is [`Trace`], like every value the graph keeps: the node is traced
    /// when it is built, and a token in the value is in its reach, with no
    /// declaration (RFD 3). A foreign type that holds no tokens goes in a
    /// [`Leaf`](crate::Leaf); one that is not `Trace` does not compile:
    ///
    /// ```compile_fail,E0277
    /// use bough::{Runtime, Source};
    ///
    /// #[derive(Clone)]
    /// struct Label(&'static str);
    ///
    /// let (graph, edge) = Runtime::build(|b| {
    ///     let (clicks, _clicks_in) = b.input::<()>();
    ///     let _ = clicks.map_to(Label("clicked")).node(b); // error: Label is not Trace
    /// });
    /// edge.keep();
    /// ```
    fn map_to<B>(self, value: B) -> MapTo<Self, B>
    where
        B: Clone + Trace + 'static,
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
    /// node, so the mode must accept its type; a `Threaded` graph refuses an
    /// `Rc` event here:
    ///
    /// ```compile_fail,E0277
    /// use bough::{Runtime, Source};
    /// use std::rc::Rc;
    ///
    /// let (_graph, edge) = Runtime::build_threaded(|b| {
    ///     let (numbers, _numbers_in) = b.input::<u32>();
    ///     let _count = numbers
    ///         .map(Rc::new)
    ///         .accumulate_mut(b, 0u32, |_, n: &mut u32| *n += 1); // error: Rc is not Send
    /// });
    /// edge.keep();
    /// ```
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
    /// per event.
    ///
    /// The state is private to the node and is updated when the node runs,
    /// at most once per transaction, so `f` always reads the state from
    /// before the instant, as the semantics' snapshot of a hold would.
    ///
    /// The output goes in the node's slot, so the mode must accept its
    /// type; a `Threaded` graph refuses an `Rc` output here:
    ///
    /// ```compile_fail,E0277
    /// use bough::{Runtime, Source};
    /// use std::rc::Rc;
    ///
    /// let (_graph, edge) = Runtime::build_threaded(|b| {
    ///     let (numbers, _numbers_in) = b.input::<u32>();
    ///     let _numbered = numbers.scan(b, 0u32, |n, k| (Rc::new(n), k + 1)); // error: Rc is not Send
    /// });
    /// edge.keep();
    /// ```
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
    /// use bough::{Runtime, Source};
    /// use std::rc::Rc;
    ///
    /// let (_graph, edge) = Runtime::build_threaded(|b| {
    ///     let (numbers, _numbers_in) = b.input::<u32>();
    ///     let _stream = numbers.map(Rc::new).node(b); // error: Rc is not Send
    /// });
    /// edge.keep();
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

    /// Emits the elements of each event in child transactions: element n
    /// of an event at transaction t in the child transaction `t ++ [n]`.
    ///
    /// The child transactions of t run after t's listeners, one after
    /// another, and before anything the I/O side sends next. Each is a
    /// whole transaction: cells step in it, a snapshot in a later child
    /// reads what an earlier one committed, and listeners run after each
    /// child's commit. [`Runtime::send`](crate::Runtime::send) and
    /// [`Runtime::transaction`](crate::Runtime::transaction) return after the
    /// last of them, so a sample then reads what it committed. The build
    /// closure's transaction has children too: a split of a
    /// [`steps_with_current`](Cell::steps_with_current) built there runs
    /// them before [`Runtime::build`](crate::Runtime::build) returns.
    ///
    /// ```
    /// use std::cell::RefCell;
    /// use std::rc::Rc;
    ///
    /// use bough::{Runtime, Source};
    ///
    /// let (mut graph, edge) = Runtime::build(|b| {
    ///     let (words, words_in) = b.input::<Vec<char>>();
    ///     let letters = words.split(b).share(b);
    ///     let count = letters.accumulate(b, 0u32, |_, n| n + 1);
    ///     (words_in, letters, count)
    /// });
    /// let (words_in, letters, count) = edge.keep();
    /// let seen = Rc::new(RefCell::new(Vec::new()));
    /// let log = seen.clone();
    /// graph.listen(letters, move |c| log.borrow_mut().push(c)).keep();
    /// graph.send(words_in, vec!['a', 'b', 'c']); // three child transactions
    /// assert_eq!(*seen.borrow(), ['a', 'b', 'c']);
    /// assert_eq!(*graph.sample(count), 3); // what the last child committed
    /// ```
    ///
    /// Every split that fires at t shares t's children: child n carries
    /// element n of each, so those elements are simultaneous, and a
    /// [`merge`](Source::merge) of two splits combines them. A
    /// [`defer`](Source::defer) is a split of one element, so its event
    /// joins element 0. An empty event emits nothing. A split that fires
    /// inside a child, over another split's elements or through a loop, has
    /// children of that child, which run before the next child of t: depth
    /// first, which is time order.
    ///
    /// The output does not depend on the input, since it fires in a later
    /// instant, so a loop through a split is legal (the rule is
    /// [`Build::cell_loop`]'s), and each round of it runs one level of
    /// child transactions deeper.
    ///
    /// A split is two nodes: one takes the event, the other emits the
    /// elements. The iterator waits in the first between child
    /// transactions and each element in the second's slot, so the mode
    /// must accept both types; a `Threaded` graph refuses `Rc` elements:
    ///
    /// ```compile_fail,E0277
    /// use bough::{Runtime, Source};
    /// use std::rc::Rc;
    ///
    /// let (_graph, edge) = Runtime::build_threaded(|b| {
    ///     let (numbers, _numbers_in) = b.input::<u32>();
    ///     let _items = numbers.map(|n| vec![Rc::new(n)]).split(b); // error: Rc is not Send
    /// });
    /// edge.keep();
    /// ```
    fn split<M>(self, build: &mut Build<M>) -> Stream<<Self::Event as IntoIterator>::Item>
    where
        M: Mode
            + Accepts<Self>
            + Accepts<<Self::Event as IntoIterator>::IntoIter>
            + Accepts<<Self::Event as IntoIterator>::Item>,
        Self::Event: IntoIterator + 'static,
        <Self::Event as IntoIterator>::Item: 'static,
    {
        let (dependency, cells) = build.chain_reach(&self);
        let parts: Box<[M::Carrier]> = Box::new([
            <M as Accepts<Self>>::erase(Erase::Value(self)),
            <M as Accepts<<Self::Event as IntoIterator>::IntoIter>>::erase(Erase::Stack),
        ]);
        let slot = <M as Accepts<<Self::Event as IntoIterator>::Item>>::erase(Erase::Slot);
        let ops = &<SplitNode<Self> as NodeOps<M>>::OPS;
        let output_ops = &<SlotNode<<Self::Event as IntoIterator>::Item> as NodeOps<M>>::OPS;
        let output = build.capture_pair(dependency, cells, parts, ops, slot, output_ops);
        Stream::from_token(build.token(output))
    }

    /// Emits each event again in the first child transaction of the
    /// transaction t it fires in, `t ++ [0]`: after t's listeners, and
    /// before anything the I/O side sends next.
    ///
    /// That child is not the defer's own. The semantics' `defer` is a
    /// [`split`](Source::split) of a one-element list, so it shares index
    /// 0 with every split and every other defer that fires at t: the
    /// deferred event is simultaneous with their events there, and a
    /// [`merge`](Source::merge) combines them.
    ///
    /// The output does not depend on the input, since it fires in a later
    /// instant, so a loop through a defer is legal (the rule is
    /// [`Build::cell_loop`]'s), and each round of it runs one child
    /// transaction deeper. A countdown:
    ///
    /// ```
    /// use std::cell::RefCell;
    /// use std::rc::Rc;
    ///
    /// use bough::{Runtime, Source};
    ///
    /// let (mut graph, edge) = Runtime::build(|b| {
    ///     let (counts, counts_loop) = b.stream_loop::<u32>();
    ///     let again = counts.filter(|n| *n > 1).map(|n| n - 1).defer(b);
    ///     let (starts, starts_in) = b.input::<u32>();
    ///     let counts = starts.or_else(b, again).share(b);
    ///     counts_loop.close(b, counts);
    ///     (starts_in, counts)
    /// });
    /// let (starts_in, counts) = edge.keep();
    /// let seen = Rc::new(RefCell::new(Vec::new()));
    /// let log = seen.clone();
    /// graph.listen(counts, move |n| log.borrow_mut().push(n)).keep();
    /// graph.send(starts_in, 3); // 3 at [1], 2 at [1, 0], 1 at [1, 0, 0]
    /// assert_eq!(*seen.borrow(), [3, 2, 1]);
    /// ```
    ///
    /// Such a loop needs something that stops it, here the filter. A loop
    /// through a defer whose every event comes back never ends, and
    /// neither does the `send` that started it: the semantics give it
    /// infinitely many events before the next transaction, and no check at
    /// close can tell a loop that stops from one that does not, so the
    /// engine does not guard against it. It keeps one level of child
    /// transactions in progress per round, so a loop that does stop grows
    /// memory with its depth, not the stack.
    ///
    /// A defer is two nodes: one takes the event, the other emits it. The
    /// event waits in the first until the child transaction, and in the
    /// second's slot after it, so the mode must accept its type; a
    /// `Threaded` graph refuses `Rc` events:
    ///
    /// ```compile_fail,E0277
    /// use bough::{Runtime, Source};
    /// use std::rc::Rc;
    ///
    /// let (_graph, edge) = Runtime::build_threaded(|b| {
    ///     let (numbers, _numbers_in) = b.input::<u32>();
    ///     let _later = numbers.map(Rc::new).defer(b); // error: Rc is not Send
    /// });
    /// edge.keep();
    /// ```
    fn defer<M>(self, build: &mut Build<M>) -> Stream<Self::Event>
    where
        M: Mode + Accepts<Self> + Accepts<Self::Event>,
        Self::Event: 'static,
    {
        let (dependency, cells) = build.chain_reach(&self);
        let parts: Box<[M::Carrier]> = Box::new([
            <M as Accepts<Self>>::erase(Erase::Value(self)),
            <M as Accepts<Self::Event>>::erase(Erase::Stack),
        ]);
        let slot = <M as Accepts<Self::Event>>::erase(Erase::Slot);
        let ops = &<DeferNode<Self> as NodeOps<M>>::OPS;
        let output_ops = &<SlotNode<Self::Event> as NodeOps<M>>::OPS;
        let output = build.capture_pair(dependency, cells, parts, ops, slot, output_ops);
        Stream::from_token(build.token(output))
    }

    /// The semantics' `Execute`: runs `f` at each event with a fresh
    /// build context, so graph can be constructed at runtime. Its results
    /// are ordinary events: tokens on their way to a hold and a
    /// [`Cell::switch_stream`] or [`Cell::switch_cell`], or plain values.
    ///
    /// `f` runs in the middle of the transaction t of the event, and what
    /// it builds exists from t on, t included: a hold built over a stream
    /// that fires at t holds that event after t, a
    /// [`steps_with_current`](Cell::steps_with_current) fires at t, and a
    /// switch starts at t from the inner its outer held before t, moving
    /// after t if the outer steps at t. A [`sample`](Cell::sample) inside
    /// `f` reads the value before t, as every read during a transaction
    /// does. The new nodes run at t once `f` has returned and this node has
    /// fired, each after what it depends on, so they may depend on this
    /// node's own events, through a loop.
    ///
    /// Tokens created inside `f` flow out as data. An input built there
    /// reaches I/O code as an event, and a collection runs after each unit,
    /// so `f` anchors what it sends out with
    /// [`Build::anchor`](crate::Build::anchor). Since a listener has no graph
    /// access, I/O code attaches listeners and sends to the input after
    /// [`Runtime::send`](crate::Runtime::send) returns: anchor it at the edge.
    ///
    /// ```
    /// use std::cell::RefCell;
    /// use std::rc::Rc;
    ///
    /// use bough::{Runtime, Source};
    ///
    /// // Each event opens a counter of its own: an input and a hold over it.
    /// let (mut graph, edge) = Runtime::build(|b| {
    ///     let (open, open_in) = b.input::<u32>();
    ///     let opened = open.construct(b, |b, start| {
    ///         let (bumps, bumps_in) = b.input::<u32>();
    ///         let count = bumps.accumulate(b, start, |n, c| c + n);
    ///         b.anchor((bumps_in, count))
    ///     });
    ///     (open_in, opened)
    /// });
    /// let (open_in, opened) = edge.keep();
    /// let received = Rc::new(RefCell::new(Vec::new()));
    /// let log = received.clone();
    /// graph.listen(opened, move |counter| log.borrow_mut().push(counter)).keep();
    /// graph.send(open_in, 10); // the row arrives anchored
    /// let (bumps_in, count) = *received.borrow()[0];
    /// graph.send(bumps_in, 5); // then wire
    /// assert_eq!(*graph.sample(count), 15);
    /// ```
    ///
    /// Each run of `f` is a scope, as the build closure is: a loop declared
    /// in it must close in it, under the rule [`Build::cell_loop`] states,
    /// and a loop left open is a panic when `f` returns. So are a close,
    /// and a switch's first link, that would make a same-instant cycle, the
    /// switch's at the end of the transaction's new nodes. Each of these
    /// panics poisons the graph, as does a panic in `f` itself, and so
    /// does a run that swaps its build context for another graph's.
    ///
    /// A run allocates the nodes it builds, into slots collection freed
    /// if there are any. They live while a root reaches them: a hold that
    /// a switch reads keeps the screen it holds, and a screen the switch
    /// has left, which nothing names any more, is collected (RFD 3). A
    /// token the closure captures that this construct's stream does not
    /// depend on is declared with [`Build::depends`], or it may be
    /// collected before the closure's next run uses it; a token the
    /// closure hands I/O code as data is anchored there. A construct whose
    /// chain does not fire costs what a chain node costs.
    ///
    /// The event waits in the node's slot for its consumer, and keeps
    /// there until the next event if nothing consumes it, so the mode must
    /// accept its type; a `Threaded` graph refuses a construct of `Rc`s:
    ///
    /// ```compile_fail,E0277
    /// use bough::{Runtime, Source};
    /// use std::rc::Rc;
    ///
    /// let (_graph, edge) = Runtime::build_threaded(|b| {
    ///     let (numbers, _numbers_in) = b.input::<u32>();
    ///     let _made = numbers.construct(b, |_, n| Rc::new(n)); // error: Rc is not Send
    /// });
    /// edge.keep();
    /// ```
    fn construct<M, B, F>(self, build: &mut Build<M>, f: F) -> Stream<B>
    where
        M: Mode + Accepts<Self> + Accepts<F> + Accepts<B>,
        B: 'static,
        F: FnMut(&mut Build<M>, Self::Event) -> B + 'static,
    {
        let (dependency, cells) = build.chain_reach(&self);
        let data = Data::Slot(<M as Accepts<B>>::erase(Erase::Slot));
        let parts: Box<[M::Carrier]> = Box::new([
            <M as Accepts<Self>>::erase(Erase::Value(self)),
            <M as Accepts<F>>::erase(Erase::Value(f)),
        ]);
        let ops = &<ConstructNode<Self, F, B> as NodeOps<M>>::OPS;
        let n = build.materialize(Kind::Stream, data, parts, ops, &[dependency], 0);
        build.set_reach(n, cells);
        Stream::from_token(build.token(n))
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
    /// Checks a chain's dependency and the tokens its trace visits, and
    /// returns their indices: the dependency, and the reach, which is every
    /// token besides the dependency. A foreign or stale token is a
    /// build-time panic.
    pub(crate) fn chain_reach<S: Source>(&self, chain: &S) -> (u32, Vec<u32>) {
        let dependency = self.check(chain.dependency());
        let mut tracer = Tracer::new();
        chain.trace(&mut tracer);
        let reach = tracer
            .visited
            .into_iter()
            .map(|token| self.check(token))
            .filter(|&index| index != dependency)
            .collect();
        (dependency, reach)
    }

    /// Records what a new node keeps alive beyond its dependencies.
    pub(crate) fn set_reach(&mut self, n: u32, cells: Vec<u32>) {
        if !cells.is_empty() {
            self.store.cold[n as usize].reach = cells;
        }
    }
}

/// A materialized node: what [`Runtime::listen`](crate::Runtime::listen) accepts.
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
    fn pull<M: Mode>(&mut self, cx: &mut Cx<'_, M>) -> Option<B> {
        self.source.pull(cx).map(&self.f)
    }
}
impl<S: Trace, F> Trace for Map<S, F> {
    /// The source alone: a closure is opaque (RFD 3).
    fn trace(&self, tracer: &mut Tracer) {
        self.source.trace(tracer)
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
    fn pull<M: Mode>(&mut self, cx: &mut Cx<'_, M>) -> Option<S::Event> {
        self.source.pull(cx).filter(|e| (self.predicate)(e))
    }
}
impl<S: Trace, P> Trace for Filter<S, P> {
    /// The source alone: a closure is opaque (RFD 3).
    fn trace(&self, tracer: &mut Tracer) {
        self.source.trace(tracer)
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
    fn pull<M: Mode>(&mut self, cx: &mut Cx<'_, M>) -> Option<B> {
        self.source.pull(cx).and_then(&self.f)
    }
}
impl<S: Trace, F> Trace for FilterMap<S, F> {
    /// The source alone: a closure is opaque (RFD 3).
    fn trace(&self, tracer: &mut Tracer) {
        self.source.trace(tracer)
    }
}

/// The adapter returned by [`Source::map_to`].
pub struct MapTo<S, B> {
    source: S,
    value: B,
}
impl<S, B> sealed::Sealed for MapTo<S, B> {}
impl<S: Source, B: Clone + Trace + 'static> Source for MapTo<S, B> {
    type Event = B;
    fn dependency(&self) -> Token {
        self.source.dependency()
    }
    fn pull<M: Mode>(&mut self, cx: &mut Cx<'_, M>) -> Option<B> {
        self.source.pull(cx).map(|_| self.value.clone())
    }
}
impl<S: Trace, B: Trace> Trace for MapTo<S, B> {
    /// The value every event carries, then the source.
    fn trace(&self, tracer: &mut Tracer) {
        self.value.trace(tracer);
        self.source.trace(tracer)
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
    fn pull<M: Mode>(&mut self, cx: &mut Cx<'_, M>) -> Option<B> {
        let a = self.source.pull(cx)?;
        // The value before the instant: a read, not a dependency.
        Some((self.f)(a, cx.sample::<C::Value>(self.cell.token().index)))
    }
}
impl<S: Trace, C: CellRef, F> Trace for Snapshot<S, C, F> {
    /// The cell it reads, which is reach and never a dependency, since it
    /// is read as it was before the instant; then the source. The closure is
    /// opaque (RFD 3).
    fn trace(&self, tracer: &mut Tracer) {
        tracer.visit(&self.cell);
        self.source.trace(tracer)
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
    fn pull<M: Mode>(&mut self, cx: &mut Cx<'_, M>) -> Option<S::Event> {
        // The source is pulled whether or not the gate is open, so a `once`
        // inside it takes its first event even when the gate drops it.
        let a = self.source.pull(cx)?;
        (*cx.sample::<bool>(self.cell.token().index)).then_some(a)
    }
}
impl<S: Trace, C: CellRef> Trace for Gate<S, C> {
    /// The cell it reads, which is reach and never a dependency, since it
    /// is read as it was before the instant; then the source.
    fn trace(&self, tracer: &mut Tracer) {
        tracer.visit(&self.cell);
        self.source.trace(tracer)
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
    fn pull<M: Mode>(&mut self, cx: &mut Cx<'_, M>) -> Option<S::Event> {
        if self.done {
            return None;
        }
        let a = self.source.pull(cx)?;
        self.done = true;
        Some(a)
    }
}
impl<S: Trace> Trace for Once<S> {
    fn trace(&self, tracer: &mut Tracer) {
        self.source.trace(tracer)
    }
}
