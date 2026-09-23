//! Threading modes (RFD 6).
//!
//! A [`Graph`](crate::Graph) is `Local` by default. In `Threaded` mode every
//! value and closure the graph stores must be `Send`, checked once at each
//! materialization and at `listen`, and the graph itself is `Send`. Tokens
//! are plain integers and `Send` in every mode. `Threaded` exists only where
//! the target has pointer atomics, since its handles share atomic flags; a
//! Cortex-M0 has `Local` alone. On wasm32 the gate is true, so `Threaded`
//! exists there with no threads to use it, and `Local` is the web mode.

mod sealed {
    pub trait Sealed {}
}

/// A threading mode: [`Local`] or [`Threaded`]. Sealed.
pub trait Mode: sealed::Sealed + 'static {
    /// The flag a [`Listener`](crate::Listener) or an
    /// [`Anchor`](crate::Anchor) shares with its node, so that dropping the
    /// handle needs no graph access: a counted cell in `Local`, an atomic in
    /// `Threaded`. This is why a handle carries the mode.
    #[doc(hidden)]
    type Flag;
}

/// The default mode: one thread builds and drives the graph, and stored
/// values and closures need not be `Send`.
pub struct Local;

/// Everything the graph stores must be `Send`, and the graph is `Send`, so it
/// can be moved into a task or shared behind a mutex.
#[cfg(target_has_atomic = "ptr")]
pub struct Threaded;

impl sealed::Sealed for Local {}
impl Mode for Local {
    type Flag = alloc::rc::Rc<core::cell::Cell<bool>>;
}
#[cfg(target_has_atomic = "ptr")]
impl sealed::Sealed for Threaded {}
#[cfg(target_has_atomic = "ptr")]
impl Mode for Threaded {
    type Flag = alloc::sync::Arc<core::sync::atomic::AtomicBool>;
}

/// Whether a graph in this mode may store a `T`.
///
/// `Local` accepts everything; `Threaded` accepts `Send` types. Every
/// materializer and `listen` carries an `M: Accepts<..>` bound for the values
/// and closures it stores, which is where a non-`Send` capture in a
/// `Threaded` graph fails to compile. On a non-atomics wasm32 build a
/// `JsValue` is `Send`, so `Threaded` accepts it; a `Closure` is not.
pub trait Accepts<T: ?Sized> {}

impl<T: ?Sized> Accepts<T> for Local {}
#[cfg(target_has_atomic = "ptr")]
impl<T: ?Sized + Send> Accepts<T> for Threaded {}
