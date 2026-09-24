//! Stateful cells: holds, input cells (holds over an input), accumulators
//! and in-place accumulators; and `scan`, a stream whose private state is
//! the same fold.

use super::Marker;
use crate::build::Build;
use crate::engine::{Cx, Data, NodeOps, Ops, cell_mut, in_place_mut, part};
use crate::mode::Mode;
use crate::source::Source;

/// `hold`: the chain's event becomes `pending`, and commit moves it into
/// the value. The hold is the chain's sole consumer, so the event moves
/// through without `Clone`.
pub(crate) struct HoldNode<S>(Marker<S>);

fn eval_hold<M: Mode, S: Source>(parts: &mut [M::Carrier], b: &mut Build<M>, me: u32)
where
    S::Event: 'static,
{
    if let Some(v) = part::<M, S>(&mut parts[0]).pull(&mut Cx { b: &mut *b }) {
        b.put_pending(me, v);
    }
}

/// A stateful cell's commit: the pending value, if the cell stepped,
/// replaces the value. The old value is dropped here, after every reader in
/// the transaction has seen it.
pub(crate) fn commit_cell<M: Mode, A: 'static>(_: &mut [M::Carrier], d: &mut Data<M>) {
    let c = cell_mut::<M, A>(d);
    if let Some(v) = c.pending.take() {
        c.value = v;
    }
}

impl<M: Mode, S: Source> NodeOps<M> for HoldNode<S>
where
    S::Event: 'static,
{
    const OPS: Ops<M> = Ops {
        eval: eval_hold::<M, S>,
        commit: commit_cell::<M, S::Event>,
        ..Ops::<M>::DEFAULT
    };
}

/// `accumulate`: the semantics' knot, `hold initial (snapshot f s c)` where
/// `c` is the hold itself. The node's data stays in the arena while its
/// program runs, so the function reads the accumulator's own committed
/// value, the value before the instant, which is what the knot's snapshot
/// of itself reads. The rest is a hold.
pub(crate) struct AccumulateNode<S, St, F>(Marker<(S, St, F)>);

fn eval_accumulate<M, S, St, F>(parts: &mut [M::Carrier], b: &mut Build<M>, me: u32)
where
    M: Mode,
    S: Source,
    St: 'static,
    F: Fn(S::Event, &St) -> St + 'static,
{
    let [chain, f] = parts else {
        unreachable!("bough engine: an accumulator has two parts")
    };
    let Some(event) = part::<M, S>(chain).pull(&mut Cx { b: &mut *b }) else {
        return;
    };
    let next = part::<M, F>(f)(event, b.value::<St>(me));
    b.put_pending(me, next);
}

impl<M, S, St, F> NodeOps<M> for AccumulateNode<S, St, F>
where
    M: Mode,
    S: Source,
    St: 'static,
    F: Fn(S::Event, &St) -> St + 'static,
{
    const OPS: Ops<M> = Ops {
        eval: eval_accumulate::<M, S, St, F>,
        commit: commit_cell::<M, St>,
        ..Ops::<M>::DEFAULT
    };
}

/// `accumulate_mut`: the event waits in the node's pending slot, and the
/// function mutates the state at commit, after every reader in the
/// transaction has seen the state from before the instant. The node's token
/// is a `State`, which has no stream view, so nothing asks it for a value
/// after the instant, which does not exist until commit.
pub(crate) struct InPlaceNode<S, St, F>(Marker<(S, St, F)>);

fn eval_in_place<M, S, St>(parts: &mut [M::Carrier], b: &mut Build<M>, me: u32)
where
    M: Mode,
    S: Source,
    St: 'static,
    S::Event: 'static,
{
    let Some(event) = part::<M, S>(&mut parts[0]).pull(&mut Cx { b: &mut *b }) else {
        return;
    };
    *in_place_mut::<M, St, S::Event>(&mut b.store.data[me as usize]).1 = Some(event);
    b.set_fired(me);
}

fn commit_in_place<M, S, St, F>(parts: &mut [M::Carrier], d: &mut Data<M>)
where
    M: Mode,
    S: Source,
    St: 'static,
    S::Event: 'static,
    F: FnMut(S::Event, &mut St) + 'static,
{
    let (state, pending) = in_place_mut::<M, St, S::Event>(d);
    if let Some(event) = pending.take() {
        part::<M, F>(&mut parts[1])(event, state);
    }
}

impl<M, S, St, F> NodeOps<M> for InPlaceNode<S, St, F>
where
    M: Mode,
    S: Source,
    St: 'static,
    S::Event: 'static,
    F: FnMut(S::Event, &mut St) + 'static,
{
    const OPS: Ops<M> = Ops {
        eval: eval_in_place::<M, S, St>,
        commit: commit_in_place::<M, S, St, F>,
        ..Ops::<M>::DEFAULT
    };
}

/// `scan`, Sodium's `collect`: at each event the function maps the event
/// and the state to an output and the next state. The state lives in the
/// node's program, where nothing else can read it, and is updated during
/// evaluation. A node runs at most once per instant, so, as with `once`'s
/// flag, that cannot be told from updating it at commit: the function
/// always reads the state from before the instant.
pub(crate) struct ScanNode<S, St, B, F>(Marker<(S, St, B, F)>);

fn eval_scan<M, S, St, B, F>(parts: &mut [M::Carrier], b: &mut Build<M>, me: u32)
where
    M: Mode,
    S: Source,
    St: 'static,
    B: 'static,
    F: Fn(S::Event, &St) -> (B, St) + 'static,
{
    let [chain, f, state] = parts else {
        unreachable!("bough engine: a scan has three parts")
    };
    let Some(event) = part::<M, S>(chain).pull(&mut Cx { b: &mut *b }) else {
        return;
    };
    let state = part::<M, St>(state);
    let (out, next) = part::<M, F>(f)(event, state);
    *state = next;
    b.put_event(me, out);
}

impl<M, S, St, B, F> NodeOps<M> for ScanNode<S, St, B, F>
where
    M: Mode,
    S: Source,
    St: 'static,
    B: 'static,
    F: Fn(S::Event, &St) -> (B, St) + 'static,
{
    const OPS: Ops<M> = Ops {
        eval: eval_scan::<M, S, St, B, F>,
        ..Ops::<M>::DEFAULT
    };
}
