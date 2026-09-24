//! Lightweight FRP rooted in Rust.
//!
//! Bough implements the Sodium FRP denotational semantics with an API that
//! cleanly separates building FRP logic from driving it with I/O. Every
//! node-creating operation needs a [`Build`] context, which only exists
//! inside [`Graph::build`] and inside [`Source::construct`] closures; sending,
//! listening and sampling from outside live on [`Graph`], which only exists
//! once the build closure has returned. The design is recorded in the RFDs
//! at <https://github.com/bough-frp/rfd>.
//!
//! # Status
//!
//! The engine is being built behind these signatures in stages. Stage 1,
//! the first-order core, works: inputs, coalescing or not, input cells,
//! constants and `never`; the adapters `map`, `filter`, `filter_map`,
//! `map_to`, `snapshot`, `gate` and `once`, which fuse into the one node
//! that materializes them; the materializers `hold`, `node`, `share`,
//! `merge` and `or_else`; and on [`Graph`] transactions, listeners and
//! `sample`. Stage 2 adds the cells: the accumulators `accumulate`,
//! `accumulate_mut` and `scan`, read-through cells with `map_cell` and
//! `lift`, and the stream views `steps` and `steps_with_current`, with
//! [`State`] and [`CellRef`]. Stage 3 adds cell loops, `cell_loop` and
//! [`CellLoop`], closed under the rule that the dependency graph stays
//! acyclic. Every other operation still has a `todo!()` body. The examples
//! in the documentation that call only working operations run, the rest
//! compile, and the guarantees the RFDs make are fixed by `compile_fail`
//! doc tests.
//!
//! # Targets
//!
//! The core is `no_std` over `alloc`. The `std` feature, on by default, adds
//! the thread-id guard on [`Remote`], `Trace` for the standard collections and
//! `Instant`, and the standard mutex under input slots. Where the target has
//! no pointer atomics, on a Cortex-M0, `Threaded`, `Remote` and the unit queue
//! do not exist, and the path from an interrupt handler into the graph is an
//! [`InputSlot`]. The `critical-section` feature guards slots on bare metal;
//! a web build keeps `std` (RFD 7).
//!
//! RFD 2's example: a click counter and its label, a listener that fires
//! now and on every step, one send, and a transaction.
//!
//! ```
//! use std::cell::RefCell;
//! use std::rc::Rc;
//!
//! use bough::{Graph, Source};
//!
//! struct Click;
//!
//! let (mut graph, (clicks_in, label)) = Graph::build(|b| {
//!     let (clicks, clicks_in) = b.input::<Click>();
//!     let count = clicks.accumulate(b, 0u32, |_, n| n + 1);
//!     let label = count.map_cell(b, |n| n.to_string());
//!     (clicks_in, label) // whatever build returns is the edge, and the root set
//! });
//!
//! let shown = Rc::new(RefCell::new(Vec::new()));
//! let _listener = graph.listen_cell(label, {
//!     let shown = shown.clone();
//!     move |text| shown.borrow_mut().push(text.clone()) // fires now, then on every step
//! });
//! graph.send(clicks_in, Click); // one transaction
//! graph.transaction(|tx| {
//!     tx.send(clicks_in, Click); // several sends, one instant
//! });
//! assert_eq!(*shown.borrow(), ["0", "1", "2"]);
//! assert_eq!(graph.sample(label), "2");
//! ```

// The later stages' bodies are still `todo!()`, so their parameters and
// fields are unused, and the engine carries the kinds, fields and entries
// those stages fill in. The names are the documentation, so they stay.
#![allow(dead_code, unused_variables)]
#![warn(missing_docs)]
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

mod build;
mod cell;
mod engine;
mod error;
mod graph;
mod lift;
mod mode;
mod slot;
#[cfg(feature = "smoke")]
mod smoke;
mod source;
mod token;
mod trace;

pub use build::{Build, CellLoop, StreamLoop};
pub use cell::CellRef;
#[cfg(feature = "statistics")]
pub use engine::Statistics;
pub use error::{PoisonedError, PumpError, SendError, TokenError, TransactionSendError};
#[cfg(target_has_atomic = "ptr")]
pub use error::{RemoteSendError, RemoteTransactionError};
pub use graph::{Anchor, CollectionPolicy, Graph, Listener, Transaction};
#[cfg(target_has_atomic = "ptr")]
pub use graph::{Remote, RemoteTransaction};
pub use lift::Lift;
#[cfg(target_has_atomic = "ptr")]
pub use mode::Threaded;
pub use mode::{Accepts, Local, Mode};
pub use slot::InputSlot;
#[cfg(feature = "smoke")]
#[doc(hidden)]
pub use smoke::smoke;
pub use source::{Filter, FilterMap, Gate, Map, MapTo, Node, Once, Snapshot, Source};
pub use token::{Cell, Input, Shared, State, Stream, TokenRef};
pub use trace::{Leaf, Trace, Tracer};
