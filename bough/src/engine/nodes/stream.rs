//! Stream nodes: coalescing inputs, fused chains, merges.

use core::any::Any;

use super::Marker;
use crate::build::Build;
use crate::engine::{Cx, Data, NodeOps, Ops, part, slot_mut};
use crate::mode::Mode;
use crate::source::Source;

/// `input_coalescing`: an input whose second send in one instant is folded
/// into the first, first send on the left. Its one part is the function.
pub(crate) struct CoalescingInput<A, F>(Marker<(A, F)>);

fn coalesce_input<M, A, F>(parts: &mut [M::Carrier], data: &mut Data<M>, new: &mut dyn Any)
where
    M: Mode,
    A: 'static,
    F: Fn(A, A) -> A + 'static,
{
    let new = new
        .downcast_mut::<Option<A>>()
        .expect("bough engine: send type")
        .take()
        .expect("bough engine: a send carries a value");
    let slot = slot_mut::<M, A>(data);
    // Nothing reads an input's slot before the sends are over, so the first
    // send's event is still there.
    let old = slot
        .take()
        .expect("bough engine: a fired input holds its event during the sends");
    *slot = Some(part::<M, F>(&mut parts[0])(old, new));
}

impl<M, A, F> NodeOps<M> for CoalescingInput<A, F>
where
    M: Mode,
    A: 'static,
    F: Fn(A, A) -> A + 'static,
{
    const OPS: Ops<M> = Ops {
        coalesce: coalesce_input::<M, A, F>,
        ..Ops::<M>::DEFAULT
    };
}

/// `node` and `share`: the fused chain, whose event goes in the slot. The
/// chain's adapters are inlined into one monomorphized `pull`.
pub(crate) struct ChainNode<S>(Marker<S>);

fn eval_chain<M: Mode, S: Source>(parts: &mut [M::Carrier], b: &mut Build<M>, me: u32)
where
    S::Event: 'static,
{
    let chain = part::<M, S>(&mut parts[0]);
    if let Some(v) = chain.pull(&mut Cx { b: &mut *b }) {
        b.put_event(me, v);
    }
}

impl<M: Mode, S: Source> NodeOps<M> for ChainNode<S>
where
    S::Event: 'static,
{
    const OPS: Ops<M> = Ops {
        eval: eval_chain::<M, S>,
        ..Ops::<M>::DEFAULT
    };
}

/// `merge` and `or_else`: two chains and the combining function in one
/// node, `f(left, right)` when both fire. `or_else`'s function is an engine
/// function pointer keeping the left event.
pub(crate) struct MergeNode<S, T, F>(Marker<(S, T, F)>);

fn eval_merge<M, S, T, F>(parts: &mut [M::Carrier], b: &mut Build<M>, me: u32)
where
    M: Mode,
    S: Source,
    T: Source<Event = S::Event>,
    F: Fn(S::Event, S::Event) -> S::Event + 'static,
    S::Event: 'static,
{
    let [left, right, f] = parts else {
        unreachable!("bough engine: a merge has three parts")
    };
    let l = part::<M, S>(left).pull(&mut Cx { b: &mut *b });
    let r = part::<M, T>(right).pull(&mut Cx { b: &mut *b });
    let out = match (l, r) {
        (Some(l), Some(r)) => Some(part::<M, F>(f)(l, r)),
        (l, None) => l,
        (None, r) => r,
    };
    if let Some(v) = out {
        b.put_event(me, v);
    }
}

impl<M, S, T, F> NodeOps<M> for MergeNode<S, T, F>
where
    M: Mode,
    S: Source,
    T: Source<Event = S::Event>,
    F: Fn(S::Event, S::Event) -> S::Event + 'static,
    S::Event: 'static,
{
    const OPS: Ops<M> = Ops {
        eval: eval_merge::<M, S, T, F>,
        ..Ops::<M>::DEFAULT
    };
}
