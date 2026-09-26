//! Threading modes (RFD 6).
//!
//! A [`Runtime`](crate::Runtime) is `Local` by default. In `Threaded` mode every
//! value and closure the graph stores must be `Send`, checked once at each
//! materialization and at `listen`, and the graph itself is `Send`. Tokens
//! are plain integers and `Send` in every mode. `Threaded` exists only where
//! the target has pointer atomics, since only there is the state a runtime
//! shares with its guards atomic; a Cortex-M0 has `Local` alone. On wasm32 the gate is true, so `Threaded`
//! exists there with no threads to use it, and `Local` is the web mode.
//!
//! Every value, closure and chain a node stores is erased into the mode's
//! carrier: `Box<dyn Any>` in `Local`, `Box<dyn Any + Send>` in `Threaded`.
//! Exactly two functions build a carrier: [`Accepts::erase`], whose
//! `Threaded` impl exists only for `T: Send`, and `Mode::erase_send`, which
//! requires `T: Send` itself. So `Runtime<Threaded>: Send` is derived by the
//! compiler from the field types, with no `unsafe impl`, and a materializer
//! that forgets an `Accepts` bound fails to compile inside the engine.
//!
//! A `Local` graph is not `Send`, and nothing in the crate says so by hand:
//!
//! ```compile_fail,E0277
//! fn assert_send<T: Send>() {}
//! assert_send::<bough::Runtime<bough::Local>>(); // error: dyn Any cannot be sent between threads
//! ```
//!
//! A `Threaded` graph refuses a closure that captures something that is not
//! `Send`, at the materializer that stores it:
//!
//! ```compile_fail,E0277
//! use bough::{Runtime, Source};
//! use std::rc::Rc;
//!
//! let offset = Rc::new(5u32);
//! let (_graph, edge) = Runtime::build_threaded(move |b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let _held = numbers.map(move |n| n + *offset).hold(b, 0u32); // error: Rc is not Send
//! });
//! edge.keep();
//! ```

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::any::Any;

use crate::engine::{CellValue, Memo};

mod sealed {
    pub trait Sealed {}
}

/// A threading mode: [`Local`] or [`Threaded`]. Sealed.
pub trait Mode: sealed::Sealed + Sized + 'static {
    /// What every stored value, closure and chain is erased into:
    /// `Box<dyn Any>` in `Local`, `Box<dyn Any + Send>` in `Threaded`.
    #[doc(hidden)]
    type Carrier: Carrier;

    /// Erases a value the engine makes that is `Send` whatever the user's
    /// types are, such as a token inside `input_cell`'s chain or `or_else`'s
    /// function pointer. Generic code cannot prove `M: Accepts<Stream<A>>`,
    /// but it can prove `Stream<A>: Send`.
    #[doc(hidden)]
    fn erase_send<T: Send + 'static>(value: T) -> Self::Carrier;
}

/// Access to an erased part: a checked downcast, never an unchecked one.
#[doc(hidden)]
pub trait Carrier: 'static {
    /// The erased value, for a checked downcast by reference.
    fn get(&self) -> &dyn Any;
    /// The erased value, for a checked downcast by mutable reference.
    fn get_mut(&mut self) -> &mut dyn Any;
}

impl Carrier for Box<dyn Any> {
    fn get(&self) -> &dyn Any {
        &**self
    }
    fn get_mut(&mut self) -> &mut dyn Any {
        &mut **self
    }
}

impl Carrier for Box<dyn Any + Send> {
    fn get(&self) -> &dyn Any {
        &**self
    }
    fn get_mut(&mut self) -> &mut dyn Any {
        &mut **self
    }
}

/// The closed set of shapes the engine stores a user type `T` in, fixed in
/// stage 1 of the engine. Each shape is a concrete container of exactly one
/// `T`, so inside the `Threaded` impl, where `T: Send` is known, every shape
/// is `Send` and coerces to `Box<dyn Any + Send>`.
#[doc(hidden)]
pub enum Erase<T> {
    /// `T` itself: a chain, a closure, a state.
    Value(T),
    /// `Option<T>`, empty: a stream node's slot, a pending event.
    Slot,
    /// A stateful cell: its committed value, and no pending value yet.
    Cell(T),
    /// A read-through cell's memo and post-instant value, both empty.
    Memo,
    /// `Vec<Option<T>>`, empty: a split's iterators or a defer's events, one
    /// per depth at which the node fired.
    Stack,
}

/// Whether a graph in this mode may store a `T`.
///
/// `Local` accepts everything; `Threaded` accepts `Send` types. Every
/// materializer and `listen` carries an `M: Accepts<..>` bound for the values
/// and closures it stores, which is where a non-`Send` capture in a
/// `Threaded` graph fails to compile. On a non-atomics wasm32 build a
/// `JsValue` is `Send`, so `Threaded` accepts it; a `Closure` is not.
///
/// `Mode` is a supertrait, and the hidden method is the only way the engine
/// stores a user type, so a bound left off a materializer does not compile.
/// Downstream impls were never possible: coherence rejects
/// `impl Accepts<NotSend> for Threaded`.
///
/// A function generic over the mode builds graph with the bounds its
/// materializers need. It takes its closures from its caller, whose mode is
/// concrete, or uses function pointers, because a bound can name those
/// types:
///
/// ```
/// use bough::{Accepts, Build, Cell, Map, Mode, Source, Stream};
///
/// fn plus<M, F>(b: &mut Build<M>, numbers: Stream<u32>, f: F) -> Cell<u32>
/// where
///     M: Mode + Accepts<u32> + Accepts<Map<Stream<u32>, F>>,
///     F: Fn(u32) -> u32 + 'static,
/// {
///     numbers.map(f).hold(b, 0)
/// }
/// ```
///
/// It cannot make a closure of its own and store it, since the bound would
/// have to name the closure's type:
///
/// ```compile_fail,E0277
/// use bough::{Accepts, Build, Cell, Mode, Source, Stream};
///
/// fn plus_one<M: Mode + Accepts<u32>>(b: &mut Build<M>, numbers: Stream<u32>) -> Cell<u32> {
///     numbers.map(|n| n + 1).hold(b, 0) // error: M: Accepts<Map<Stream<u32>, {closure}>>
/// }
/// ```
pub trait Accepts<T: ?Sized>: Mode {
    /// Erases `T` in one of the engine's shapes.
    #[doc(hidden)]
    fn erase(what: Erase<T>) -> Self::Carrier
    where
        T: Sized + 'static;
}

/// The body of both `erase` impls: the same match, typed at the carrier of
/// the impl it expands in.
macro_rules! erase_body {
    ($what:expr, $t:ty) => {
        match $what {
            Erase::Value(value) => Box::new(value),
            Erase::Slot => Box::new(None::<$t>),
            Erase::Cell(value) => Box::new(CellValue {
                value,
                pending: None,
            }),
            Erase::Memo => Box::new(Memo::<$t>::new()),
            Erase::Stack => Box::new(Vec::<Option<$t>>::new()),
        }
    };
}

/// The default mode: one thread builds and drives the graph, and stored
/// values and closures need not be `Send`.
pub struct Local;

impl sealed::Sealed for Local {}
impl Mode for Local {
    type Carrier = Box<dyn Any>;
    fn erase_send<T: Send + 'static>(value: T) -> Box<dyn Any> {
        Box::new(value)
    }
}

impl<T: ?Sized> Accepts<T> for Local {
    fn erase(what: Erase<T>) -> Box<dyn Any>
    where
        T: Sized + 'static,
    {
        erase_body!(what, T)
    }
}

/// Everything the graph stores must be `Send`, and the graph is `Send`, so it
/// can be moved into a task or shared behind a mutex.
#[cfg(target_has_atomic = "ptr")]
pub struct Threaded;

#[cfg(target_has_atomic = "ptr")]
impl sealed::Sealed for Threaded {}
#[cfg(target_has_atomic = "ptr")]
impl Mode for Threaded {
    type Carrier = Box<dyn Any + Send>;
    fn erase_send<T: Send + 'static>(value: T) -> Box<dyn Any + Send> {
        Box::new(value)
    }
}

#[cfg(target_has_atomic = "ptr")]
impl<T: ?Sized + Send> Accepts<T> for Threaded {
    fn erase(what: Erase<T>) -> Box<dyn Any + Send>
    where
        T: Sized + 'static,
    {
        erase_body!(what, T)
    }
}
