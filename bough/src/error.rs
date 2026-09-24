//! Errors of the `try_` variants (RFD 5).
//!
//! Each family of operations with the same failure modes has its own type,
//! and no type carries a variant that one of its operations cannot return.
//! The bounded engine, when it lands, adds `Exhausted` to three of these and
//! is a major version for it.

use core::error::Error;
use core::fmt;

/// Failure modes of [`Graph::try_send`](crate::Graph::try_send). One send
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
/// Returned by `try_transaction`, `try_collect_garbage` and `try_remote`.
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

/// Failure modes of [`Graph::try_pump`](crate::Graph::try_pump).
///
/// Whether an input is collected or coalesces is graph knowledge, so a
/// send inside a queued unit, or a slot connected to an input since
/// collected, can only fail when the driver pumps. The offending unit or
/// slot is dropped whole and the error returned; the rest stay pending for
/// the next pump. Where the target has no `Remote` only `Poisoned` and
/// `Stale` can occur; the variants stay, so that a match is portable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PumpError {
    /// A previous transaction never finished: a panic escaped it.
    Poisoned,
    /// A queued unit sends to an input that was collected before the driver
    /// pumped, or a slot with a pending event is connected to one.
    Stale,
    /// A queued unit sent twice to a non-coalescing input.
    DoubleSend,
    /// A remote transaction sent with a token from another graph. Its
    /// closure runs on the driver, so this is found there; a single remote
    /// send is checked when it is queued.
    ForeignGraph,
}

/// Failure modes of [`Remote::try_send`](crate::Remote::try_send).
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteSendError {
    /// The token belongs to another graph.
    ForeignGraph,
    /// Called on the driver thread while a transaction runs: I/O from inside
    /// graph code. Checked under `std`, where a thread id exists.
    InsideTransaction,
    /// The graph is poisoned; the inbox mirrors the bit, so no thread keeps
    /// filling an inbox that no pump will drain.
    Poisoned,
    /// The graph was dropped, so nothing will drain the inbox and every
    /// input is gone.
    GraphDropped,
}

/// Failure modes of
/// [`Remote::try_transaction`](crate::Remote::try_transaction).
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteTransactionError {
    /// Called on the driver thread while a transaction runs. Checked under
    /// `std`, where a thread id exists.
    InsideTransaction,
    /// The graph is poisoned.
    Poisoned,
    /// The graph was dropped.
    GraphDropped,
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
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
display_error!(RemoteSendError, "remote send failed");
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
display_error!(RemoteTransactionError, "remote transaction failed");
