//! Node types: one zero-sized marker per type, whose `NodeOps::OPS` names
//! the monomorphized functions. Each function downcasts its own parts with
//! the types it was monomorphized for.

use core::marker::PhantomData;

pub(crate) mod cell;
pub(crate) mod stream;

/// The phantom every node marker carries: `Send`, `Sync` and `'static`
/// whatever its type parameters are.
type Marker<T> = PhantomData<fn() -> T>;
