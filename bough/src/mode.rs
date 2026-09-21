//! Threading modes (RFD 6).
//!
//! A [`Graph`](crate::Graph) is `Local` by default. In `Threaded` mode every
//! value and closure the graph stores must be `Send`, checked once at each
//! materialization and at `listen`, and the graph itself is `Send`. Tokens
//! are plain integers and `Send` in every mode.

mod sealed {
    pub trait Sealed {}
}

/// A threading mode: [`Local`] or [`Threaded`]. Sealed.
pub trait Mode: sealed::Sealed + 'static {}

/// The default mode: one thread builds and drives the graph, and stored
/// values and closures need not be `Send`.
pub struct Local;

/// Everything the graph stores must be `Send`, and the graph is `Send`, so it
/// can be moved into a task or shared behind a mutex.
pub struct Threaded;

impl sealed::Sealed for Local {}
impl Mode for Local {}
impl sealed::Sealed for Threaded {}
impl Mode for Threaded {}

/// Whether a graph in this mode may store a `T`.
///
/// `Local` accepts everything; `Threaded` accepts `Send` types. Every
/// materializer and `listen` carries an `M: Accepts<..>` bound for the values
/// and closures it stores, which is where a non-`Send` capture in a
/// `Threaded` graph fails to compile.
pub trait Accepts<T: ?Sized> {}

impl<T: ?Sized> Accepts<T> for Local {}
impl<T: ?Sized + Send> Accepts<T> for Threaded {}
