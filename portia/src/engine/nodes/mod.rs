//! Node types: one zero-sized marker per type, whose `NodeOps::OPS` names
//! the monomorphized functions. Each function downcasts its own parts with
//! the types it was monomorphized for.

use core::marker::PhantomData;

use crate::engine::{NodeOps, Ops};
use crate::mode::Mode;

pub(crate) mod cell;
pub(crate) mod construct;
pub(crate) mod read;
pub(crate) mod split;
pub(crate) mod stream;
pub(crate) mod switch;

/// The phantom every node marker carries: `Send`, `Sync` and `'static`
/// whatever its type parameters are.
type Marker<T> = PhantomData<fn() -> T>;

/// A node with no functions: `never`.
pub(crate) struct DefaultNode;

impl<M: Mode> NodeOps<M> for DefaultNode {
    const OPS: Ops<M> = Ops::<M>::DEFAULT;
}
