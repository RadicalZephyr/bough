//! The engine: the storage seam, the node kinds, and the transaction.
//!
//! Crate-private. Items are `pub` only where a hidden trait method names
//! them, and the module is private, so they are unnameable outside the
//! crate.

use core::cell::OnceCell;

/// A stateful cell's value: a hold, an accumulator, a constant, an input
/// cell. Evaluation writes `pending`; commit moves it into `value`, so every
/// read during a transaction sees the value from before the instant.
pub(crate) struct CellValue<A> {
    pub(crate) value: A,
    pub(crate) pending: Option<A>,
}

/// A read-through cell's memo, the value before the instant, in a
/// `OnceCell` so that `sample(&self) -> &A` can compute it; and its
/// post-instant value, written through `&mut` by a steps view and promoted
/// into the memo at commit.
pub(crate) struct Memo<A> {
    pub(crate) value: OnceCell<A>,
    pub(crate) post_value: Option<A>,
}

impl<A> Memo<A> {
    pub(crate) fn new() -> Self {
        Memo {
            value: OnceCell::new(),
            post_value: None,
        }
    }
}
