//! Errors of the `try_` variants (RFD 5).
//!
//! Each family of operations with the same failure modes has its own type,
//! and no type carries a variant that one of its operations cannot return.
//! The bounded engine, when it lands, adds `Exhausted` to three of these and
//! is a major version for it.

#[cfg(any(feature = "undo", feature = "stage"))]
use alloc::string::String;
#[cfg(any(feature = "undo", feature = "stage"))]
use alloc::vec::Vec;
use core::error::Error;
use core::fmt;

/// A unit the engine refused, under the rollback probe's `undo` or `stage`
/// with [`Runtime::set_rollback`](crate::Runtime::set_rollback) on: it
/// failed, the engine rolled it back, and the graph is as it was before
/// it. The events it sent are dropped.
#[cfg(any(feature = "undo", feature = "stage"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The node whose code failed, by number, which is the engine's only
    /// name for it; `None` when no node's code was running, as when a
    /// switch's move closes a cycle.
    pub node: Option<u32>,
    /// What failed: a panic's message, a cycle, or a closure's error.
    pub message: String,
    /// The nodes the unit's events started, by number: the events it
    /// dropped.
    pub inputs: Vec<u32>,
}

#[cfg(any(feature = "undo", feature = "stage"))]
impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("refused a transaction: ")?;
        if let Some(node) = self.node {
            write!(f, "node {node} failed: ")?;
        }
        write!(f, "{}; dropped its events at ", self.message)?;
        for (k, input) in self.inputs.iter().enumerate() {
            let sep = if k == 0 { "" } else { ", " };
            write!(f, "{sep}node {input}")?;
        }
        Ok(())
    }
}

#[cfg(any(feature = "undo", feature = "stage"))]
impl Error for Refusal {}

/// Failure modes of [`Runtime::try_send`](crate::Runtime::try_send). One send
/// opens one transaction, so no double send can occur here.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(any(feature = "undo", feature = "stage")), derive(Copy))]
pub enum SendError {
    /// The input's node was collected. The panicking variant treats this as
    /// a debug-mode panic and a release-mode no-op.
    Stale,
    /// The token belongs to another graph.
    ForeignGraph,
    /// A previous transaction never finished: a panic escaped it.
    Poisoned,
    /// The transaction failed and was rolled back (the rollback probe).
    #[cfg(any(feature = "undo", feature = "stage"))]
    Refused(Refusal),
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

/// Failure modes of the `Runtime`'s `try_listen`, `try_listen_cell`,
/// `try_listen_steps`, `try_listen_once`, `try_listen_cell_once`,
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
    /// handle knows as soon as the panic leaves the runtime, where panics
    /// unwind, and otherwise once an entry on the runtime has found it.
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

/// Failure modes of queuing a transaction through a handle,
/// [`Io::transaction`](crate::Io::transaction) or `RemoteIo::transaction`,
/// checked in this order: those of [`IoError`] but a foreign token. A
/// transaction's closure hides the tokens it names, so a foreign one is
/// found at the pump, as [`PumpError::ForeignGraph`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoTransactionError {
    /// The runtime was dropped.
    Gone,
    /// The runtime is poisoned, as for [`IoError::Poisoned`].
    Poisoned,
    /// Called from graph code, as for [`IoError::FromGraphCode`].
    FromGraphCode,
}

/// A handle's call that names tokens can fail for every reason its
/// transaction can, and one more, so `?` takes the one into the other.
impl From<IoTransactionError> for IoError {
    fn from(error: IoTransactionError) -> IoError {
        match error {
            IoTransactionError::Gone => IoError::Gone,
            IoTransactionError::Poisoned => IoError::Poisoned,
            IoTransactionError::FromGraphCode => IoError::FromGraphCode,
        }
    }
}

/// Failure modes of tying a once-listener to a transaction,
/// [`Transaction::try_listen_once`](crate::Transaction::try_listen_once)
/// and
/// [`Transaction::try_listen_cell_once`](crate::Transaction::try_listen_cell_once).
/// A transaction only opens on a runtime that isn't poisoned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionListenError {
    /// The node was collected.
    Stale,
    /// The token belongs to another graph.
    ForeignGraph,
}

/// Failure modes of [`Runtime::try_pump`](crate::Runtime::try_pump).
///
/// Whether a node is collected or an input coalesces is graph knowledge,
/// so a queued call, or a slot connected to an input since collected, can
/// only fail when the driver pumps. The offending call or slot is dropped
/// whole and the error returned; the rest stay pending for the next pump. An [`Io`](crate::Io) queues units on every target, so
/// every variant can occur everywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(any(feature = "undo", feature = "stage")), derive(Copy))]
pub enum PumpError {
    /// A previous transaction never finished: a panic escaped it.
    Poisoned,
    /// A unit failed and was rolled back (the rollback probe); the rest
    /// stay pending.
    #[cfg(any(feature = "undo", feature = "stage"))]
    Refused(Refusal),
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
display_error!(
    IoTransactionError,
    "the runtime refused a transaction through its handle"
);
display_error!(TransactionListenError, "the token is stale or foreign");
