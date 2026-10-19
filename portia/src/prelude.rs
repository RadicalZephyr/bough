//! The names a graph author uses, in one import:
//!
//! ```
//! use portia::prelude::*;
//!
//! let mut graph = Runtime::local();
//! let (numbers_in, doubled) = graph.build(|b| {
//!     let (numbers, numbers_in) = b.input::<u32>();
//!     (numbers_in, numbers.map(|n| n * 2).hold(b, 0))
//! });
//! graph.send(numbers_in, 21);
//! assert_eq!(*graph.sample(doubled), 42);
//! ```
//!
//! It holds the traits whose methods graph code calls: [`Source`] for
//! adapters and materializers, [`Builder`] for the builder's own
//! operations, and [`Lift`]; the traits a helper names in its bounds,
//! [`Stores`], [`Accepts`], [`Mode`], [`CellRef`], [`TokenRef`] and
//! [`Trace`]; and the types a core and its host name: the builders, the
//! runtime and its modes, the tokens, [`Anchored`] and [`Listener`].
//! Errors, I/O handles and adapter types stay at the crate root.
//!
//! A `Builder` of your own shadows the glob-imported name, but the build
//! builder's methods stay in scope:
//!
//! ```
//! use portia::prelude::*;
//!
//! struct Builder; // the app's own, as `gtk::Builder` would be
//!
//! let mut graph = Runtime::local();
//! let (numbers_in, total) = graph.build(|b| {
//!     let (numbers, numbers_in) = b.input::<u32>();
//!     let offset = b.constant(1u32);
//!     let total = numbers.snapshot(offset, |n, o| n + o).hold(b, 0);
//!     (numbers_in, total)
//! });
//! graph.send(numbers_in, 41);
//! assert_eq!(*graph.sample(total), 42);
//! let _ = Builder;
//! ```

#[cfg(target_has_atomic = "ptr")]
pub use crate::Threaded;

pub use crate::{
    Accepts, Anchored, BuildContext, Builder, Cell, CellRef, Input, Lift, Listener, Local, Mode,
    Portable, Runtime, Shared, Source, State, Stores, Stream, TokenRef, Trace,
};

// Exported twice: by name, so a bound can say `Builder`, and as `_`, so its
// methods stay in scope when a type of the user's own called `Builder`, such
// as `gtk::Builder`, shadows the name the glob brings in.
pub use crate::Builder as _;
