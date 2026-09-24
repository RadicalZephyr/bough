//! The build context (RFD 2).

use alloc::boxed::Box;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::engine::nodes::cell::HoldNode;
use crate::engine::nodes::stream::CoalescingInput;
use crate::engine::{COMMITS, Data, Kind, NodeOps, Ops, Sched, Store, Tx};
use crate::mode::{Accepts, Erase, Local, Mode};
use crate::slot::InputSlot;
use crate::source::Source;
use crate::token::{Cell, Input, Stream, Token, TokenRef};
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
}

impl<M: Mode> Build<M> {
    pub(crate) fn new() -> Self {
        Build {
            graph_id: next_graph_id(),
            store: Store::new(),
            tx: 0,
            in_tx: false,
            s: Sched::default(),
        }
    }

    /// Opens a scope: the build closure, or one run of a construct closure.
    pub(crate) fn push_scope(&mut self) {
        self.s.scopes.push(self.s.open_loops.len());
    }

    /// Closes a scope. A loop declared in it and still open is a build-time
    /// panic (stage 3 declares loops).
    pub(crate) fn pop_scope(&mut self) {
        let start = self.s.scopes.pop().expect("bough engine: a scope is open");
        assert!(
            self.s.open_loops.len() == start,
            "bough: a loop declared in this scope was never closed"
        );
    }

    /// The hold of an input cell. Its chain is the input's own stream token,
    /// which is `Send` whatever `A` is, so it is erased through
    /// `erase_send` and needs no `Accepts<Stream<A>>` bound.
    fn hold_input<A>(&mut self, stream: Stream<A>, initial: A) -> Cell<A>
    where
        A: 'static,
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
        let n = self.materialize(Kind::Input, data, Box::new([]), &Ops::<M>::DEFAULT, &[], 0);
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
        let n = self.materialize(
            Kind::Constant,
            data,
            Box::new([]),
            &Ops::<M>::DEFAULT,
            &[],
            0,
        );
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
