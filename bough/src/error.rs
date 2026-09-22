//! Errors of the `try_` variants (RFD 5).
//!
//! Each family of operations with the same failure modes has its own type,
//! and no type carries a variant that one of its operations cannot return.

use std::error::Error;
use std::fmt;

/// Failure modes of [`Graph::try_send`](crate::Graph::try_send).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// The input's node was collected. The panicking variant treats this as
    /// a debug-mode panic and a release-mode no-op.
    Stale,
    /// The token belongs to another graph.
    ForeignGraph,
    /// A second send to a non-coalescing input in one transaction.
    DoubleSend,
    /// A previous panic escaped `send`.
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

/// The graph is poisoned: a previous panic escaped `send`.
///
/// Returned by `try_transaction`, `try_collect_garbage`, `try_pump` and
/// `try_remote`.
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
    /// A previous panic escaped `send`.
    Poisoned,
}

/// Failure modes of [`Graph::try_pump`](crate::Graph::try_pump).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PumpError {
    /// A previous panic escaped `send`.
    Poisoned,
    /// A remote transaction sent twice to a non-coalescing input. Whether an
    /// input coalesces is graph knowledge, so this is only discoverable here.
    /// The offending transaction is dropped and the rest stay queued.
    DoubleSend,
}

/// Failure modes of [`Remote::try_send`](crate::Remote::try_send).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteSendError {
    /// The token belongs to another graph.
    ForeignGraph,
    /// Called on the driver thread while a transaction is evaluating: I/O
    /// from inside graph code.
    InsideTransaction,
}

/// [`Remote::try_transaction`](crate::Remote::try_transaction) was called on
/// the driver thread while a transaction is evaluating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InsideTransactionError;

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
display_error!(RemoteSendError, "remote send failed");
display_error!(
    InsideTransactionError,
    "remote transaction requested from inside a transaction"
);
