//! Operations on cells (RFD 4).

use crate::Build;
use crate::mode::{Accepts, Mode};
use crate::source::Node;
use crate::token::{Cell, Stream};

impl<A: 'static> Cell<A> {
    /// The value the cell has at the start of the current transaction, or its
    /// current value between transactions.
    ///
    /// The context is borrowed shared, so several samples compose in one
    /// expression, and a caller that wants to keep a value clones it:
    ///
    /// ```no_run
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
        todo!()
    }

    /// The cell's steps as a stream, Sodium's `updates`. Fires on every step,
    /// including one to an equal value. Requires `Clone`, since the hold
    /// keeps the value too.
    pub fn steps<M>(self, build: &mut Build<M>) -> Stream<A>
    where
        M: Mode + Accepts<A>,
        A: Clone,
    {
        todo!()
    }

    /// Sodium's `value`: fires once at the instant it is created, with the
    /// value the cell has just after that instant, then on every step.
    pub fn steps_with_current<M>(self, build: &mut Build<M>) -> Stream<A>
    where
        M: Mode + Accepts<A>,
        A: Clone,
    {
        todo!()
    }

    /// A read-through cell: `f` of this cell's value, computed on read and
    /// memoized against this cell's version. `f` must be pure; the engine
    /// may call it any number of times per change.
    pub fn map_cell<M, B, F>(self, build: &mut Build<M>, f: F) -> Cell<B>
    where
        M: Mode + Accepts<F> + Accepts<B>,
        B: 'static,
        F: Fn(&A) -> B + 'static,
    {
        todo!()
    }

    /// A read-through cell of two cells. `lift` is binary and composes by
    /// chaining; Sodium's `apply` is `lift` with `|f, a| f(a)`.
    pub fn lift<M, B, C, F>(self, build: &mut Build<M>, other: Cell<B>, f: F) -> Cell<C>
    where
        M: Mode + Accepts<F> + Accepts<C>,
        B: 'static,
        C: 'static,
        F: Fn(&A, &B) -> C + 'static,
    {
        todo!()
    }
}

impl<A: 'static> Cell<Cell<A>> {
    /// Sodium's `switchC`: the cell that the outer cell currently selects,
    /// read through with no state of its own.
    pub fn switch_cell<M: Mode>(self, build: &mut Build<M>) -> Cell<A> {
        todo!()
    }
}

impl<S> Cell<S>
where
    S: Node + 'static,
    S::Item: 'static,
{
    /// Sodium's `switchS`: the occurrences of the stream the cell selected
    /// before the instant. A cell holding linear streams may have exactly
    /// one switch; a second is a build-time error.
    pub fn switch_stream<M: Mode>(self, build: &mut Build<M>) -> Stream<S::Item> {
        todo!()
    }
}
