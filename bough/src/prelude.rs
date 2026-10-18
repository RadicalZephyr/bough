//! The names a graph author uses, in one import:
//!
//! ```
//! use bough::prelude::*;
//!
//! let (mut graph, edge) = Runtime::build(|b| {
//!     let (numbers, numbers_in) = b.input::<u32>();
//!     (numbers_in, numbers.map(|n| n * 2).hold(b, 0))
//! });
//! let (numbers_in, doubled) = edge.keep();
//! graph.send(numbers_in, 21);
//! assert_eq!(*graph.sample(doubled), 42);
//! ```
//!
//! It holds the traits whose methods graph code calls: [`Source`] for
//! adapters and materializers, [`Context`] for the builder's own
//! operations, and [`Lift`]; the traits a helper names in its bounds,
//! [`Stores`], [`Accepts`], [`Mode`], [`CellRef`], [`TokenRef`] and
//! [`Trace`]; and the types a core and its host name: the builders, the
//! runtime and its modes, the tokens, [`Anchored`] and [`Listener`].
//! Errors, I/O handles and adapter types stay at the crate root.

#[cfg(target_has_atomic = "ptr")]
pub use crate::Threaded;

// The builder's methods stay in scope where an explicit import of another
// `Context`, such as `std::task::Context`, shadows the name above.
pub use crate::Context as _;
pub use crate::{
    Accepts, Anchored, Build, Cell, CellRef, Context, Input, Lift, Listener, Local, Mode, Portable,
    Runtime, Shared, Source, State, Stores, Stream, TokenRef, Trace,
};
