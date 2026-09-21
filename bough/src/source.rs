//! Streams as chains and nodes (RFD 4).
//!
//! Stages transform occurrences and take no context; each returns an adapter
//! type that is itself a [`Source`]. Materializers take the build context and
//! create exactly one node from a chain, fusing the stages between two nodes
//! into that node's closure. A chain is affine: it is consumed by the first
//! materializer, and using it twice is a compile error.
//!
//! ```no_run
//! use bough::{Graph, Source};
//!
//! let (graph, total) = Graph::build(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let (limit, _limit_in) = b.input_cell(10u32);
//!     numbers
//!         .map(|n| n * 2)                 // a stage: no node, no context
//!         .filter(|n| *n > 2)             // another stage
//!         .snapshot(limit, |n, l| n.min(*l))
//!         .hold(b, 0u32)                  // one node for the whole chain
//! });
//! ```
//!
//! A chain used twice does not compile:
//!
//! ```compile_fail,E0382
//! use bough::{Graph, Source};
//!
//! let (graph, _) = Graph::build(|b| {
//!     let (numbers, _in) = b.input::<u32>();
//!     let doubled = numbers.map(|n| n * 2);
//!     let a = doubled.hold(b, 0u32);
//!     let b2 = doubled.hold(b, 0u32); // error: use of moved value
//! });
//! ```

use crate::Build;
use crate::mode::{Accepts, Mode};
use crate::token::{Cell, Shared, Stream};
use crate::trace::Trace;

/// Anything that yields occurrences: a materialized node or a chain of
/// stages.
///
/// The item is an associated type, as with `Iterator`: an adapter type such
/// as [`Map`] cannot implement a generic `Source<A>`, because `A` would appear
/// only in its bounds.
pub trait Source: Sized {
    /// The type of each occurrence.
    type Item;

    // ----- stages: no node, no context -----

    /// Transforms each occurrence, taking it by value.
    fn map<B, F>(self, f: F) -> Map<Self, F>
    where
        F: Fn(Self::Item) -> B + 'static,
    {
        Map { source: self, f }
    }

    /// Keeps the occurrences the predicate accepts.
    fn filter<P>(self, predicate: P) -> Filter<Self, P>
    where
        P: Fn(&Self::Item) -> bool + 'static,
    {
        Filter {
            source: self,
            predicate,
        }
    }

    /// Maps and filters in one step. Sodium's `filterOptional` is
    /// `filter_map(|o| o)`.
    fn filter_map<B, F>(self, f: F) -> FilterMap<Self, F>
    where
        F: Fn(Self::Item) -> Option<B> + 'static,
    {
        FilterMap { source: self, f }
    }

    /// Replaces each occurrence with a clone of one value.
    fn map_to<B>(self, value: B) -> MapTo<Self, B>
    where
        B: Clone + 'static,
    {
        MapTo {
            source: self,
            value,
        }
    }

    /// Combines each occurrence with the value the cell had at the start of
    /// the transaction.
    fn snapshot<B, C, F>(self, cell: Cell<B>, f: F) -> Snapshot<Self, B, F>
    where
        F: Fn(Self::Item, &B) -> C + 'static,
    {
        Snapshot {
            source: self,
            cell,
            f,
        }
    }

    /// Keeps the occurrences during which the cell is `true`.
    fn gate(self, cell: Cell<bool>) -> Gate<Self> {
        Gate { source: self, cell }
    }

    /// Keeps only the first occurrence.
    fn once(self) -> Once<Self> {
        Once { source: self }
    }

    // ----- materializers: one node, build context -----

    /// A cell that holds the latest occurrence, starting at `initial`.
    ///
    /// The hold is the chain's sole consumer and moves the occurrence into
    /// its committed value at commit, so no `Clone` is needed.
    fn hold<M>(self, build: &mut Build<M>, initial: Self::Item) -> Cell<Self::Item>
    where
        M: Mode + Accepts<Self> + Accepts<Self::Item>,
        Self::Item: Trace + 'static,
    {
        todo!()
    }

    /// Sodium's `accum`: `hold initial (snapshot f self cell)`, with `f`
    /// reading the state by reference and returning the new state.
    fn accumulate<M, S, F>(self, build: &mut Build<M>, initial: S, f: F) -> Cell<S>
    where
        M: Mode + Accepts<Self> + Accepts<S> + Accepts<F>,
        S: Trace + 'static,
        F: Fn(Self::Item, &S) -> S + 'static,
    {
        todo!()
    }

    /// In-place accumulation: `f` mutates the state at commit, after every
    /// reader in the transaction has seen the previous state.
    ///
    /// Observationally the same as [`accumulate`](Source::accumulate), and a
    /// `Vec` accumulator becomes a push. The cell has no stream view:
    /// [`steps`](Cell::steps) on it, or on any cell derived from it, is a
    /// build-time error.
    fn accumulate_mut<M, S, F>(self, build: &mut Build<M>, initial: S, f: F) -> Cell<S>
    where
        M: Mode + Accepts<Self> + Accepts<S> + Accepts<F>,
        S: Trace + 'static,
        F: FnMut(Self::Item, &mut S) + 'static,
    {
        todo!()
    }

    /// Sodium's `collect`, `Iterator::scan`: a running state and an output
    /// per occurrence.
    fn scan<M, S, B, F>(self, build: &mut Build<M>, initial: S, f: F) -> Stream<B>
    where
        M: Mode + Accepts<Self> + Accepts<S> + Accepts<F>,
        S: Trace + 'static,
        B: 'static,
        F: Fn(Self::Item, &S) -> (B, S) + 'static,
    {
        todo!()
    }

    /// Explicit fan-out: a stream with any number of consumers, each of which
    /// clones the occurrence. This is the one place `Clone` is required of a
    /// stream's items.
    fn share<M>(self, build: &mut Build<M>) -> Shared<Self::Item>
    where
        M: Mode + Accepts<Self> + Accepts<Self::Item>,
        Self::Item: Clone + 'static,
    {
        todo!()
    }

    /// Materializes a chain as a linear stream with an identity of its own,
    /// so it can be stored in a value or returned from build.
    fn node<M>(self, build: &mut Build<M>) -> Stream<Self::Item>
    where
        M: Mode + Accepts<Self>,
        Self::Item: 'static,
    {
        todo!()
    }

    /// Merges two streams; `f` combines simultaneous occurrences, with this
    /// stream's occurrence on the left. Both inputs move through.
    fn merge<M, T, F>(self, build: &mut Build<M>, other: T, f: F) -> Stream<Self::Item>
    where
        M: Mode + Accepts<Self> + Accepts<T> + Accepts<F>,
        T: Source<Item = Self::Item>,
        F: Fn(Self::Item, Self::Item) -> Self::Item + 'static,
        Self::Item: 'static,
    {
        todo!()
    }

    /// Merges two streams, this stream winning when both fire.
    fn or_else<M, T>(self, build: &mut Build<M>, other: T) -> Stream<Self::Item>
    where
        M: Mode + Accepts<Self> + Accepts<T>,
        T: Source<Item = Self::Item>,
        Self::Item: 'static,
    {
        todo!()
    }

    /// Emits each element of an occurrence in its own child transaction,
    /// which runs after this one and before the next external one.
    fn split<M>(self, build: &mut Build<M>) -> Stream<<Self::Item as IntoIterator>::Item>
    where
        M: Mode + Accepts<Self>,
        Self::Item: IntoIterator + 'static,
        <Self::Item as IntoIterator>::Item: 'static,
    {
        todo!()
    }

    /// Emits each occurrence in a child transaction of its own.
    fn defer<M>(self, build: &mut Build<M>) -> Stream<Self::Item>
    where
        M: Mode + Accepts<Self>,
        Self::Item: 'static,
    {
        todo!()
    }

    /// The semantics' `Execute`: runs `f` at each occurrence with a fresh
    /// build context, so graph can be constructed at runtime. Its results
    /// reach the world only through [`Cell::switch_stream`] and
    /// [`Cell::switch_cell`].
    fn construct<M, B, F>(self, build: &mut Build<M>, f: F) -> Stream<B>
    where
        M: Mode + Accepts<Self> + Accepts<F>,
        B: 'static,
        F: FnMut(&mut Build<M>, Self::Item) -> B + 'static,
    {
        todo!()
    }
}

/// A materialized node: what [`Graph::listen`](crate::Graph::listen) accepts.
/// Adapter types do not implement it, so a chain cannot be listened to.
pub trait Node: Source {}

impl<A> Source for Stream<A> {
    type Item = A;
}
impl<A> Node for Stream<A> {}
impl<A: Clone> Source for Shared<A> {
    type Item = A;
}
impl<A: Clone> Node for Shared<A> {}

/// The adapter returned by [`Source::map`].
pub struct Map<S, F> {
    source: S,
    f: F,
}
impl<S: Source, B, F: Fn(S::Item) -> B + 'static> Source for Map<S, F> {
    type Item = B;
}

/// The adapter returned by [`Source::filter`].
pub struct Filter<S, P> {
    source: S,
    predicate: P,
}
impl<S: Source, P: Fn(&S::Item) -> bool + 'static> Source for Filter<S, P> {
    type Item = S::Item;
}

/// The adapter returned by [`Source::filter_map`].
pub struct FilterMap<S, F> {
    source: S,
    f: F,
}
impl<S: Source, B, F: Fn(S::Item) -> Option<B> + 'static> Source for FilterMap<S, F> {
    type Item = B;
}

/// The adapter returned by [`Source::map_to`].
pub struct MapTo<S, B> {
    source: S,
    value: B,
}
impl<S: Source, B: Clone + 'static> Source for MapTo<S, B> {
    type Item = B;
}

/// The adapter returned by [`Source::snapshot`].
pub struct Snapshot<S, B, F> {
    source: S,
    cell: Cell<B>,
    f: F,
}
impl<S: Source, B, C, F: Fn(S::Item, &B) -> C + 'static> Source for Snapshot<S, B, F> {
    type Item = C;
}

/// The adapter returned by [`Source::gate`].
pub struct Gate<S> {
    source: S,
    cell: Cell<bool>,
}
impl<S: Source> Source for Gate<S> {
    type Item = S::Item;
}

/// The adapter returned by [`Source::once`].
pub struct Once<S> {
    source: S,
}
impl<S: Source> Source for Once<S> {
    type Item = S::Item;
}
