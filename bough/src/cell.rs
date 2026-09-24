//! Operations on cells (RFD 4).

use crate::Build;
use crate::mode::{Accepts, Mode};
use crate::source::Node;
use crate::token::{Cell, State, Stream, TokenRef};

/// Anything that names a cell: a [`Cell`], or a [`State`].
///
/// Every operation that reads a cell's value takes either: `sample`,
/// `snapshot`, `gate`, `lift`, and on [`Graph`](crate::Graph) `sample`,
/// `listen_cell` and `listen_steps` and their `try_` forms. Each reads the
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
}

impl<A: 'static> CellRef for Cell<A> {
    type Value = A;
}

impl<A: 'static> CellRef for State<A> {
    type Value = A;
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
    /// memoized against this cell's version. `f` must be pure; the engine
    /// calls it at most once per version of its input, and not at all if the
    /// cell is never read.
    pub fn map_cell<M, B, F>(self, build: &mut Build<M>, f: F) -> Cell<B>
    where
        M: Mode + Accepts<F> + Accepts<B>,
        B: 'static,
        F: Fn(&A) -> B + 'static,
    {
        todo!()
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
    pub fn steps<M>(self, build: &mut Build<M>) -> Stream<A>
    where
        M: Mode + Accepts<A>,
    {
        todo!()
    }

    /// Sodium's `value`: fires once at its creation instant with the
    /// post-instant value, then on every step like [`steps`](Cell::steps),
    /// with the same warning. From I/O code,
    /// [`Graph::listen_cell`](crate::Graph::listen_cell) is the same view.
    pub fn steps_with_current<M>(self, build: &mut Build<M>) -> Stream<A>
    where
        M: Mode + Accepts<A>,
    {
        todo!()
    }
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
