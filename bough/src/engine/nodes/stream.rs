//! Stream nodes: inputs.

use core::any::Any;

use super::Marker;
use crate::engine::{Data, NodeOps, Ops, part, slot_mut};
use crate::mode::Mode;

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
