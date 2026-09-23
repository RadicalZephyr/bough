//! Input slots: the path from an interrupt handler into a graph (RFD 7).

use core::marker::PhantomData;
use core::task::Waker;

/// A static mailbox for one input: written from any context, folded in
/// place, drained by the driver.
///
/// A slot holds one pending event. A write when one is pending folds the two
/// with the slot's fold, so the slot never grows; a burst of writes between
/// two pumps becomes one event. The fold must be associative, because the
/// engine folds left to right in arrival order, and a slot has one producer.
/// The driver's [`pump`](crate::Graph::pump) runs each pending slot as one
/// transaction of its own, so two slots are never simultaneous.
///
/// A slot is a value the writer owns, which for an interrupt handler means a
/// `static`; the fold is a `fn` pointer so that the type can be named there.
/// Build connects it to an input with [`connect`](crate::Build::connect).
///
/// ```no_run
/// use bough::{Graph, InputSlot, Source};
///
/// static PRESSES: InputSlot<u32> = InputSlot::new(|a, b| a + b);
///
/// let (mut graph, _) = Graph::build(|b| {
///     let (presses, presses_in) = b.input::<u32>();
///     b.connect(presses_in, &PRESSES);
///     presses.hold(b, 0)
/// });
///
/// // from an interrupt handler, or any other context
/// PRESSES.send(1);
///
/// // from the driver
/// graph.pump();
/// ```
///
/// Under `std` the slot is guarded by the standard mutex; with the
/// `critical-section` feature, by the critical section the target's HAL
/// provides. A web build keeps `std`; a `no_std` web build registers a no-op
/// critical section itself, since the crate ships none for wasm.
pub struct InputSlot<A> {
    fold: fn(A, A) -> A,
    // The pending event and the waker live behind the mode's lock once the
    // inbox increment lands; the skeleton records only the fold.
    pending: PhantomData<fn() -> A>,
}

impl<A: Send> InputSlot<A> {
    /// A slot whose pending event and a new event are folded with `fold`,
    /// pending on the left.
    pub const fn new(fold: fn(A, A) -> A) -> Self {
        Self {
            fold,
            pending: PhantomData,
        }
    }

    /// A slot that keeps only the latest event, dropping the pending one. The
    /// drop is declared here rather than silent.
    pub const fn keep_latest() -> Self {
        Self::new(|_, latest| latest)
    }

    /// Writes one event: folds it into the pending event if there is one, and
    /// wakes the driver. Never blocks and never allocates.
    pub fn send(&self, value: A) {
        todo!()
    }

    /// Registers the waker a write wakes. A driver that is a future stores
    /// `cx.waker().clone()`; a bare-metal main loop that sleeps on the
    /// interrupt itself needs none.
    pub fn set_waker(&self, waker: Waker) {
        todo!()
    }
}
