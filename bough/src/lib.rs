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

mod build;
mod cell;
mod error;
mod graph;
mod mode;
mod source;
mod token;
mod trace;

pub use build::{Build, CellLoop, StreamLoop};
pub use error::{
    InsideTransactionError, PoisonedError, PumpError, RemoteSendError, SendError, TokenError,
    TransactionSendError,
};
pub use graph::{
    Anchor, CollectionPolicy, Graph, Listener, Remote, RemoteTransaction, Transaction,
};
pub use mode::{Accepts, Local, Mode, Threaded};
pub use source::{Filter, FilterMap, Gate, Map, MapTo, Node, Once, Snapshot, Source};
pub use token::{Cell, Input, Shared, Stream, TokenRef};
pub use trace::{Trace, Tracer};
