//! Input slots: the path from an interrupt handler into a graph (RFD 7).

use core::any::Any;
use core::mem;
use core::task::Waker;

use crate::engine::edge::{Drain, Lock};

/// A static mailbox for one input: written from any context, folded in
/// place, drained by the driver.
///
/// A slot holds one pending event. A write when one is pending folds the two
/// with the slot's fold, pending on the left, so the slot never grows and a
/// write never allocates; a burst of writes between two pumps becomes one
/// event. The driver's [`pump`](crate::Runtime::pump) runs each pending slot
/// as one transaction of its own, higher priority first, so two slots are
/// never simultaneous: simultaneity means one external cause, which a
/// tuple input or a remote transaction declares, never the timing of a
/// drain.
///
/// The fold law: a slot's sequence of writes is cut into runs by when the
/// driver pumps, which is timing the semantics do not see, and each run
/// folds left to right, in the order of the writes, into one event. So the
/// fold must be associative, for the answer not to depend on where the
/// cuts fall. It need not be commutative, because a slot has one producer;
/// two producers writing one slot would interleave by timing, so each gets
/// a slot of its own.
///
/// A slot is a value the writer owns, which for an interrupt handler means a
/// `static`; the fold is a `fn` pointer so that the type can be named there,
/// and a closure that captures nothing converts to one. Dropping the older
/// event is [`keep_latest`](InputSlot::keep_latest), declared rather than
/// silent. Build connects the slot to an input with
/// [`connect`](crate::Build::connect).
///
/// ```
/// use bough::{Runtime, InputSlot, Source};
///
/// static PRESSES: InputSlot<u32> = InputSlot::new(|a, b| a + b);
///
/// let (mut graph, edge) = Runtime::build(|b| {
///     let (presses, presses_in) = b.input::<u32>();
///     b.connect(presses_in, &PRESSES, 0);
///     presses.accumulate(b, 0u32, |n, total| total + n)
/// });
/// let total = edge.keep();
///
/// // from an interrupt handler, or any other context: a burst
/// PRESSES.send(1);
/// PRESSES.send(2);
///
/// // from the driver: the burst is one event, one transaction
/// graph.pump();
/// assert_eq!(*graph.sample(total), 3);
/// ```
///
/// Under `std` the slot is guarded by the standard mutex; without `std`, by
/// the critical section the target's HAL provides through the
/// `critical-section` feature. A `no_std` build without that feature has no
/// slots: with no `unsafe` in the crate there is no lock to build from
/// atomics, and a spin lock would deadlock against an interrupt that
/// preempts the driver while it drains. A web build keeps `std`; a `no_std`
/// web build registers a no-op critical section itself, since the crate
/// ships none for wasm. The fold runs under the lock, so it should be as
/// cheap as an interrupt handler needs; a fold that panics loses the
/// pending event.
pub struct InputSlot<A> {
    fold: fn(A, A) -> A,
    state: Lock<Pending<A>>,
}

/// A slot's state behind its lock.
struct Pending<A> {
    /// The event written since the last drain, folded.
    event: Option<A>,
    /// What a write wakes.
    waker: Option<Waker>,
    /// The graph the slot is connected to, or 0.
    graph: u32,
}

impl<A: Send> InputSlot<A> {
    /// A slot whose pending event and a new event are folded with `fold`,
    /// pending on the left. The fold must be associative.
    pub const fn new(fold: fn(A, A) -> A) -> Self {
        InputSlot {
            fold,
            state: Lock::new(Pending {
                event: None,
                waker: None,
                graph: 0,
            }),
        }
    }

    /// A slot that keeps only the latest event, dropping the pending one. The
    /// drop is declared here rather than silent.
    pub const fn keep_latest() -> Self {
        Self::new(|_, latest| latest)
    }

    /// Writes one event: folds it into the pending event if there is one, and
    /// wakes the driver. Never blocks on the graph and never allocates; the
    /// wake runs after the lock is released.
    pub fn send(&self, value: A) {
        let waker = self.state.with(|s| {
            s.event = Some(match s.event.take() {
                Some(pending) => (self.fold)(pending, value),
                None => value,
            });
            s.waker.clone()
        });
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Registers the waker a write wakes, until the graph the slot is
    /// connected to registers its own with
    /// [`Runtime::set_waker`](crate::Runtime::set_waker), which reaches every
    /// connected slot. A bare-metal main loop that sleeps on the interrupt
    /// itself needs none.
    pub fn set_waker(&self, waker: Waker) {
        let old = self.state.with(|s| s.waker.replace(waker));
        drop(old);
    }
}

impl<A: Send + 'static> Drain for InputSlot<A> {
    fn connect(&self, graph: u32, waker: Option<&Waker>) -> bool {
        let old = self.state.with(|s| {
            if s.graph != 0 {
                return Err(());
            }
            s.graph = graph;
            Ok(match waker {
                Some(waker) => s.waker.replace(waker.clone()),
                None => None,
            })
        });
        // A waker's drop is its owner's code: it runs outside the lock.
        old.is_ok()
    }

    fn drain(&self, fire: &mut dyn FnMut(&mut dyn Any)) -> bool {
        let Some(event) = self.state.with(|s| s.event.take()) else {
            return false;
        };
        fire(&mut Some(event));
        true
    }

    fn set_waker(&self, waker: Option<Waker>) {
        let old = self.state.with(|s| mem::replace(&mut s.waker, waker));
        drop(old);
    }

    fn disconnect(&self) {
        let (event, waker) = self.state.with(|s| {
            s.graph = 0;
            (s.event.take(), s.waker.take())
        });
        drop((event, waker));
    }
}
