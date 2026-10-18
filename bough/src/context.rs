//! Build contexts: the one trait every materializer takes, and
//! [`Portable`], the context a mode-agnostic core is written against.
//!
//! A materializer takes `&mut C` for any [`Context`] `C`, and asks
//! [`Stores<T>`] of `C` for each value and closure it keeps. A
//! [`Build<M>`](crate::Build) stores what its mode [`Accepts`]; a
//! [`Portable`] stores what is `Send`, whichever builder it borrows. So
//! `stream.hold(b, x)` reads the same with all three, and a portable core
//! can store a closure it writes itself, since the bound names `Portable`,
//! not the closure.
//!
//! The hidden methods are the engine's primitives, each typed by the node
//! it makes rather than by the mode: a `Build<M>` takes its mode's
//! functions, and a `Portable` matches on the builder it borrows and takes
//! that one's. A portable chain is monomorphized once per mode it may run
//! in, because the node functions stay per mode.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::any::Any;

use crate::build::{Build, CellLoop, StateLoop, StreamLoop};
use crate::cell::CellRef;
use crate::engine::nodes::construct::{ConstructNode, Direct, ThroughPortable};
use crate::engine::{Data, Kind, NodeOps};
#[cfg(target_has_atomic = "ptr")]
use crate::mode::Threaded;
use crate::mode::{Accepts, Erase, Local, Mode};
use crate::runtime::Anchored;
use crate::source::{Node, Source};
use crate::token::Token;
use crate::token::{Cell, Input, State, Stream, TokenRef};
use crate::trace::Trace;

pub(crate) mod sealed {
    pub trait Sealed {}
}

/// A node type with functions for every mode the target has, so a context
/// that is not one mode can take whichever its builder needs.
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
/// context's erased type.
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

/// What every node-creating operation takes: a [`Build`] of either mode,
/// or a [`Portable`] view of one. Sealed; its items are hidden.
pub trait Context: sealed::Sealed {
    /// What [`Stores::erase`] makes: the mode's carrier for a `Build`, a
    /// `Send` box for a `Portable`.
    #[doc(hidden)]
    type Erased: 'static;

    /// What a `construct` closure built in this context receives.
    #[doc(hidden)]
    type Inner<'p>: Context;

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
}

/// Whether a context may store a `T`: a `Build<M>` when `M:`
/// [`Accepts<T>`], a [`Portable`] when `T: Send`.
///
/// Every materializer bounds its context on this for the values and
/// closures it keeps, which is where a non-`Send` capture in a portable
/// core fails to compile.
pub trait Stores<T>: Context {
    /// Erases `T` in one of the engine's shapes.
    #[doc(hidden)]
    fn erase(what: Erase<T>) -> Self::Erased
    where
        T: Sized + 'static;
}

impl<M: Mode> sealed::Sealed for Build<M> {}

impl<M: Mode, T> Stores<T> for Build<M>
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

impl<M: Mode> Context for Build<M> {
    type Erased = M::Carrier;
    type Inner<'p> = Build<M>;

    fn erase_send<T: Send + 'static>(value: T) -> M::Carrier {
        M::erase_send(value)
    }

    fn check_token(&self, t: Token) -> u32 {
        Build::check(self, t)
    }

    fn token_of(&self, n: u32) -> Token {
        Build::token(self, n)
    }

    fn reach_of<S: Source>(&self, chain: &S) -> (u32, Vec<u32>) {
        Build::chain_reach(self, chain)
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
        let n = Build::materialize(self, kind, data, parts, M::ops_of::<N>(), deps, flags);
        Build::set_reach(self, n, reach);
        n
    }

    fn make_pair<N: AllOps, O: AllOps>(
        &mut self,
        dependency: u32,
        reach: Vec<u32>,
        parts: Box<[M::Carrier]>,
        slot: M::Carrier,
    ) -> u32 {
        Build::capture_pair(
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
        Build::switch_cell_node::<C>(self, outer)
    }

    fn make_switch_stream<S: Node>(&mut self, outer: Token, slot: M::Carrier) -> Token
    where
        S::Event: 'static,
    {
        Build::switch_stream_node::<S>(self, outer, slot)
    }

    fn close_cell(&mut self, forward: Token, definition: Token) {
        Build::close_cell_loop(self, forward, definition)
    }

    fn close_stream<N: AllOps>(
        &mut self,
        forward: Token,
        dependency: u32,
        reach: Vec<u32>,
        slot: M::Carrier,
        chain: M::Carrier,
    ) {
        Build::close_stream_loop(
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
        let n = Build::materialize(self, Kind::Stream, data, parts, ops, &[dependency], 0);
        Build::set_reach(self, n, reach);
        n
    }

    fn read<A: 'static>(&self, t: Token) -> &A {
        let i = Build::check(self, t);
        Build::value::<A>(self, i)
    }
}

/// The builder a mode-agnostic core is written against: a view of a
/// `Build<Local>` or a `Build<Threaded>` that stores only what is `Send`.
///
/// A core takes `&mut Portable<'_>`, which has no type parameter, so a
/// closure it writes itself needs no bound to name it: the compiler checks
/// `Send` at each materializer, against the closure's real type.
///
/// ```
/// use bough::{Cell, Portable, Runtime, Source, Stream};
///
/// fn plus_one(b: &mut Portable<'_>, numbers: Stream<u32>) -> Cell<u32> {
///     numbers.map(|n| n + 1).hold(b, 0)
/// }
///
/// let (mut graph, edge) = Runtime::build(|b| {
///     let (numbers, numbers_in) = b.input::<u32>();
///     (numbers_in, plus_one(&mut b.portable(), numbers))
/// });
/// let (numbers_in, latest) = edge.keep();
/// graph.send(numbers_in, 41);
/// assert_eq!(*graph.sample(latest), 42);
///
/// let (mut graph, edge) = Runtime::build_threaded(|b| {
///     let (numbers, numbers_in) = b.input::<u32>();
///     (numbers_in, plus_one(&mut b.portable(), numbers))
/// });
/// let (numbers_in, latest) = edge.keep();
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
/// use bough::{Cell, Portable, Runtime, Source, Stream};
///
/// fn offset(b: &mut Portable<'_>, numbers: Stream<u32>) -> Cell<u32> {
///     let by = Rc::new(5u32);
///     numbers.map(move |n| n + *by).hold(b, 0) // error: Rc is not Send
/// }
///
/// let (_graph, edge) = Runtime::build(|b| {
///     let (numbers, _numbers_in) = b.input::<u32>();
///     offset(&mut b.portable(), numbers)
/// });
/// edge.keep();
/// ```
pub struct Portable<'a> {
    arm: Arm<'a>,
}

/// The builder a `Portable` borrows.
enum Arm<'a> {
    Local(&'a mut Build<Local>),
    #[cfg(target_has_atomic = "ptr")]
    Threaded(&'a mut Build<Threaded>),
}

/// Runs `$e` with `$b` bound to the borrowed builder, whichever mode it is,
/// and `$into` to the function that takes a `Portable`'s erased value into
/// that builder's carrier.
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

impl Build<Local> {
    /// A [`Portable`] view of this builder, for a core that runs in both
    /// modes. It stores only what is `Send`; the graph it builds is this
    /// one, so its tokens and this builder's mix freely.
    pub fn portable(&mut self) -> Portable<'_> {
        Portable {
            arm: Arm::Local(self),
        }
    }
}

#[cfg(target_has_atomic = "ptr")]
impl Build<Threaded> {
    /// A [`Portable`] view of this builder, for a core that runs in both
    /// modes.
    pub fn portable(&mut self) -> Portable<'_> {
        Portable {
            arm: Arm::Threaded(self),
        }
    }
}

impl<'a> Portable<'a> {
    /// A view of a `Local` builder, for the engine's construct node.
    pub(crate) fn local(b: &'a mut Build<Local>) -> Self {
        Portable { arm: Arm::Local(b) }
    }

    /// A view of a `Threaded` builder, for the engine's construct node.
    #[cfg(target_has_atomic = "ptr")]
    pub(crate) fn threaded(b: &'a mut Build<Threaded>) -> Self {
        Portable {
            arm: Arm::Threaded(b),
        }
    }
}

/// The builder's own operations, for what is `Send`: each matches on the
/// builder and calls its method of the same name.
impl Portable<'_> {
    /// [`Build::input`], for a `Send` event.
    pub fn input<A: Send + 'static>(&mut self) -> (Stream<A>, Input<A>) {
        dispatch!(&mut self.arm, |b, _into| b.input::<A>())
    }

    /// [`Build::input_cell`], for a `Send` value.
    pub fn input_cell<A: Trace + Send + 'static>(&mut self, initial: A) -> (Cell<A>, Input<A>) {
        dispatch!(&mut self.arm, |b, _into| b.input_cell(initial))
    }

    /// [`Build::constant`], for a `Send` value.
    pub fn constant<A: Trace + Send + 'static>(&mut self, value: A) -> Cell<A> {
        dispatch!(&mut self.arm, |b, _into| b.constant(value))
    }

    /// [`Build::never`].
    pub fn never<A: 'static>(&mut self) -> Stream<A> {
        dispatch!(&mut self.arm, |b, _into| b.never::<A>())
    }

    /// [`Build::cell_loop`].
    pub fn cell_loop<A: 'static>(&mut self) -> (Cell<A>, CellLoop<A>) {
        dispatch!(&mut self.arm, |b, _into| b.cell_loop::<A>())
    }

    /// [`Build::state_loop`].
    pub fn state_loop<A: 'static>(&mut self) -> (State<A>, StateLoop<A>) {
        dispatch!(&mut self.arm, |b, _into| b.state_loop::<A>())
    }

    /// [`Build::stream_loop`].
    pub fn stream_loop<A: 'static>(&mut self) -> (Stream<A>, StreamLoop<A>) {
        dispatch!(&mut self.arm, |b, _into| b.stream_loop::<A>())
    }

    /// [`Build::depends`].
    pub fn depends(&mut self, node: &impl TokenRef, on: &[&dyn Trace]) {
        dispatch!(&mut self.arm, |b, _into| b.depends(node, on))
    }

    /// [`Build::anchor`].
    pub fn anchor<T: Trace>(&mut self, value: T) -> Anchored<T> {
        dispatch!(&mut self.arm, |b, _into| b.anchor(value))
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

impl<'a> Context for Portable<'a> {
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
}
