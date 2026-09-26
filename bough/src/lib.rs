//! Lightweight FRP rooted in Rust.
//!
//! Bough implements the Sodium FRP denotational semantics with an API that
//! cleanly separates building FRP logic from driving it with I/O. Every
//! node-creating operation needs a [`Build`] context, which only exists
//! inside [`Runtime::build`] and inside [`Source::construct`] closures; sending,
//! listening and sampling from outside live on [`Runtime`], which only exists
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
//! `merge` and `or_else`; and on [`Runtime`] transactions, listeners and
//! `sample`. Stage 2 adds the cells: the accumulators `accumulate`,
//! `accumulate_mut` and `scan`, read-through cells with `map_cell` and
//! `lift`, and the stream views `steps` and `steps_with_current`, with
//! [`State`] and [`CellRef`]. Stage 3 adds loops, `cell_loop` with
//! [`CellLoop`], `state_loop` with [`StateLoop`] and `stream_loop` with
//! [`StreamLoop`], closed under the rule that the dependency graph stays
//! acyclic. Stage 4 adds child transactions: `split` emits the elements of
//! an event in the children of its transaction, and `defer` the event in
//! its first child, and the children run depth first before `send`
//! returns. Stage 5 adds the switches, `switch_cell`, over cells and over
//! states, and `switch_stream`, whose selection is not a dependency; each
//! follows the inner its outer selects and moves at commit. Stage 6 adds
//! [`construct`](Source::construct), which runs a closure with the build
//! context at each event of a stream, in the middle of its transaction:
//! what the closure builds exists from that instant on, and each run is a
//! scope that must close the loops it declares. Stage 7 adds collection
//! (RFD 3): a node lives while a root reaches it, a live [`Listener`] or a
//! live [`Anchor`], and [`Runtime::build`] anchors what its closure returns;
//! what it reaches is its dependencies, the tokens [`Trace`] finds in a
//! stateful cell's value, and what [`Build::depends`] declares; and
//! collection, automatic by default and never inside a transaction, frees
//! the rest, so that a stale token is an error. Stage 8 adds the I/O edge (RFD 6, RFD 7): an
//! [`InputSlot`] holds one pending event folded in place, and
//! [`pump`](Runtime::pump) runs each pending slot as a transaction of its
//! own, in connection order; a [`Remote`] queues a send, or a remote
//! transaction's sends, as one unit from any thread, and `pump` then runs
//! each unit as one transaction, in arrival order; a write or a remote
//! send wakes the waker the driver registered with
//! [`set_waker`](Runtime::set_waker). No body is `todo!()` any more. The
//! examples in the documentation run, and the guarantees the RFDs make are
//! fixed by `compile_fail` doc tests.
//!
//! # Targets
//!
//! The core is `no_std` over `alloc`. The `std` feature, on by default, adds
//! the thread-id guard on [`Remote`], `Trace` for the standard collections and
//! `Instant`, and the standard mutex under input slots. Where the target has
//! no pointer atomics, on a Cortex-M0, `Threaded`, `Remote` and the unit queue
//! do not exist, and the path from an interrupt handler into the graph is an
//! [`InputSlot`]. The `critical-section` feature guards slots on bare metal;
//! a web build keeps `std` (RFD 7). A slot and a remote's inbox need one of
//! the two locks: with no `unsafe` in the crate there is none to build from
//! atomics, so a `no_std` build without `critical-section` has neither
//! slots nor `Remote`, and keeps [`pump`](Runtime::pump) and
//! [`set_waker`](Runtime::set_waker).
//!
//! RFD 2's example: a click counter and its label, a listener that fires
//! now and on every step, one send, and a transaction.
//!
//! ```
//! use std::cell::RefCell;
//! use std::rc::Rc;
//!
//! use bough::{Runtime, Source};
//!
//! struct Click;
//!
//! let (mut graph, edge) = Runtime::build(|b| {
//!     let (clicks, clicks_in) = b.input::<Click>();
//!     let count = clicks.accumulate(b, 0u32, |_, n| n + 1);
//!     let label = count.map_cell(b, |n| n.to_string());
//!     (clicks_in, label) // whatever build returns is the edge, anchored
//! });
//! let (clicks_in, label) = edge.keep(); // kept for the graph's life
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

#![warn(missing_docs)]
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

mod build;
mod capabilities;
mod cell;
mod engine;
mod error;
mod guard;
mod lift;
mod mode;
#[cfg(doctest)]
mod refusals;
mod runtime;
#[cfg(any(feature = "std", feature = "critical-section"))]
mod slot;
#[cfg(feature = "smoke")]
mod smoke;
mod source;
mod token;
mod trace;

/// Derives [`Trace`](trait@Trace) for a struct or an enum, with
/// `#[trace(skip)]` for a field that cannot hold tokens.
#[cfg(feature = "derive")]
pub use bough_derive::Trace;
pub use build::{Build, CellLoop, StateLoop, StreamLoop};
pub use cell::CellRef;
#[cfg(feature = "statistics")]
pub use engine::Statistics;
pub use error::{PoisonedError, PumpError, SendError, TokenError, TransactionSendError};
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
pub use error::{RemoteSendError, RemoteTransactionError};
pub use lift::Lift;
#[cfg(target_has_atomic = "ptr")]
pub use mode::Threaded;
pub use mode::{Accepts, Local, Mode};
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
pub use runtime::Remote;
pub use runtime::{
    Anchor, Anchored, CollectionPolicy, IoTransaction, Listener, Runtime, Transaction,
};
#[cfg(any(feature = "std", feature = "critical-section"))]
pub use slot::InputSlot;
#[cfg(feature = "smoke")]
#[doc(hidden)]
pub use smoke::smoke;
pub use source::{Filter, FilterMap, Gate, Map, MapTo, Node, Once, Snapshot, Source};
pub use token::{Cell, Input, Shared, State, Stream, TokenRef};
pub use trace::{Leaf, Trace, Tracer};
