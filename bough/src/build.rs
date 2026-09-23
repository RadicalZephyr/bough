//! The build context (RFD 2).

use core::marker::PhantomData;

use crate::mode::{Accepts, Local, Mode};
use crate::slot::InputSlot;
use crate::source::Source;
use crate::token::{Cell, Input, Stream, Token, TokenRef};
use crate::trace::Trace;

/// The context every node-creating operation requires.
///
/// It exists inside [`Graph::build`](crate::Graph::build) and inside
/// [`Source::construct`] closures, and nowhere else. It has no `send` and no
/// `listen`; I/O lives on [`Graph`](crate::Graph).
pub struct Build<M: Mode = Local> {
    mode: PhantomData<M>,
}

impl<M: Mode> Build<M> {
    /// A stream driven from I/O code, and the token that drives it.
    pub fn input<A: 'static>(&mut self) -> (Stream<A>, Input<A>) {
        todo!()
    }

    /// An input that may be sent more than once in a transaction; `f`
    /// combines the values, first send on the left.
    pub fn input_coalescing<A, F>(&mut self, f: F) -> (Stream<A>, Input<A>)
    where
        A: 'static,
        F: Fn(A, A) -> A + 'static,
        M: Accepts<F>,
    {
        todo!()
    }

    /// A cell driven from I/O code: a hold over an input.
    pub fn input_cell<A>(&mut self, initial: A) -> (Cell<A>, Input<A>)
    where
        A: Trace + 'static,
        M: Accepts<A>,
    {
        todo!()
    }

    /// A cell driven from I/O code whose input coalesces.
    pub fn input_cell_coalescing<A, F>(&mut self, initial: A, f: F) -> (Cell<A>, Input<A>)
    where
        A: Trace + 'static,
        F: Fn(A, A) -> A + 'static,
        M: Accepts<A> + Accepts<F>,
    {
        todo!()
    }

    /// A cell that never changes.
    pub fn constant<A>(&mut self, value: A) -> Cell<A>
    where
        A: Trace + 'static,
        M: Accepts<A>,
    {
        todo!()
    }

    /// A stream that never fires.
    pub fn never<A: 'static>(&mut self) -> Stream<A> {
        todo!()
    }

    /// Declares a cell loop: a forward token usable anywhere, and the closer
    /// that later defines it.
    ///
    /// ```no_run
    /// use bough::{Graph, Source};
    ///
    /// let (graph, count) = Graph::build(|b| {
    ///     let (count, count_loop) = b.cell_loop::<u32>();      // declare
    ///     let (ticks, _ticks_in) = b.input::<()>();
    ///     let next = ticks.snapshot(count, |_, n| n + 1).hold(b, 0u32);
    ///     count_loop.close(b, next);                             // close
    ///     count
    /// });
    /// ```
    ///
    /// Every path from the definition back to the forward token must pass
    /// through a hold, an accumulator, a `split` or a `defer`; the check runs
    /// at close. A loop declared and never closed is a build-time panic at
    /// the end of the scope that declared it.
    pub fn cell_loop<A: 'static>(&mut self) -> (Cell<A>, CellLoop<A>) {
        todo!()
    }

    /// Declares a stream loop: a linear forward stream, and the closer that
    /// later defines it with any chain.
    pub fn stream_loop<A: 'static>(&mut self) -> (Stream<A>, StreamLoop<A>) {
        todo!()
    }

    /// Declares that `node` keeps every token in `on` alive, for a closure
    /// that captures tokens that are not upstream of its own node (RFD 3).
    /// One declaration per closure; the slice is heterogeneous.
    pub fn depends(&mut self, node: &impl TokenRef, on: &[&dyn TokenRef]) {
        todo!()
    }

    /// Connects an [`InputSlot`] to an input, so that
    /// [`pump`](crate::Graph::pump) drains it (RFD 7).
    ///
    /// Callable more than once for one input, one slot per producer; the
    /// driver drains slots in connection order, each as its own transaction.
    /// The slot's fold and the input's coalescing function are independent:
    /// the fold combines a burst between two pumps, the coalescing function
    /// combines two sends inside one transaction, which slots never cause.
    pub fn connect<A: Send + 'static>(&mut self, input: Input<A>, slot: &'static InputSlot<A>) {
        todo!()
    }
}

/// The closer of a cell loop.
///
/// Consumed by [`close`](CellLoop::close), so a loop cannot close twice. It
/// cannot be used from inside a `construct` closure either:
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
    /// Defines the loop: the forward token becomes `definition`.
    pub fn close<M: Mode>(self, build: &mut Build<M>, definition: Cell<A>) {
        todo!()
    }
}

/// The closer of a stream loop.
pub struct StreamLoop<A> {
    token: Token,
    event: PhantomData<fn() -> A>,
}

impl<A: 'static> StreamLoop<A> {
    /// Defines the loop: the forward stream becomes `definition`.
    pub fn close<M, S>(self, build: &mut Build<M>, definition: S)
    where
        M: Mode + Accepts<S>,
        S: Source<Event = A>,
    {
        todo!()
    }
}
