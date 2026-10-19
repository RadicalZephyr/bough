//! A typed binding REPL over one running Bough graph.
//!
//! Its job is to be a probe: can `construct`, Bough's counterpart of
//! Sodium's `execute`, carry live rewiring of a running, strongly typed
//! graph, and is it ergonomic to do so? The REPL keeps the types on
//! purpose, so the friction shows. The findings are a note in the RFD
//! repository; this crate is the harness that produced them.
//!
//! The commands, one parenthesized form per line, `;` starting a comment:
//!
//! | Command | Meaning |
//! |---|---|
//! | `(input x 0)` / `(input b true)` | Declare a typed input; the literal's syntax gives its type. |
//! | `(def y (add x (mul x 2)))` | Bind `y` to an expression of bindings, literals and functions, nested as deep as you like. |
//! | `(def y x)` | Bind `y` to an existing binding, or to a literal. |
//! | `(def y (mul x x))` | Redefine `y`, of the same type and with no cycle; everything downstream follows. |
//! | `(defgraph g [a n] (add (mul a n) 1))` | Name a body of graph building: `(def y (g x 2))` builds it afresh, with `x` and `2` for `a` and `n`. |
//! | `(set x 5)` | Send a value to an input. |
//! | `(watch y)` | Print `y = value` now and at every step. |
//! | `(unwatch y)` | Stop printing `y`. |
//! | `(graph)` | Print every binding with its type and definition. |
//! | `(tick t 1000)` | An `Int` input a timer thread sets to 1, 2, 3, ..., one every 1000 ms. |
//! | `(help)` | List the help topics: `(help commands)`, `(help fns)`, `(help all)` and `(help name)`. |

pub mod clock;
pub mod graph;
pub mod read;
pub mod registry;
mod repl;
pub mod ty;

pub use repl::Repl;
