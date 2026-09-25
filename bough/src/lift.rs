// SPDX-License-Identifier: MPL-2.0

//! `lift` over a tuple of cells (RFD 4).

use crate::Build;
use crate::mode::{Accepts, Mode};
use crate::token::Cell;

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
pub trait Lift<F, R>: sealed::Sealed + Sized {
    /// A read-through cell of these cells: `f` over their values, computed on
    /// read and memoized until one of them steps. Two inputs stepping in one
    /// instant are one step of the result.
    fn lift<M>(self, build: &mut Build<M>, f: F) -> Cell<R>
    where
        M: Mode + Accepts<F> + Accepts<R>;
}

macro_rules! lift_tuple {
    ($($cell:ident),+) => {
        impl<$($cell: 'static,)+> sealed::Sealed for ($(Cell<$cell>,)+) {}

        impl<$($cell: 'static,)+ R: 'static, F> Lift<F, R> for ($(Cell<$cell>,)+)
        where
            F: Fn($(&$cell,)+) -> R + 'static,
        {
            fn lift<M>(self, build: &mut Build<M>, f: F) -> Cell<R>
            where
                M: Mode + Accepts<F> + Accepts<R>,
            {
                todo!()
            }
        }
    };
}

lift_tuple!(A, B);
lift_tuple!(A, B, C);
lift_tuple!(A, B, C, D);
lift_tuple!(A, B, C, D, E);
lift_tuple!(A, B, C, D, E, G);
