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
//! This is the API skeleton: every public signature, with `todo!()` bodies.
//! The examples in the documentation compile against it, and the guarantees
//! the RFDs make are fixed by `compile_fail` doc tests. The engine lands
//! behind these signatures one increment at a time.
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
//! ```no_run
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
//! let _listener = graph.listen_cell(label, |text| println!("{text}"));
//! graph.send(clicks_in, Click);
//! graph.transaction(|tx| {
//!     tx.send(clicks_in, Click);
//! });
//! ```

// The bodies are `todo!()` until the engine lands, so parameters and fields
// are unused for now. The names are the documentation, so they stay.
#![allow(dead_code, unused_variables)]
#![warn(missing_docs)]
#![no_std]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

mod build;
mod cell;
mod error;
mod graph;
mod lift;
mod mode;
mod slot;
mod source;
mod token;
mod trace;

pub use build::{Build, CellLoop, StreamLoop};
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
pub use source::{Filter, FilterMap, Gate, Map, MapTo, Node, Once, Snapshot, Source};
pub use token::{Cell, Input, Shared, Stream, TokenRef};
pub use trace::{Leaf, Trace, Tracer};
