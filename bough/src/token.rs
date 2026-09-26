//! Tokens: the names I/O code and graph code hold for nodes.
//!
//! A token is an index, a generation and a graph id. It has no method that
//! creates a node without a [`Build`](crate::Build) context. `Cell`, `State`,
//! `Input` and `Shared` are `Copy` for every event type; `Stream` is
//! move-only because it is linear (RFD 4).

use core::fmt;
use core::hash::{Hash, Hasher};
use core::marker::PhantomData;

/// The node reference inside every token. `pub` so the sealed accessor may
/// name it, but the module is private, so it is unnameable from outside.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Token {
    pub(crate) index: u32,
    pub(crate) generation: u32,
    pub(crate) graph: u32,
}

pub(crate) mod sealed {
    /// Implemented by the five token types and nothing else.
    pub trait Sealed {
        fn token(&self) -> super::Token;
    }
}

/// Anything that names a node: the five token types.
///
/// Used by [`Build::depends`](crate::Build::depends), which takes a slice of
/// any tokens. Every token type is also [`Trace`](crate::Trace), visiting
/// itself, so [`Runtime::anchor`](crate::Runtime::anchor), which takes any
/// value that holds tokens, takes a single token too.
pub trait TokenRef: sealed::Sealed {}

/// A linear stream of events.
///
/// A `Stream` has exactly one consumer: every constructor takes it by value,
/// and using it twice is a compile error. To give it more than one consumer,
/// [`share`](crate::Source::share) it. A toolkit's event becomes a Bough
/// event when I/O code sends it into an input. The `PhantomData<fn() -> A>` keeps the
/// token `Send` and `Sync` whatever `A` is; a token is three integers.
pub struct Stream<A> {
    pub(crate) token: Token,
    event: PhantomData<fn() -> A>,
}

/// A stream with any number of consumers, each of which clones the event.
///
/// Produced by [`share`](crate::Source::share), which requires `A: Clone`.
pub struct Shared<A> {
    pub(crate) token: Token,
    event: PhantomData<fn() -> A>,
}

/// A value that exists at every instant.
///
/// Cell values are read by reference and are never cloned by the engine. A
/// cell is either a hold, which moves its event into its committed value
/// at commit, or a read-through cell computed from other cells on demand.
pub struct Cell<A> {
    pub(crate) token: Token,
    event: PhantomData<fn() -> A>,
}

/// A cell whose new value does not exist until commit: the state of an
/// in-place accumulator, [`accumulate_mut`](crate::Source::accumulate_mut),
/// and every read-through cell computed from one.
///
/// A `State` is read like a [`Cell`]. `sample`, `snapshot`, `gate`,
/// `map_cell`, `lift` and the cell listeners on
/// [`Runtime`](crate::Runtime) accept either, through
/// [`CellRef`](crate::CellRef); `map_cell` over a `State` is a `State`, and
/// so is a `lift` with a `State` among its inputs. What a `State` lacks is
/// a stream view. `steps` and `steps_with_current` carry a cell's value
/// after the instant, during the instant, and an in-place accumulator's
/// function runs only at commit, after every reader has seen the state
/// from before the instant. The listeners read after commit, so
/// [`Runtime::listen_steps`](crate::Runtime::listen_steps) and
/// [`Runtime::listen_cell`](crate::Runtime::listen_cell) work on a `State`.
///
/// ```compile_fail,E0599
/// use bough::{Runtime, Source};
///
/// let (_graph, _) = Runtime::build(|b| {
///     let (names, _names_in) = b.input::<String>();
///     let members = names.accumulate_mut(b, Vec::new(), |name, m: &mut Vec<String>| m.push(name));
///     let _joined = members.steps(b); // error: no method named `steps` found for struct `State`
/// });
/// ```
pub struct State<A> {
    pub(crate) token: Token,
    event: PhantomData<fn() -> A>,
}

/// The I/O side of an input: the token that [`Runtime::send`](crate::Runtime::send)
/// takes.
///
/// `Copy`, because several I/O sources may drive one input and because
/// inputs travel as data. Two sends to a non-coalescing input in one
/// transaction are an error, and that is a rule about one instant that no
/// token discipline could make static.
pub struct Input<A> {
    pub(crate) token: Token,
    event: PhantomData<fn() -> A>,
}

macro_rules! token_impls {
    ($name:ident, $doc:literal) => {
        impl<A> $name<A> {
            pub(crate) fn from_token(token: Token) -> Self {
                Self {
                    token,
                    event: PhantomData,
                }
            }
        }
        impl<A> sealed::Sealed for $name<A> {
            fn token(&self) -> Token {
                self.token
            }
        }
        impl<A> TokenRef for $name<A> {}
        impl<A> PartialEq for $name<A> {
            fn eq(&self, other: &Self) -> bool {
                self.token == other.token
            }
        }
        impl<A> Eq for $name<A> {}
        impl<A> Hash for $name<A> {
            fn hash<H: Hasher>(&self, state: &mut H) {
                self.token.hash(state)
            }
        }
        impl<A> fmt::Debug for $name<A> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(
                    f,
                    concat!($doc, "#{}@{}"),
                    self.token.index, self.token.graph
                )
            }
        }
    };
}
token_impls!(Stream, "Stream");
token_impls!(Shared, "Shared");
token_impls!(Cell, "Cell");
token_impls!(State, "State");
token_impls!(Input, "Input");

// Unconditional Copy through hand-written impls, so `Cell<String>` is Copy.
macro_rules! copy_impls {
    ($name:ident) => {
        impl<A> Clone for $name<A> {
            fn clone(&self) -> Self {
                *self
            }
        }
        impl<A> Copy for $name<A> {}
    };
}
copy_impls!(Shared);
copy_impls!(Cell);
copy_impls!(State);
copy_impls!(Input);
