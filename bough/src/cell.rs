//! Operations on cells (RFD 4).

use alloc::boxed::Box;

use crate::Build;
use crate::engine::nodes::read::{ReadFn, ReadNode};
use crate::engine::nodes::stream::StepsNode;
use crate::engine::{Data, Kind, NodeOps};
use crate::mode::{Accepts, Erase, Mode};
use crate::source::Node;
use crate::token::{Cell, State, Stream, Token, TokenRef};

/// Anything that names a cell: a [`Cell`], or a [`State`].
///
/// Every operation that reads a cell's value takes either: `snapshot`,
/// `gate`, `lift`, and on [`Graph`](crate::Graph) `sample`, `listen_cell`
/// and `listen_steps` and their `try_` forms; `sample` and `map_cell` exist
/// on both, and `map_cell` over a `State` is a `State`. Each reads the
/// value from before the instant, or the committed value after it, and
/// both kinds of cell have those. The stream views `steps` and
/// `steps_with_current` exist on `Cell` alone: they carry the value after
/// the instant during the instant, which a `State` does not have until
/// commit.
///
/// Sealed: `Cell` and `State` are its only implementors, as the five token
/// types are the only [`TokenRef`]s.
///
/// ```
/// use bough::{Graph, Source};
///
/// let (mut graph, (joins_in, members)) = Graph::build(|b| {
///     let (joins, joins_in) = b.input::<String>();
///     let (lines, _lines_in) = b.input::<String>();
///     let joins = joins.share(b);
///     let members = joins.accumulate_mut(b, Vec::new(), |name, m: &mut Vec<String>| m.push(name));
///     let open = joins.accumulate_mut(b, false, |_, open: &mut bool| *open = true);
///     let _said = lines
///         .gate(open)
///         .snapshot(members, |line, m| format!("{line} to {}", m.len()))
///         .node(b);
///     assert!(members.sample(b).is_empty());
///     (joins_in, members)
/// });
/// let _sizes = graph.listen_cell(members, |m| println!("{} members", m.len()));
/// graph.send(joins_in, "ada".to_string());
/// assert_eq!(graph.sample(members).len(), 1);
/// ```
pub trait CellRef: TokenRef + Copy + 'static {
    /// The value the cell holds.
    type Value: 'static;

    /// `Cell` or `State` as a type, which says whether a stream view of
    /// the cell can exist. `lift` joins its inputs' kinds.
    #[doc(hidden)]
    type Kind: CellKind;
}

impl<A: 'static> CellRef for Cell<A> {
    type Value = A;
    type Kind = Steps;
}

impl<A: 'static> CellRef for State<A> {
    type Value = A;
    type Kind = NoSteps;
}

/// The two kinds of cell token as types, and how a read-through cell over
/// several cells combines them. Hidden: [`CellRef`] names it, and
/// `Lift::Output` is computed from it.
#[doc(hidden)]
pub trait CellKind: 'static {
    /// The token of a cell of this kind holding `A`.
    type Ref<A: 'static>: CellRef<Value = A>;

    /// The kind of a read-through cell over a cell of this kind and cells
    /// of kind `K`: a `State` if any input is one.
    type Join<K: CellKind>: CellKind;

    /// The token of this kind naming a node.
    fn wrap<A: 'static>(token: Token) -> Self::Ref<A>;
}

/// The kind of a [`Cell`]: its value after the instant exists during the
/// instant, so it has stream views.
#[doc(hidden)]
pub struct Steps;

/// The kind of a [`State`]: its new value exists from commit, so it has no
/// stream view.
#[doc(hidden)]
pub struct NoSteps;

impl CellKind for Steps {
    type Ref<A: 'static> = Cell<A>;
    type Join<K: CellKind> = K;
    fn wrap<A: 'static>(token: Token) -> Cell<A> {
        Cell::from_token(token)
    }
}

impl CellKind for NoSteps {
    type Ref<A: 'static> = State<A>;
    type Join<K: CellKind> = NoSteps;
    fn wrap<A: 'static>(token: Token) -> State<A> {
        State::from_token(token)
    }
}

/// A read-through cell over the cells at `inputs`: `f` of their values,
/// computed on read. Its function and its memo live in the node's data; it
/// has no program.
pub(crate) fn read_through<M, V, R, F>(build: &mut Build<M>, inputs: &[u32], f: F) -> Token
where
    M: Mode + Accepts<F> + Accepts<R>,
    V: 'static,
    R: 'static,
    F: ReadFn<V, R>,
{
    let data = Data::ReadThrough {
        f: <M as Accepts<F>>::erase(Erase::Value(f)),
        memo: <M as Accepts<R>>::erase(Erase::Memo),
    };
    let ops = &<ReadNode<V, R, F> as NodeOps<M>>::OPS;
    let n = build.materialize(Kind::ReadThrough, data, Box::new([]), ops, inputs, 0);
    build.token(n)
}

impl<A: 'static> State<A> {
    /// The state at the start of the current transaction, or its current
    /// state between transactions. An in-place accumulator's function runs
    /// at commit, so every read during a transaction sees the state from
    /// before it, as [`Cell::sample`] does.
    pub fn sample<M: Mode>(self, build: &Build<M>) -> &A {
        let i = build.check(self.token);
        build.value::<A>(i)
    }

    /// A read-through cell over the state, as [`Cell::map_cell`]. It is a
    /// `State` too: its value after an instant in which the state stepped
    /// does not exist until commit either, so it has no stream view.
    ///
    /// ```compile_fail,E0599
    /// use bough::{Graph, Source};
    ///
    /// let (_graph, _) = Graph::build(|b| {
    ///     let (names, _names_in) = b.input::<String>();
    ///     let members = names.accumulate_mut(b, Vec::new(), |name, m: &mut Vec<String>| m.push(name));
    ///     let count = members.map_cell(b, |m| m.len());
    ///     let _counts = count.steps(b); // error: no method named `steps` found for struct `State`
    /// });
    /// ```
    pub fn map_cell<M, B, F>(self, build: &mut Build<M>, f: F) -> State<B>
    where
        M: Mode + Accepts<F> + Accepts<B>,
        B: 'static,
        F: Fn(&A) -> B + 'static,
    {
        let input = build.check(self.token);
        State::from_token(read_through::<M, (A,), B, F>(build, &[input], f))
    }
}

impl<A: 'static> Cell<A> {
    /// The value the cell has at the start of the current transaction, or its
    /// current value between transactions.
    ///
    /// The context is borrowed shared, so several samples compose in one
    /// expression, and a caller that wants to keep a value clones it:
    ///
    /// ```
    /// use bough::{Graph, Source};
    ///
    /// let (graph, _) = Graph::build(|b| {
    ///     let (a, _a_in) = b.input_cell(1u32);
    ///     let (c, _c_in) = b.input_cell(2u32);
    ///     let text = format!("{} {}", a.sample(b), c.sample(b));
    ///     let x = a.sample(b);
    ///     let y = c.sample(b);
    ///     let sum = *x + *y;
    ///     (text, sum)
    /// });
    /// ```
    pub fn sample<M: Mode>(self, build: &Build<M>) -> &A {
        let i = build.check(self.token);
        build.value::<A>(i)
    }

    /// A read-through cell: `f` of this cell's value, computed on read and
    /// memoized until this cell steps. `f` must be pure; the engine calls it
    /// at most once per value of its input, and not at all if the cell is
    /// never read.
    ///
    /// The new cell steps whenever this one does, so its listeners fire on
    /// every step, and a read during a transaction sees the value from
    /// before the instant, as every cell read does. A [`steps`](Cell::steps)
    /// view computes the value after the instant during the instant, and
    /// that value becomes the memo at commit, so the view and the listeners
    /// share one call per step.
    pub fn map_cell<M, B, F>(self, build: &mut Build<M>, f: F) -> Cell<B>
    where
        M: Mode + Accepts<F> + Accepts<B>,
        B: 'static,
        F: Fn(&A) -> B + 'static,
    {
        let input = build.check(self.token);
        Cell::from_token(read_through::<M, (A,), B, F>(build, &[input], f))
    }
}

impl<A: Clone + 'static> Cell<A> {
    /// Sodium's `updates`: a stream of this cell's steps, each carrying the
    /// post-instant value, including a step to an equal value.
    ///
    /// The Sodium book, section 8.4: "To protect the idea of a continuously
    /// varying cell, a true FRP system must ensure that changes in a cell's
    /// value aren't observable." This stream observes them, and so depends on
    /// how the cell was built rather than only on what it holds. Use it where
    /// an operational situation needs it, such as sending a cell over a wire;
    /// from I/O code, [`Graph::listen_steps`](crate::Graph::listen_steps) is
    /// the same view. It does not exist on a [`State`], whose new value does
    /// not exist until commit.
    ///
    /// The node clones the value after the instant into its slot. For a
    /// read-through cell that value is computed from its inputs' values
    /// after the instant and kept, and at commit it becomes the cell's
    /// memo, so the cell's function runs once per step, however many steps
    /// views and listeners the cell has.
    pub fn steps<M>(self, build: &mut Build<M>) -> Stream<A>
    where
        M: Mode + Accepts<A>,
    {
        Stream::from_token(steps_node::<M, A, false>(self, build))
    }

    /// Sodium's `value`: fires once at its creation instant with the
    /// post-instant value, then on every step like [`steps`](Cell::steps),
    /// with the same warning. From I/O code,
    /// [`Graph::listen_cell`](crate::Graph::listen_cell) is the same view.
    ///
    /// Built in the build closure, it fires in transaction zero, so a hold
    /// built there over it starts the graph at the cell's value. A creation
    /// and a step in one instant are one event, carrying the value after
    /// the step.
    pub fn steps_with_current<M>(self, build: &mut Build<M>) -> Stream<A>
    where
        M: Mode + Accepts<A>,
    {
        Stream::from_token(steps_node::<M, A, true>(self, build))
    }
}

/// The node of `steps` and `steps_with_current`: a stream node whose one
/// dependency is the cell, and which has no program of its own.
fn steps_node<M, A, const CURRENT: bool>(cell: Cell<A>, build: &mut Build<M>) -> Token
where
    M: Mode + Accepts<A>,
    A: Clone + 'static,
{
    let input = build.check(cell.token);
    let data = Data::Slot(<M as Accepts<A>>::erase(Erase::Slot));
    let ops = &<StepsNode<A, CURRENT> as NodeOps<M>>::OPS;
    let n = build.materialize(Kind::Stream, data, Box::new([]), ops, &[input], 0);
    build.token(n)
}

impl<A: 'static> Cell<Cell<A>> {
    /// Sodium's `switchC`: the cell that the outer cell currently selects,
    /// read through on read. Its one piece of state is the inner it depends
    /// on, relinked at commit whenever the outer steps; it steps at creation
    /// and at every switch instant, even when the new inner is quiet.
    pub fn switch_cell<M: Mode>(self, build: &mut Build<M>) -> Cell<A> {
        todo!()
    }
}

impl<S> Cell<S>
where
    S: Node + 'static,
    S::Event: 'static,
{
    /// Sodium's `switchS`: the events of the stream the cell selected
    /// before the instant. A cell holding linear streams may have exactly
    /// one switch; a second is a build-time error.
    pub fn switch_stream<M: Mode>(self, build: &mut Build<M>) -> Stream<S::Event> {
        todo!()
    }
}
