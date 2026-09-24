//! The I/O edge's engine side (RFD 6, RFD 7): the lock the edge's shared
//! state lives behind, the input slots connected to the graph, and the
//! waker the driver registered.
//!
//! A slot is shared between the code that writes it, an interrupt handler
//! or another thread, and the driver, so its state needs a lock. With no
//! `unsafe` in the crate there is no lock to build from atomics: `core` and
//! `alloc` have no interior mutability that is `Sync`, and a spin lock
//! would need an `UnsafeCell` and would deadlock against an interrupt that
//! preempts the driver while it holds it. So the lock is the standard
//! mutex under `std`, a critical section with the `critical-section`
//! feature, and nothing otherwise: a `no_std` build without that feature
//! has no input slots.

use core::any::Any;
#[cfg(any(feature = "std", feature = "critical-section"))]
use core::task::Waker;

#[cfg(any(feature = "std", feature = "critical-section"))]
use alloc::vec::Vec;

use super::DoubleSend;
use crate::build::Build;
use crate::mode::Mode;
#[cfg(any(feature = "std", feature = "critical-section"))]
use crate::token::Token;

/// The lock under the edge's shared state: the standard mutex.
///
/// Only a slot's fold and the clone or drop of a waker run under it, never
/// a transaction, so a panic there leaves the state consistent and a
/// poisoned mutex is used as it is.
#[cfg(feature = "std")]
pub(crate) struct Lock<T>(std::sync::Mutex<T>);

#[cfg(feature = "std")]
impl<T> Lock<T> {
    pub(crate) const fn new(value: T) -> Self {
        Lock(std::sync::Mutex::new(value))
    }

    /// Runs `f` with the state locked.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut state)
    }
}

/// The lock under the edge's shared state on bare metal: a critical
/// section, which the target's HAL provides through the
/// `critical-section` crate.
#[cfg(all(not(feature = "std"), feature = "critical-section"))]
pub(crate) struct Lock<T>(critical_section::Mutex<core::cell::RefCell<T>>);

#[cfg(all(not(feature = "std"), feature = "critical-section"))]
impl<T> Lock<T> {
    pub(crate) const fn new(value: T) -> Self {
        Lock(critical_section::Mutex::new(core::cell::RefCell::new(
            value,
        )))
    }

    /// Runs `f` inside a critical section.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        critical_section::with(|cs| f(&mut self.0.borrow_ref_mut(cs)))
    }
}

/// What the graph needs from an input slot whose event type it does not
/// know. `Sync`, so that a graph holding `&'static dyn Drain`s is `Send`
/// where its mode says it is.
#[cfg(any(feature = "std", feature = "critical-section"))]
pub(crate) trait Drain: Sync {
    /// Connects the slot to graph `graph`, with the graph's waker if it has
    /// one. False if the slot is connected already.
    fn connect(&self, graph: u32, waker: Option<&Waker>) -> bool;
    /// Takes the pending event, if there is one, and hands it to `fire` as
    /// an `&mut Option<A>`, after the lock is released.
    fn drain(&self, fire: &mut dyn FnMut(&mut dyn Any));
    /// Replaces the waker a write wakes.
    fn set_waker(&self, waker: Option<Waker>);
    /// Forgets the graph and the waker, and drops a pending event.
    fn disconnect(&self);
}

/// A slot connected to an input, in connection order.
#[cfg(any(feature = "std", feature = "critical-section"))]
#[derive(Clone, Copy)]
pub(crate) struct Connection {
    pub(crate) input: Token,
    pub(crate) slot: &'static dyn Drain,
}

/// The graph's side of the edge. It lives in the build context, since
/// [`connect`](crate::Build::connect) takes one.
pub(crate) struct Edge {
    /// The waker the driver registered; a slot connected later gets it too.
    #[cfg(any(feature = "std", feature = "critical-section"))]
    pub(crate) waker: Option<Waker>,
    /// The connected slots, in connection order.
    #[cfg(any(feature = "std", feature = "critical-section"))]
    pub(crate) slots: Vec<Connection>,
}

impl Edge {
    pub(crate) fn new() -> Self {
        Edge {
            #[cfg(any(feature = "std", feature = "critical-section"))]
            waker: None,
            #[cfg(any(feature = "std", feature = "critical-section"))]
            slots: Vec::new(),
        }
    }
}

/// A graph that goes away, dropped or unwound out of a panicking build,
/// disconnects its slots, so that each can be connected again and no event
/// written for this graph reaches another.
impl Drop for Edge {
    fn drop(&mut self) {
        #[cfg(any(feature = "std", feature = "critical-section"))]
        for connection in self.slots.drain(..) {
            connection.slot.disconnect();
        }
    }
}

/// `ops.fire` of an input whose events are `A`s: takes the event out of
/// `event`, an `Option<A>`, and starts the input with it. How a value whose
/// type the caller does not know, a slot's or a remote unit's, reaches the
/// typed [`fire_start`](Build::fire_start).
pub(crate) fn fire_input<M: Mode, A: 'static>(
    b: &mut Build<M>,
    input: u32,
    event: &mut dyn Any,
) -> Result<(), DoubleSend> {
    let event = event
        .downcast_mut::<Option<A>>()
        .expect("bough engine: send type")
        .take()
        .expect("bough engine: a send carries a value");
    b.fire_start(input, event)
}
