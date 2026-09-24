//! Stateful cells: holds, and input cells, which are holds over an input.

use super::Marker;
use crate::build::Build;
use crate::engine::{Cx, Data, NodeOps, Ops, cell_mut, part};
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
