//! `construct`, the semantics' `Execute`: a stream node whose program runs
//! a closure with the whole build context at each event, and whose event is
//! what the closure returns.
//!
//! The program leaves the arena while it runs, as every program does, so the
//! closure can be handed `&mut Build`. What it creates is appended to the
//! arena, which may reallocate; nothing holds a reference into it across the
//! call, and the node's own event goes in its slot by index afterwards. A
//! node the closure creates exists from this instant: it is linked to its
//! dependencies at once, which is harmless since marking is over, and runs
//! in this instant's new-node phase, by pull over its dependencies, once the
//! closure has returned and this node has fired. So it may depend on this
//! node's own event, through a loop.
//!
//! Each run is a scope: a loop the closure declares must close in it. And a
//! run that ends with the build context of another graph in its place, which
//! `mem::swap` with a nested `Runtime::build`'s context can do in safe code,
//! panics before touching the arena, which poisons the graph.

use super::Marker;
use crate::build::Build;
use crate::context::Portable;
use crate::engine::{Cx, NodeOps, Ops, clear_slot, part};
use crate::mode::Mode;
use crate::source::Source;

/// `construct` over a chain `S`, running `F`, whose events are `B`s. Its
/// parts are the chain and the closure. `V` says what the closure takes:
/// the builder itself ([`Direct`]), or a [`Portable`] view of it
/// ([`ThroughPortable`]).
pub(crate) struct ConstructNode<S, F, B, V>(Marker<(S, F, B, V)>);

/// How a construct node hands its builder to its closure.
pub trait Via<M: Mode, F, E, B> {
    /// Runs the closure with the builder.
    fn call(f: &mut F, b: &mut Build<M>, event: E) -> B;
}

/// A closure that takes `&mut Build<M>`: a `construct` built on a `Build`.
pub struct Direct;

/// A closure that takes `&mut Portable`: a `construct` built in a portable
/// core, whichever mode its builder is.
pub struct ThroughPortable;

impl<M: Mode, F, E, B> Via<M, F, E, B> for Direct
where
    F: FnMut(&mut Build<M>, E) -> B,
{
    fn call(f: &mut F, b: &mut Build<M>, event: E) -> B {
        f(b, event)
    }
}

impl<M: Mode, F, E, B> Via<M, F, E, B> for ThroughPortable
where
    F: for<'p> FnMut(&mut Portable<'p>, E) -> B,
{
    fn call(f: &mut F, b: &mut Build<M>, event: E) -> B {
        f(&mut M::portable(b), event)
    }
}

fn eval_construct<M, S, F, B, V>(parts: &mut [M::Carrier], b: &mut Build<M>, me: u32)
where
    M: Mode,
    S: Source,
    B: 'static,
    F: 'static,
    V: Via<M, F, S::Event, B>,
{
    let [chain, f] = parts else {
        unreachable!("bough engine: a construct has two parts")
    };
    let Some(event) = part::<M, S>(chain).pull(&mut Cx { b: &mut *b }) else {
        return;
    };
    let graph = b.graph_id;
    b.push_scope();
    let out = V::call(part::<M, F>(f), b, event);
    // A `mem::swap` with another graph's build context is safe code; with
    // no `unsafe` in the engine it is a wrong-graph error, caught here
    // before anything indexes the arena with this node's index.
    assert!(
        b.graph_id == graph,
        "bough: a construct closure swapped its build context for another graph's"
    );
    b.pop_scope();
    b.put_event(me, out);
}

impl<M, S, F, B, V> NodeOps<M> for ConstructNode<S, F, B, V>
where
    M: Mode,
    S: Source,
    B: 'static,
    F: 'static,
    V: Via<M, F, S::Event, B>,
{
    const OPS: Ops<M> = Ops {
        eval: eval_construct::<M, S, F, B, V>,
        clear_slot: clear_slot::<M, B>,
        ..Ops::<M>::DEFAULT
    };
}
