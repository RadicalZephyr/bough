//! Read-through cells: `map_cell` and `lift`.
//!
//! A read-through cell has no program. Marking orders it like any node,
//! and the evaluation loop settles it without user code: it steps iff one
//! of its inputs stepped. Its function runs when the cell is read, into
//! the memo, a `OnceCell` holding the value before the instant, so that a
//! read through `&Build` can fill it; and when a steps view asks for the
//! value after the instant, into the post-instant value, from its inputs'
//! values after the instant. At commit a cell that stepped promotes that
//! value into the memo, since the value after instant t is the value
//! before t + 1, or clears the memo if nothing computed it; a cell that
//! was marked and did not step keeps its memo (F4). So the function runs
//! once per step even with a steps view and a listener on one cell.

use core::any::Any;
use core::cell::OnceCell;

use super::Marker;
use crate::build::Build;
use crate::engine::{Data, Memo, NodeOps, Ops, Passed, memo_mut};
use crate::mode::{Carrier, Mode};

/// A read-through cell's function over its inputs' values, arity 1 for
/// `map_cell` and 2 to 6 for `lift`. `V` is the tuple of the inputs' value
/// types, so one node type serves every arity.
pub(crate) trait ReadFn<V, R>: 'static {
    /// The function of the inputs' values before the instant, read as
    /// steps of a read that has passed what `passed` says.
    fn values<M: Mode>(&self, b: &Build<M>, inputs: &[u32], passed: Passed) -> R;

    /// The function of the inputs' values after the instant. Every input
    /// has been prepared.
    fn posts<M: Mode>(&self, b: &Build<M>, inputs: &[u32]) -> R;
}

macro_rules! read_fn {
    ($($v:ident $i:tt),+) => {
        impl<$($v: 'static,)+ R, F> ReadFn<($($v,)+), R> for F
        where
            F: Fn($(&$v),+) -> R + 'static,
        {
            fn values<M: Mode>(&self, b: &Build<M>, inputs: &[u32], passed: Passed) -> R {
                self($(b.value_through::<$v>(inputs[$i], passed)),+)
            }

            fn posts<M: Mode>(&self, b: &Build<M>, inputs: &[u32]) -> R {
                self($(b.post::<$v>(inputs[$i])),+)
            }
        }
    };
}

read_fn!(A 0);
read_fn!(A 0, B 1);
read_fn!(A 0, B 1, C 2);
read_fn!(A 0, B 1, C 2, D 3);
read_fn!(A 0, B 1, C 2, D 3, E 4);
read_fn!(A 0, B 1, C 2, D 3, E 4, G 5);

/// `map_cell` and `lift`: the function and the memo in the node's data,
/// and no program.
pub(crate) struct ReadNode<V, R, F>(Marker<(V, R, F)>);

/// The value before the instant: the memo, filled on the first read since
/// the cell last stepped. The inputs are read the same way, so a chain of
/// read-through cells fills its memos from the bottom up. A read that comes
/// back to this cell while it fills the memo would fill it again for ever,
/// since the `OnceCell` panics on re-entry only once an inner fill
/// returns; what the read has passed stops it (`Passed`).
fn value_read<M, V, R, F>(b: &Build<M>, me: u32, passed: Passed) -> &dyn Any
where
    M: Mode,
    V: 'static,
    R: 'static,
    F: ReadFn<V, R>,
{
    let Data::ReadThrough { f, memo } = &b.store.data[me as usize] else {
        unreachable!("bough engine: node {me} is not a read-through cell")
    };
    let memo = memo
        .get()
        .downcast_ref::<Memo<R>>()
        .expect("bough engine: memo type");
    memo.value.get_or_init(|| {
        let f = f
            .get()
            .downcast_ref::<F>()
            .expect("bough engine: function type");
        f.values(b, &b.store.relations[me as usize].deps, passed)
    })
}

/// The value after the instant of a cell that stepped, computed from its
/// inputs' values after the instant for a steps view, and kept beside the
/// memo until commit. The memo itself still holds the value before the
/// instant, which a snapshot in the same instant may read.
fn post_read<M, V, R, F>(b: &mut Build<M>, me: u32)
where
    M: Mode,
    V: 'static,
    R: 'static,
    F: ReadFn<V, R>,
{
    let v = {
        let b: &Build<M> = b;
        let Data::ReadThrough { f, .. } = &b.store.data[me as usize] else {
            unreachable!("bough engine: node {me} is not a read-through cell")
        };
        let f = f
            .get()
            .downcast_ref::<F>()
            .expect("bough engine: function type");
        f.posts(b, &b.store.relations[me as usize].deps)
    };
    memo_mut::<M, R>(&mut b.store.data[me as usize]).post_value = Some(v);
}

/// A cell that stepped, at commit. The value after the instant, if a steps
/// view computed it, is the value before the next instant, so it becomes
/// the memo; otherwise the memo is cleared and the next read computes it.
/// The memo is replaced through `&mut`, which no reader holds at commit.
fn settle_read<M: Mode, R: 'static>(d: &mut Data<M>) {
    let memo = memo_mut::<M, R>(d);
    match memo.post_value.take() {
        Some(v) => memo.value = OnceCell::from(v),
        None => {
            memo.value.take();
        }
    }
}

impl<M, V, R, F> NodeOps<M> for ReadNode<V, R, F>
where
    M: Mode,
    V: 'static,
    R: 'static,
    F: ReadFn<V, R>,
{
    const OPS: Ops<M> = Ops {
        value: value_read::<M, V, R, F>,
        compute_post: post_read::<M, V, R, F>,
        settle_memo: settle_read::<M, R>,
        ..Ops::<M>::DEFAULT
    };
}
