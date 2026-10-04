//! A typed binding REPL over one running Bough graph.
//!
//! Its job is to be a probe: can `construct`, Bough's counterpart of
//! Sodium's `execute`, carry live rewiring of a running, strongly typed
//! graph, and is it ergonomic to do so? The REPL keeps the types on
//! purpose, so the friction shows. The findings are a note in the RFD
//! repository; this crate is the harness that produced them.
//!
//! The commands:
//!
//! | Command | Meaning |
//! |---|---|
//! | `input x 0` / `input b true` | Declare a typed input; the literal's syntax gives its type. |
//! | `def y add x 3` | Bind `y` to a registry function applied to bindings or literals. |
//! | `def y x` | Bind `y` to an existing binding, or to a literal. |
//! | `set x 5` | Send a value to an input. |
//! | `watch y` | Print `y = value` now and at every step. |
//! | `graph` | Print every binding with its type and definition. |

pub mod graph;
pub mod registry;
mod repl;
pub mod ty;

pub use repl::Repl;
