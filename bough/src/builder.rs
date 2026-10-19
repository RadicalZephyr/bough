//! Builders: [`Builder`], the one trait every materializer takes, and
//! [`Portable`], the builder a core is written against.
//!
//! A materializer takes `&mut C` for any [`Builder`] `C`, and asks
//! [`Stores<T>`] of `C` for each value and closure it keeps. A
//! [`BuildContext<M>`](crate::BuildContext) stores what its mode [`Accepts`]; a
//! [`Portable`] stores what is `Send`, whichever build context it borrows. So
//! `stream.hold(b, x)` reads the same with all three, and a portable core
//! can store a closure it writes itself, since the bound names `Portable`,
//! not the closure.
//!
//! The hidden methods are the engine's primitives, each typed by the node
//! it makes rather than by the mode: a `BuildContext<M>` takes its mode's
//! functions, and a `Portable` matches on the build context it borrows
//! and takes that one's. A portable chain is monomorphized once per mode it
//! may run in, because the node functions stay per mode.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::any::Any;

use crate::build::{BuildContext, CellLoop, StateLoop, StreamLoop};
use crate::cell::CellRef;
use crate::engine::COMMITS;
use crate::engine::nodes::DefaultNode;
use crate::engine::nodes::cell::{ConstantNode, HoldNode};
use crate::engine::nodes::construct::{ConstructNode, Direct, ThroughPortable};
use crate::engine::nodes::stream::{CoalescingInput, SlotNode};
use crate::engine::{Data, Kind, NodeOps};
use crate::guard::Liveness;
#[cfg(target_has_atomic = "ptr")]
use crate::mode::Threaded;
use crate::mode::{Accepts, Erase, Local, Mode};
use crate::runtime::{Anchor, Anchored};
use crate::source::{Node, Source};
use crate::token::Token;
use crate::token::{Cell, Input, State, Stream, TokenRef};
use crate::trace::{Trace, Tracer};

pub(crate) mod sealed {
    pub trait Sealed {}
}

/// A node type with functions for every mode the target has, so a builder
/// that is not one mode can take whichever the build context it borrows needs.
#[doc(hidden)]
#[cfg(target_has_atomic = "ptr")]
pub trait AllOps: NodeOps<Local> + NodeOps<Threaded> {}
#[cfg(target_has_atomic = "ptr")]
impl<N: NodeOps<Local> + NodeOps<Threaded>> AllOps for N {}

/// A node type with functions for every mode the target has: `Local`
/// alone on a target with no pointer atomics.
#[doc(hidden)]
#[cfg(not(target_has_atomic = "ptr"))]
pub trait AllOps: NodeOps<Local> {}
#[cfg(not(target_has_atomic = "ptr"))]
impl<N: NodeOps<Local>> AllOps for N {}

/// A node's data plane before it is in a mode's carrier: [`Data`] over a
/// builder's erased type.
#[doc(hidden)]
pub enum Plane<E> {
    Empty,
    Slot(E),
    Cell(E),
    InPlace { state: E, pending: E },
    ReadThrough { f: E, memo: E },
}

impl<E> Plane<E> {
    fn map<D>(self, into: impl Fn(E) -> D) -> Plane<D> {
        match self {
            Plane::Empty => Plane::Empty,
            Plane::Slot(e) => Plane::Slot(into(e)),
            Plane::Cell(e) => Plane::Cell(into(e)),
            Plane::InPlace { state, pending } => Plane::InPlace {
                state: into(state),
                pending: into(pending),
            },
            Plane::ReadThrough { f, memo } => Plane::ReadThrough {
                f: into(f),
                memo: into(memo),
            },
        }
    }

    fn into_data<M: Mode>(self, into: impl Fn(E) -> M::Carrier) -> Data<M> {
        match self {
            Plane::Empty => Data::Empty,
            Plane::Slot(e) => Data::Slot(into(e)),
            Plane::Cell(e) => Data::Cell(into(e)),
            Plane::InPlace { state, pending } => Data::InPlace {
                state: into(state),
                pending: into(pending),
            },
            Plane::ReadThrough { f, memo } => Data::ReadThrough {
                f: into(f),
                memo: into(memo),
            },
        }
    }
}

/// What every node-creating operation takes: a [`BuildContext`] of either mode,
/// or a [`Portable`] lent by one. Sealed.
///
/// Its public methods are the builder's own operations, `input`,
/// `constant`, the loops, `depends` and `anchor`, defined once here for
/// every builder; the rest are hidden. [`prelude`](crate::prelude) brings
/// it into scope.
pub trait Builder: sealed::Sealed + Sized {
    /// What [`Stores::erase`] makes: the mode's carrier for a `BuildContext`, a
    /// `Send` box for a `Portable`.
    #[doc(hidden)]
    type Erased: 'static;

    /// What a `construct` closure built with this builder receives.
    #[doc(hidden)]
    type Inner<'p>: Builder;

    /// Erases a value the engine makes that is `Send` whatever the user's
    /// types are.
    #[doc(hidden)]
    fn erase_send<T: Send + 'static>(value: T) -> Self::Erased;

    /// Checks a token: a foreign or stale one panics.
    #[doc(hidden)]
    fn check_token(&self, t: Token) -> u32;

    /// The token of a node of this graph.
    #[doc(hidden)]
    fn token_of(&self, n: u32) -> Token;

    /// A chain's dependency and reach, checked.
    #[doc(hidden)]
    fn reach_of<S: Source>(&self, chain: &S) -> (u32, Vec<u32>);

    /// Creates a node of type `N`, with the reach given.
    #[doc(hidden)]
    fn make<N: AllOps>(
        &mut self,
        kind: Kind,
        data: Plane<Self::Erased>,
        parts: Box<[Self::Erased]>,
        deps: &[u32],
        flags: u8,
        reach: Vec<u32>,
    ) -> u32;

    /// A split or defer's capture of type `N` and output of type `O`.
    #[doc(hidden)]
    fn make_pair<N: AllOps, O: AllOps>(
        &mut self,
        dependency: u32,
        reach: Vec<u32>,
        parts: Box<[Self::Erased]>,
        slot: Self::Erased,
    ) -> u32;

    /// A switch_cell over the cell `outer`.
    #[doc(hidden)]
    fn make_switch_cell<C: CellRef>(&mut self, outer: Token) -> Token;

    /// A switch_stream over the cell `outer`, with its slot.
    #[doc(hidden)]
    fn make_switch_stream<S: Node>(&mut self, outer: Token, slot: Self::Erased) -> Token
    where
        S::Event: 'static;

    /// Closes a cell loop.
    #[doc(hidden)]
    fn close_cell(&mut self, forward: Token, definition: Token);

    /// Closes a stream loop with a chain of node type `N`, already erased.
    #[doc(hidden)]
    fn close_stream<N: AllOps>(
        &mut self,
        forward: Token,
        dependency: u32,
        reach: Vec<u32>,
        slot: Self::Erased,
        chain: Self::Erased,
    );

    /// A construct node over a chain `S` running an `F`, both erased in
    /// `parts`, with its slot.
    #[doc(hidden)]
    fn make_construct<S, B, F>(
        &mut self,
        dependency: u32,
        reach: Vec<u32>,
        slot: Self::Erased,
        parts: Box<[Self::Erased]>,
    ) -> u32
    where
        S: Source,
        B: 'static,
        F: for<'p> FnMut(&mut Self::Inner<'p>, S::Event) -> B + 'static;

    /// A cell's value before the instant.
    #[doc(hidden)]
    fn read<A: 'static>(&self, t: Token) -> &A;

    /// A loop's forward node, open in the current scope: a cell loop's,
    /// or with `stream` a stream loop's.
    #[doc(hidden)]
    fn open_forward(&mut self, stream: bool) -> Token;

    /// Adds to what node `n` reaches beyond its dependencies.
    #[doc(hidden)]
    fn extend_reach(&mut self, n: u32, reach: Vec<u32>);

    /// Roots the nodes until the anchor this returns drops.
    #[doc(hidden)]
    fn root(&mut self, nodes: Vec<u32>) -> Anchor;

    // ----- the builder's own operations, one definition for every builder

    /// Anchors what a build closure or a
    /// [`construct`](crate::Source::construct) closure sends out to I/O
    /// code, as [`Runtime::anchor`](crate::Runtime::anchor) does: the
    /// [`Anchored`] it returns carries the value, and roots every token its
    /// [`Trace`] finds until the last of its clones drops. A construct that
    /// returns one sends its row across the boundary already rooted, so the I/O code
    /// that receives it needn't anchor it.
    ///
    /// ```
    /// use std::cell::RefCell;
    /// use std::rc::Rc;
    ///
    /// use bough::prelude::*;
    ///
    /// let mut graph = Runtime::local();
    /// let (open_in, opened) = graph.build_here(|b| {
    ///     let (open, open_in) = b.input::<u32>();
    ///     let opened = open.construct(b, |b, start| {
    ///         let (bumps, bumps_in) = b.input::<u32>();
    ///         let count = bumps.accumulate(b, start, |n, c| c + n);
    ///         b.anchor((bumps_in, count))
    ///     });
    ///     (open_in, opened)
    /// });
    /// graph.set_collect_after_every_unit(true); // a test setting
    /// let received = Rc::new(RefCell::new(Vec::new()));
    /// let log = received.clone();
    /// graph.listen(opened, move |counter| log.borrow_mut().push(counter)).keep();
    /// graph.send(open_in, 10); // the row arrives anchored
    /// let (bumps_in, count) = *received.borrow()[0];
    /// graph.send(bumps_in, 5);
    /// assert_eq!(*graph.sample(count), 15);
    /// ```
    fn anchor<T: Trace>(&mut self, value: T) -> Anchored<T> {
        let mut tracer = Tracer::new();
        value.trace(&mut tracer);
        // Every token is checked before any is rooted.
        let nodes: Vec<u32> = tracer
            .visited
            .into_iter()
            .map(|t| self.check_token(t))
            .collect();
        Anchored::new(value, self.root(nodes))
    }

    /// A stream driven from I/O code, and the token that drives it.
    ///
    /// The input's slot is created here and keeps an event nobody consumed
    /// until the next send, so the mode must accept the event type.
    fn input<A: 'static>(&mut self) -> (Stream<A>, Input<A>)
    where
        Self: Stores<A>,
    {
        let data = Plane::Slot(<Self as Stores<A>>::erase(Erase::Slot));
        let n = self.make::<SlotNode<A>>(Kind::Input, data, Box::new([]), &[], 0, Vec::new());
        let t = self.token_of(n);
        (Stream::from_token(t), Input::from_token(t))
    }

    /// An input that may be sent more than once in a transaction; `f`
    /// combines the values, first send on the left.
    fn input_coalescing<A, F>(&mut self, f: F) -> (Stream<A>, Input<A>)
    where
        A: 'static,
        F: Fn(A, A) -> A + 'static,
        Self: Stores<A> + Stores<F>,
    {
        let data = Plane::Slot(<Self as Stores<A>>::erase(Erase::Slot));
        let parts: Box<[Self::Erased]> = Box::new([<Self as Stores<F>>::erase(Erase::Value(f))]);
        let n = self.make::<CoalescingInput<A, F>>(Kind::Input, data, parts, &[], 0, Vec::new());
        let t = self.token_of(n);
        (Stream::from_token(t), Input::from_token(t))
    }

    /// A cell driven from I/O code: a hold over an input, two nodes.
    fn input_cell<A>(&mut self, initial: A) -> (Cell<A>, Input<A>)
    where
        A: Trace + 'static,
        Self: Stores<A>,
    {
        let (stream, input) = self.input::<A>();
        (hold_input(self, stream, initial), input)
    }

    /// A cell driven from I/O code whose input coalesces.
    fn input_cell_coalescing<A, F>(&mut self, initial: A, f: F) -> (Cell<A>, Input<A>)
    where
        A: Trace + 'static,
        F: Fn(A, A) -> A + 'static,
        Self: Stores<A> + Stores<F>,
    {
        let (stream, input) = self.input_coalescing::<A, F>(f);
        (hold_input(self, stream, initial), input)
    }

    /// A cell that never changes.
    fn constant<A>(&mut self, value: A) -> Cell<A>
    where
        A: Trace + 'static,
        Self: Stores<A>,
    {
        let data = Plane::Cell(<Self as Stores<A>>::erase(Erase::Cell(value)));
        let n =
            self.make::<ConstantNode<A>>(Kind::Constant, data, Box::new([]), &[], 0, Vec::new());
        Cell::from_token(self.token_of(n))
    }

    /// A stream that never fires.
    ///
    /// Its node stores nothing: a reader checks the stamp before the slot,
    /// and this node's stamp never matches.
    fn never<A: 'static>(&mut self) -> Stream<A> {
        let n =
            self.make::<DefaultNode>(Kind::Never, Plane::Empty, Box::new([]), &[], 0, Vec::new());
        Stream::from_token(self.token_of(n))
    }

    /// Declares a cell loop: a forward token usable anywhere, and the closer
    /// that later defines it. Once closed, the forward token is its
    /// definition: it reads through to the definition's value and steps
    /// when the definition steps.
    ///
    /// ```
    /// use bough::prelude::*;
    ///
    /// let mut graph = Runtime::local();
    /// let (ticks_in, count) = graph.build_here(|b| {
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
    /// `switch_stream`'s selection, nor [`depends`](Builder::depends); and
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
    /// [`state_loop`](Builder::state_loop) instead.
    fn cell_loop<A: 'static>(&mut self) -> (Cell<A>, CellLoop<A>) {
        let token = self.open_forward(false);
        (Cell::from_token(token), CellLoop::new(token))
    }

    /// Declares a loop whose definition is a [`State`]: the state of an
    /// in-place accumulator, or a read-through cell over one. The forward
    /// token is a `State` too, so no stream view of it can exist, which
    /// such a definition could not give a value after the instant to. It
    /// may also close with a [`Cell`], giving a forward that only reads.
    ///
    /// ```
    /// use bough::prelude::*;
    ///
    /// let mut graph = Runtime::local();
    /// let (names_in, members) = graph.build_here(|b| {
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
    /// The rule is the one [`cell_loop`](Builder::cell_loop) states: the
    /// dependency graph stays acyclic, and a read of a cell's value from
    /// before the instant is not a dependency.
    fn state_loop<A: 'static>(&mut self) -> (State<A>, StateLoop<A>) {
        let token = self.open_forward(false);
        (State::from_token(token), StateLoop::new(token))
    }

    /// Declares a stream loop: an affine forward stream, and the closer that
    /// later defines it with any chain. Close fuses the chain into the
    /// forward's own node, so a stream loop adds no node to its definition.
    ///
    /// ```
    /// use bough::prelude::*;
    ///
    /// // A running total, fed back through a hold that a snapshot reads.
    /// let mut graph = Runtime::local();
    /// let (numbers_in, total) = graph.build_here(|b| {
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
    /// The rule is the one [`cell_loop`](Builder::cell_loop) states: the
    /// dependency graph stays acyclic, and a read of a cell's value from
    /// before the instant is not a dependency. So the definition above may
    /// snapshot a hold of the forward, and a loop through such a read needs
    /// no `split` or `defer`. A loop through a `split` or a `defer` is legal
    /// too, since their output fires in a later child instant. A chain
    /// whose events come from the forward itself, or from anything that
    /// depends on it, a hold's `steps` included, is refused at close.
    fn stream_loop<A: 'static>(&mut self) -> (Stream<A>, StreamLoop<A>) {
        let token = self.open_forward(true);
        (Stream::from_token(token), StreamLoop::new(token))
    }

    /// Declares that `node` keeps alive every token the values in `on`
    /// hold, for a closure that captures them (RFD 3). Each value is traced,
    /// so a token counts wherever it sits: bare, or in a struct, a tuple or a
    /// collection at any depth. One declaration per closure; the slice is
    /// heterogeneous, and takes references, so an affine stream is not
    /// consumed.
    ///
    /// A node is alive while a root reaches it: the build closure's return
    /// value, a live listener, or a live [`Anchor`](crate::Anchor). What a
    /// node reaches is what it depends on, the tokens in a stateful cell's
    /// committed value and in its chain, and what this declares. A closure
    /// is the one thing the collector cannot look inside, so every value a
    /// `move` closure captures that holds a token is declared here, on the
    /// node the closure belongs to, which for an adapter is the node its
    /// chain becomes. Otherwise a node the closure names is collected when
    /// nothing else reaches it, and the closure's next use of its token is a
    /// stale-token error. Here the map emits a cell nothing else names:
    ///
    /// ```
    /// use bough::prelude::*;
    ///
    /// let mut graph = Runtime::local();
    /// let (pick_in, shown) = graph.build_here(|b| {
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
    /// graph.set_collect_after_every_unit(true); // a test setting
    /// graph.send(pick_in, true);
    /// assert_eq!(graph.sample(shown), "bonjour");
    /// ```
    ///
    /// Declaring a token the node reaches anyway costs one entry. A
    /// declaration is reach, not a dependency: it orders no evaluation and
    /// can close no cycle. Every token is checked before any is recorded,
    /// and a stale or foreign one panics, as every build-time use of one
    /// does.
    fn depends(&mut self, node: &impl TokenRef, on: &[&dyn Trace]) {
        let n = self.check_token(node.token());
        let mut tracer = Tracer::new();
        for value in on {
            value.trace(&mut tracer);
        }
        let reach: Vec<u32> = tracer
            .visited
            .into_iter()
            .map(|token| self.check_token(token))
            .collect();
        self.extend_reach(n, reach);
    }
}

/// The hold of an input cell. Its chain is the input's own stream token,
/// which is `Send` whatever `A` is, so it is erased through `erase_send`
/// and needs no `Stores<Stream<A>>` bound.
fn hold_input<C, A>(builder: &mut C, stream: Stream<A>, initial: A) -> Cell<A>
where
    C: Builder + Stores<A>,
    A: Trace + 'static,
{
    let input = builder.check_token(stream.token);
    let data = Plane::Cell(<C as Stores<A>>::erase(Erase::Cell(initial)));
    let parts: Box<[C::Erased]> = Box::new([C::erase_send(stream)]);
    let n =
        builder.make::<HoldNode<Stream<A>>>(Kind::Hold, data, parts, &[input], COMMITS, Vec::new());
    Cell::from_token(builder.token_of(n))
}

/// Whether a builder may store a `T`: a `BuildContext<M>` when `M:`
/// [`Accepts<T>`], a [`Portable`] when `T: Send`.
///
/// Every materializer bounds its builder on this for the values and
/// closures it keeps, which is where a non-`Send` capture in a portable
/// core fails to compile.
pub trait Stores<T>: Builder {
    /// Erases `T` in one of the engine's shapes.
    #[doc(hidden)]
    fn erase(what: Erase<T>) -> Self::Erased
    where
        T: Sized + 'static;
}

impl<M: Mode> sealed::Sealed for BuildContext<M> {}

impl<M: Mode, T> Stores<T> for BuildContext<M>
where
    M: Accepts<T>,
{
    fn erase(what: Erase<T>) -> M::Carrier
    where
        T: Sized + 'static,
    {
        <M as Accepts<T>>::erase(what)
    }
}

impl<M: Mode> Builder for BuildContext<M> {
    type Erased = M::Carrier;
    type Inner<'p> = BuildContext<M>;

    fn erase_send<T: Send + 'static>(value: T) -> M::Carrier {
        M::erase_send(value)
    }

    fn check_token(&self, t: Token) -> u32 {
        BuildContext::check(self, t)
    }

    fn token_of(&self, n: u32) -> Token {
        BuildContext::token(self, n)
    }

    fn reach_of<S: Source>(&self, chain: &S) -> (u32, Vec<u32>) {
        BuildContext::chain_reach(self, chain)
    }

    fn make<N: AllOps>(
        &mut self,
        kind: Kind,
        data: Plane<M::Carrier>,
        parts: Box<[M::Carrier]>,
        deps: &[u32],
        flags: u8,
        reach: Vec<u32>,
    ) -> u32 {
        let data = data.into_data::<M>(|e| e);
        let n = BuildContext::materialize(self, kind, data, parts, M::ops_of::<N>(), deps, flags);
        BuildContext::set_reach(self, n, reach);
        n
    }

    fn make_pair<N: AllOps, O: AllOps>(
        &mut self,
        dependency: u32,
        reach: Vec<u32>,
        parts: Box<[M::Carrier]>,
        slot: M::Carrier,
    ) -> u32 {
        BuildContext::capture_pair(
            self,
            dependency,
            reach,
            parts,
            M::ops_of::<N>(),
            slot,
            M::ops_of::<O>(),
        )
    }

    fn make_switch_cell<C: CellRef>(&mut self, outer: Token) -> Token {
        BuildContext::switch_cell_node::<C>(self, outer)
    }

    fn make_switch_stream<S: Node>(&mut self, outer: Token, slot: M::Carrier) -> Token
    where
        S::Event: 'static,
    {
        BuildContext::switch_stream_node::<S>(self, outer, slot)
    }

    fn close_cell(&mut self, forward: Token, definition: Token) {
        BuildContext::close_cell_loop(self, forward, definition)
    }

    fn close_stream<N: AllOps>(
        &mut self,
        forward: Token,
        dependency: u32,
        reach: Vec<u32>,
        slot: M::Carrier,
        chain: M::Carrier,
    ) {
        BuildContext::close_stream_loop(
            self,
            forward,
            dependency,
            reach,
            slot,
            chain,
            M::ops_of::<N>(),
        )
    }

    fn make_construct<S, B, F>(
        &mut self,
        dependency: u32,
        reach: Vec<u32>,
        slot: M::Carrier,
        parts: Box<[M::Carrier]>,
    ) -> u32
    where
        S: Source,
        B: 'static,
        F: for<'p> FnMut(&mut Self::Inner<'p>, S::Event) -> B + 'static,
    {
        let ops = &<ConstructNode<S, F, B, Direct> as NodeOps<M>>::OPS;
        let data = Data::Slot(slot);
        let n = BuildContext::materialize(self, Kind::Stream, data, parts, ops, &[dependency], 0);
        BuildContext::set_reach(self, n, reach);
        n
    }

    fn read<A: 'static>(&self, t: Token) -> &A {
        let i = BuildContext::check(self, t);
        BuildContext::value::<A>(self, i)
    }

    fn open_forward(&mut self, stream: bool) -> Token {
        if stream {
            BuildContext::stream_loop_node(self)
        } else {
            BuildContext::loop_node(self)
        }
    }

    fn extend_reach(&mut self, n: u32, reach: Vec<u32>) {
        self.store.cold[n as usize].reach.extend(reach);
    }

    fn root(&mut self, nodes: Vec<u32>) -> Anchor {
        let flag = Liveness::new(&self.released);
        for i in nodes {
            self.anchors.push((i, flag.clone()));
        }
        Anchor::new(Some(flag))
    }
}

/// The builder a core is written against, so that it builds under both
/// modes: it borrows a `BuildContext<Local>` or a `BuildContext<Threaded>`,
/// and stores only what is `Send`.
///
/// A core takes `&mut Portable<'_>`, which has no type parameter, so a
/// closure it writes itself needs no bound to name it: the compiler checks
/// `Send` at each materializer, against the closure's real type.
///
/// ```
/// use bough::prelude::*;
///
/// fn plus_one(b: &mut Portable<'_>, numbers: Stream<u32>) -> Cell<u32> {
///     numbers.map(|n| n + 1).hold(b, 0)
/// }
///
/// let mut graph = Runtime::local();
/// let (numbers_in, latest) = graph.build_here(|b| {
///     let (numbers, numbers_in) = b.input::<u32>();
///     (numbers_in, plus_one(&mut b.portable(), numbers))
/// });
/// graph.send(numbers_in, 41);
/// assert_eq!(*graph.sample(latest), 42);
///
/// let mut graph = Runtime::threaded();
/// let (numbers_in, latest) = graph.build_here(|b| {
///     let (numbers, numbers_in) = b.input::<u32>();
///     (numbers_in, plus_one(&mut b.portable(), numbers))
/// });
/// graph.send(numbers_in, 1);
/// assert_eq!(*graph.sample(latest), 2);
/// ```
///
/// A portable core refuses what is not `Send`, at the materializer that
/// stores it, in a `Local` build too:
///
/// ```compile_fail,E0277
/// use std::rc::Rc;
///
/// use bough::prelude::*;
///
/// fn offset(b: &mut Portable<'_>, numbers: Stream<u32>) -> Cell<u32> {
///     let by = Rc::new(5u32);
///     numbers.map(move |n| n + *by).hold(b, 0) // error: Rc is not Send
/// }
///
/// let _ = Runtime::local().build_here(|b| {
///     let (numbers, _numbers_in) = b.input::<u32>();
///     offset(&mut b.portable(), numbers)
/// });
/// ```
///
/// A `construct` in a portable core hands its closure a `&mut Portable`,
/// and the closure is stored, so it must be `Send`:
///
/// ```compile_fail,E0277
/// use std::rc::Rc;
///
/// use bough::prelude::*;
///
/// fn spawner(b: &mut Portable<'_>, numbers: Stream<u32>) -> Stream<Cell<u32>> {
///     let by = Rc::new(5u32);
///     numbers.construct(b, move |b, n| b.constant(n + *by)) // error: Rc is not Send
/// }
///
/// let _ = Runtime::local().build_here(|b| {
///     let (numbers, _numbers_in) = b.input::<u32>();
///     spawner(&mut b.portable(), numbers)
/// });
/// ```
pub struct Portable<'a> {
    arm: Arm<'a>,
}

/// The build context a `Portable` borrows.
enum Arm<'a> {
    Local(&'a mut BuildContext<Local>),
    #[cfg(target_has_atomic = "ptr")]
    Threaded(&'a mut BuildContext<Threaded>),
}

/// Runs `$e` with `$b` bound to the borrowed build context, whichever mode
/// it is, and `$into` to the function that takes a `Portable`'s erased
/// value into that build context's carrier.
macro_rules! dispatch {
    ($self:expr, |$b:ident, $into:ident| $e:expr) => {
        match $self {
            Arm::Local($b) => {
                let $into = into_local;
                let _ = $into;
                $e
            }
            #[cfg(target_has_atomic = "ptr")]
            Arm::Threaded($b) => {
                let $into = into_threaded;
                let _ = $into;
                $e
            }
        }
    };
}

/// A `Send` box is a `Local` carrier once it forgets it is `Send`.
fn into_local(e: Box<dyn Any + Send>) -> Box<dyn Any> {
    e
}

#[cfg(target_has_atomic = "ptr")]
fn into_threaded(e: Box<dyn Any + Send>) -> Box<dyn Any + Send> {
    e
}

/// Takes parts erased for a `Portable` into a builder's carriers.
fn parts_into<C>(
    parts: Box<[Box<dyn Any + Send>]>,
    into: fn(Box<dyn Any + Send>) -> C,
) -> Box<[C]> {
    parts.into_vec().into_iter().map(into).collect()
}

impl BuildContext<Local> {
    /// A [`Portable`] that borrows this build context, for a core that runs
    /// in both modes. It stores only what is `Send`; the graph it builds is
    /// this one, so its tokens and this build context's mix freely.
    pub fn portable(&mut self) -> Portable<'_> {
        Portable {
            arm: Arm::Local(self),
        }
    }
}

#[cfg(target_has_atomic = "ptr")]
impl BuildContext<Threaded> {
    /// A [`Portable`] that borrows this build context, for a core that runs
    /// in both modes.
    pub fn portable(&mut self) -> Portable<'_> {
        Portable {
            arm: Arm::Threaded(self),
        }
    }
}

impl<'a> Portable<'a> {
    /// A view of a `Local` builder, for the engine's construct node.
    pub(crate) fn local(b: &'a mut BuildContext<Local>) -> Self {
        Portable { arm: Arm::Local(b) }
    }

    /// A view of a `Threaded` builder, for the engine's construct node.
    #[cfg(target_has_atomic = "ptr")]
    pub(crate) fn threaded(b: &'a mut BuildContext<Threaded>) -> Self {
        Portable {
            arm: Arm::Threaded(b),
        }
    }
}

impl sealed::Sealed for Portable<'_> {}

impl<T: Send> Stores<T> for Portable<'_> {
    fn erase(what: Erase<T>) -> Box<dyn Any + Send>
    where
        T: Sized + 'static,
    {
        crate::mode::erase_send_shape(what)
    }
}

impl<'a> Builder for Portable<'a> {
    type Erased = Box<dyn Any + Send>;
    type Inner<'p> = Portable<'p>;

    fn erase_send<T: Send + 'static>(value: T) -> Box<dyn Any + Send> {
        Box::new(value)
    }

    fn check_token(&self, t: Token) -> u32 {
        dispatch!(&self.arm, |b, _into| b.check(t))
    }

    fn token_of(&self, n: u32) -> Token {
        dispatch!(&self.arm, |b, _into| b.token(n))
    }

    fn reach_of<S: Source>(&self, chain: &S) -> (u32, Vec<u32>) {
        dispatch!(&self.arm, |b, _into| b.chain_reach(chain))
    }

    fn make<N: AllOps>(
        &mut self,
        kind: Kind,
        data: Plane<Box<dyn Any + Send>>,
        parts: Box<[Box<dyn Any + Send>]>,
        deps: &[u32],
        flags: u8,
        reach: Vec<u32>,
    ) -> u32 {
        dispatch!(&mut self.arm, |b, into| b.make::<N>(
            kind,
            data.map(into),
            parts_into(parts, into),
            deps,
            flags,
            reach
        ))
    }

    fn make_pair<N: AllOps, O: AllOps>(
        &mut self,
        dependency: u32,
        reach: Vec<u32>,
        parts: Box<[Box<dyn Any + Send>]>,
        slot: Box<dyn Any + Send>,
    ) -> u32 {
        dispatch!(&mut self.arm, |b, into| b.make_pair::<N, O>(
            dependency,
            reach,
            parts_into(parts, into),
            into(slot)
        ))
    }

    fn make_switch_cell<C: CellRef>(&mut self, outer: Token) -> Token {
        dispatch!(&mut self.arm, |b, _into| b.switch_cell_node::<C>(outer))
    }

    fn make_switch_stream<S: Node>(&mut self, outer: Token, slot: Box<dyn Any + Send>) -> Token
    where
        S::Event: 'static,
    {
        dispatch!(&mut self.arm, |b, into| b
            .switch_stream_node::<S>(outer, into(slot)))
    }

    fn close_cell(&mut self, forward: Token, definition: Token) {
        dispatch!(&mut self.arm, |b, _into| b
            .close_cell_loop(forward, definition))
    }

    fn close_stream<N: AllOps>(
        &mut self,
        forward: Token,
        dependency: u32,
        reach: Vec<u32>,
        slot: Box<dyn Any + Send>,
        chain: Box<dyn Any + Send>,
    ) {
        dispatch!(&mut self.arm, |b, into| b.close_stream::<N>(
            forward,
            dependency,
            reach,
            into(slot),
            into(chain)
        ))
    }

    fn make_construct<S, B, F>(
        &mut self,
        dependency: u32,
        reach: Vec<u32>,
        slot: Box<dyn Any + Send>,
        parts: Box<[Box<dyn Any + Send>]>,
    ) -> u32
    where
        S: Source,
        B: 'static,
        F: for<'p> FnMut(&mut Self::Inner<'p>, S::Event) -> B + 'static,
    {
        self.make::<ConstructNode<S, F, B, ThroughPortable>>(
            Kind::Stream,
            Plane::Slot(slot),
            parts,
            &[dependency],
            0,
            reach,
        )
    }

    fn read<A: 'static>(&self, t: Token) -> &A {
        dispatch!(&self.arm, |b, _into| b.read::<A>(t))
    }

    fn open_forward(&mut self, stream: bool) -> Token {
        dispatch!(&mut self.arm, |b, _into| b.open_forward(stream))
    }

    fn extend_reach(&mut self, n: u32, reach: Vec<u32>) {
        dispatch!(&mut self.arm, |b, _into| b.extend_reach(n, reach))
    }

    fn root(&mut self, nodes: Vec<u32>) -> Anchor {
        dispatch!(&mut self.arm, |b, _into| b.root(nodes))
    }
}
