//! Errors of the `try_` variants (RFD 5).
//!
//! Each family of operations with the same failure modes has its own type,
//! and no type carries a variant that one of its operations cannot return.
//! The bounded engine, when it lands, adds `Exhausted` to three of these and
//! is a major version for it.

use core::error::Error;
use core::fmt;

/// Failure modes of [`Runtime::try_send`](crate::Runtime::try_send). One send
/// opens one transaction, so no double send can occur here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// The input's node was collected. The panicking variant treats this as
    /// a debug-mode panic and a release-mode no-op.
    Stale,
    /// The token belongs to another graph.
    ForeignGraph,
    /// A previous transaction never finished: a panic escaped it.
    Poisoned,
}

/// Failure modes of [`Transaction::try_send`](crate::Transaction::try_send).
/// Poisoning is checked once, when the transaction is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionSendError {
    /// The input's node was collected.
    Stale,
    /// The token belongs to another graph.
    ForeignGraph,
    /// A second send to a non-coalescing input in one transaction.
    DoubleSend,
}

/// The graph is poisoned: a previous transaction never finished, because a
/// panic escaped it.
///
/// Returned by `try_transaction` and `try_collect_garbage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoisonedError;

/// Failure modes of `try_listen`, `try_listen_cell`, `try_listen_steps`,
/// `try_anchor` and `try_sample`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    /// The node was collected.
    Stale,
    /// The token belongs to another graph.
    ForeignGraph,
    /// A previous transaction never finished: a panic escaped it.
    Poisoned,
}

/// Failure modes of a call through a handle, an [`Io`](crate::Io) or a
/// `RemoteIo`. A call only queues, so these are what the handle knows
/// without the runtime; what needs the graph, such as a stale token, is
/// found at the pump, as [`PumpError`]. A call checks them in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoError {
    /// The runtime was dropped.
    Gone,
    /// The runtime is poisoned: a previous transaction never finished. The
    /// handle knows once an entry on the runtime has found the poison.
    Poisoned,
    /// Called from graph code: a `map` function, a `construct` closure, a
    /// split's iterator. That's I/O inside FRP logic. A listener is I/O
    /// code, so its calls queue. A `RemoteIo` checks this under `std`
    /// only, where a thread has an id.
    FromGraphCode,
    /// A token the call names belongs to another graph. A transaction's
    /// closure hides its tokens, so a foreign one there is found at the
    /// pump instead.
    ForeignGraph,
}

/// Failure modes of [`Runtime::try_pump`](crate::Runtime::try_pump).
///
/// Whether a node is collected or an input coalesces is graph knowledge,
/// so a queued call, or a slot connected to an input since collected, can
/// only fail when the driver pumps. The offending call or slot is dropped
/// whole and the error returned; the rest stay pending for the next pump. An [`Io`](crate::Io) queues units on every target, so
/// every variant can occur everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PumpError {
    /// A previous transaction never finished: a panic escaped it.
    Poisoned,
    /// A queued unit sends to an input that was collected before the driver
    /// pumped, a queued registration names a collected node, or a slot
    /// with a pending event is connected to a collected input.
    Stale,
    /// A queued unit sent twice to a non-coalescing input.
    DoubleSend,
    /// A queued transaction sent with a token from another graph. Its
    /// closure runs on the driver, so this is found there; every other
    /// call a handle makes is checked when it is queued.
    ForeignGraph,
}

macro_rules! display_error {
    ($ty:ty, $text:literal) => {
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str($text)
            }
        }
        impl Error for $ty {}
    };
}
display_error!(SendError, "send failed");
display_error!(TransactionSendError, "send inside a transaction failed");
display_error!(PoisonedError, "the graph is poisoned by an earlier panic");
display_error!(
    TokenError,
    "the token is stale, foreign, or the graph is poisoned"
);
display_error!(PumpError, "pump failed");
display_error!(IoError, "the runtime refused a call through its handle");
