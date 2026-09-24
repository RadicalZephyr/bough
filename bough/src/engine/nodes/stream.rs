//! Stream nodes: inputs, open stream loops, fused chains, merges, and the
//! stream views of a cell.

use core::any::Any;

use super::Marker;
use crate::build::Build;
use crate::engine::{Cx, Data, NodeOps, Ops, clear_slot, part, slot_mut};
use crate::mode::Mode;
use crate::source::Source;

/// A stream node with no program whose slot holds `A`s: an input, which a
/// send fires, and a split's or a defer's output, which the child
/// scheduler fires. Collection empties its slot.
pub(crate) struct SlotNode<A>(Marker<A>);

impl<M: Mode, A: 'static> NodeOps<M> for SlotNode<A> {
    const OPS: Ops<M> = Ops {
        clear_slot: clear_slot::<M, A>,
        ..Ops::<M>::DEFAULT
    };
}

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
        clear_slot: clear_slot::<M, A>,
        ..Ops::<M>::DEFAULT
    };
}

/// A stream loop's forward before close: a stream node with no data and a
/// program that panics if run. Nothing runs it: the forward has no
/// dependency until close, so marking never reaches it, and a scope ends
/// with a panic, before its new-node phase, if a loop declared in it is
/// still open. Close replaces the program with the definition's chain.
pub(crate) struct OpenLoopNode;

fn eval_open_loop<M: Mode>(_: &mut [M::Carrier], _: &mut Build<M>, me: u32) {
    panic!("bough engine: stream loop {me} ran before it was closed")
}

impl<M: Mode> NodeOps<M> for OpenLoopNode {
    const OPS: Ops<M> = Ops {
        eval: eval_open_loop::<M>,
        ..Ops::<M>::DEFAULT
    };
}

/// `node`, `share` and a closed stream loop: the fused chain, whose event
/// goes in the slot. The chain's adapters are inlined into one
/// monomorphized `pull`.
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
        clear_slot: clear_slot::<M, S::Event>,
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
        clear_slot: clear_slot::<M, S::Event>,
        ..Ops::<M>::DEFAULT
    };
}

/// `steps` and, with `CURRENT`, `steps_with_current`: a stream node over one
/// cell, its one dependency, that fires when the cell steps with a clone of
/// the cell's value after the instant. `steps_with_current` also fires at
/// its creation instant; a creation and a step at one instant are one
/// event carrying the value after it, the semantics' `coalesce (flip
/// const)` in `Value`, because the node runs once.
pub(crate) struct StepsNode<A, const CURRENT: bool>(Marker<A>);

fn eval_steps<M, A, const CURRENT: bool>(_: &mut [M::Carrier], b: &mut Build<M>, me: u32)
where
    M: Mode,
    A: Clone + 'static,
{
    let cell = b.store.relations[me as usize].deps[0];
    let tx = b.tx;
    let fires = b.store.hot[cell as usize].fired == tx
        || (CURRENT && b.store.hot[me as usize].created == tx);
    if !fires {
        return;
    }
    // The mutable phase runs what the value after the instant reads and
    // computes read-through values after the instant; the shared phase
    // reads it. Every steps view calls prepare, whatever its cell is, so
    // no flag fixed at materialization decides whether there is work.
    b.prepare(cell);
    let v = b.post::<A>(cell).clone();
    b.put_event(me, v);
}

impl<M, A, const CURRENT: bool> NodeOps<M> for StepsNode<A, CURRENT>
where
    M: Mode,
    A: Clone + 'static,
{
    const OPS: Ops<M> = Ops {
        eval: eval_steps::<M, A, CURRENT>,
        clear_slot: clear_slot::<M, A>,
        ..Ops::<M>::DEFAULT
    };
}
