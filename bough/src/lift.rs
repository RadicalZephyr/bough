//! `lift` over a tuple of cells (RFD 4).

use crate::Build;
use crate::cell::{CellKind, CellRef, read_through};
use crate::mode::{Accepts, Mode};

mod sealed {
    pub trait Sealed {}
}

/// A tuple of two to six cells, which `lift` combines into one read-through
/// cell with a function over references to all of their values:
/// `(price, quantity).lift(b, |p, q| p * q)`.
///
/// There is no binary method on `Cell`: chaining binary lifts composes
/// functions rather than lifting three cells, and an intermediate cell would
/// clone two inputs on every read. Sodium's `apply` is
/// `(cf, ca).lift(b, |f, a| f(a))` with the cell holding `Fn(&A) -> B`.
///
/// ```
/// use bough::{Cell, Runtime, Lift, Source};
///
/// let (mut graph, edge) = Runtime::build(|b| {
///     let (price, price_in) = b.input_cell(3u32);
///     let (quantity, quantity_in) = b.input_cell(2u32);
///     let total: Cell<u32> = (price, quantity).lift(b, |p, q| p * q);
///     (price_in, quantity_in, total)
/// });
/// let (price_in, quantity_in, total) = edge.keep();
/// assert_eq!(*graph.sample(total), 6);
/// graph.transaction(|tx| {
///     tx.send(price_in, 4); // two inputs step in one instant:
///     tx.send(quantity_in, 5); // one step of the lifted cell
/// });
/// assert_eq!(*graph.sample(total), 20);
/// ```
///
/// Each element is a [`Cell`](crate::Cell) or a [`State`](crate::State).
/// The lifted cell is a `Cell` when every input is one, and a `State` when
/// any input is a `State`: its value after an instant in which that state
/// stepped does not exist until commit, so it has no stream view.
///
/// ```compile_fail,E0599
/// use bough::{Runtime, Lift, Source};
///
/// let (_graph, edge) = Runtime::build(|b| {
///     let (names, _names_in) = b.input::<String>();
///     let members = names.accumulate_mut(b, Vec::new(), |name, m: &mut Vec<String>| m.push(name));
///     let (extra, _extra_in) = b.input_cell(1usize);
///     let total = (members, extra).lift(b, |m, e| m.len() + e);
///     let _totals = total.steps(b); // error: no method named `steps` found for struct `State`
/// });
/// edge.keep();
/// ```
pub trait Lift<F, R>: sealed::Sealed + Sized {
    /// The lifted cell: `Cell<R>` when every input is a `Cell`, `State<R>`
    /// when any input is a `State`.
    type Output: CellRef<Value = R>;

    /// A read-through cell of these cells: `f` over their values, computed on
    /// read and memoized until one of them steps. Two inputs stepping in one
    /// instant are one step of the result.
    fn lift<M>(self, build: &mut Build<M>, f: F) -> Self::Output
    where
        M: Mode + Accepts<F> + Accepts<R>;
}

/// The kind of a lifted cell: the join of its inputs' kinds, a `State` if
/// any input is one.
macro_rules! join {
    ($cell:ident) => {
        <$cell as CellRef>::Kind
    };
    ($cell:ident, $($rest:ident),+) => {
        <<$cell as CellRef>::Kind as CellKind>::Join<join!($($rest),+)>
    };
}

macro_rules! lift_tuple {
    ($($cell:ident $i:tt),+) => {
        impl<$($cell: CellRef,)+> sealed::Sealed for ($($cell,)+) {}

        impl<$($cell: CellRef,)+ R: 'static, F> Lift<F, R> for ($($cell,)+)
        where
            F: Fn($(&$cell::Value,)+) -> R + 'static,
        {
            type Output = <join!($($cell),+) as CellKind>::Ref<R>;

            fn lift<M>(self, build: &mut Build<M>, f: F) -> Self::Output
            where
                M: Mode + Accepts<F> + Accepts<R>,
            {
                let inputs = [$(build.check(self.$i.token()),)+];
                let token = read_through::<M, ($($cell::Value,)+), R, F>(build, &inputs, f);
                <join!($($cell),+) as CellKind>::wrap::<R>(token)
            }
        }
    };
}

lift_tuple!(A 0, B 1);
lift_tuple!(A 0, B 1, C 2);
lift_tuple!(A 0, B 1, C 2, D 3);
lift_tuple!(A 0, B 1, C 2, D 3, E 4);
lift_tuple!(A 0, B 1, C 2, D 3, E 4, G 5);
