//! `split`: two nodes. The capture takes the event at the instant it fires
//! in, as its chain's one consumer, and keeps an iterator over the event's
//! elements for the child scheduler. The output is a stream node with no
//! dependencies, started by the scheduler in each child instant with the
//! next element. The output does not depend on the capture, since it fires
//! at a later instant, so marking never passes from one to the other and a
//! loop through them has no cycle.
//!
//! A capture's program is its chain and a stack with one entry per level of
//! child instants it has fired in and whose children are not done: a loop
//! can feed it again inside its own children. The scheduler always runs the
//! innermost level, whose entries are on top.

use alloc::vec::Vec;

use super::Marker;
use crate::build::Build;
use crate::engine::{Cx, NodeOps, Ops, part};
use crate::mode::Mode;
use crate::source::Source;

/// The iterator a split keeps between its child instants.
type Iter<S> = <<S as Source>::Event as IntoIterator>::IntoIter;

/// `split`'s capture: the chain, and a stack of iterators over the events'
/// elements. An iterator that ends is dropped at once, so an iterator that
/// is not fused cannot add elements after its end.
pub(crate) struct SplitNode<S>(Marker<S>);

fn eval_split<M, S>(parts: &mut [M::Carrier], b: &mut Build<M>, me: u32)
where
    M: Mode,
    S: Source,
    S::Event: IntoIterator,
    Iter<S>: 'static,
{
    let [chain, stack] = parts else {
        unreachable!("bough engine: a split capture has two parts")
    };
    if let Some(event) = part::<M, S>(chain).pull(&mut Cx { b: &mut *b }) {
        part::<M, Vec<Option<Iter<S>>>>(stack).push(Some(event.into_iter()));
        b.capture_fired(me);
    }
}

/// One child instant: the next element of the top iterator starts the
/// output. `false` once the iterator has ended.
fn emit_split<M, S>(parts: &mut [M::Carrier], b: &mut Build<M>, me: u32) -> bool
where
    M: Mode,
    S: Source,
    S::Event: IntoIterator,
    Iter<S>: 'static,
    <S::Event as IntoIterator>::Item: 'static,
{
    let top = part::<M, Vec<Option<Iter<S>>>>(&mut parts[1])
        .last_mut()
        .expect("bough engine: a capture in a level has an entry for that level");
    match top.as_mut().and_then(Iterator::next) {
        Some(item) => {
            b.fire_child(me, item);
            true
        }
        None => {
            *top = None;
            false
        }
    }
}

/// The level that pushed the top entry is done.
fn end_split<M, S>(parts: &mut [M::Carrier])
where
    M: Mode,
    S: Source,
    S::Event: IntoIterator,
    Iter<S>: 'static,
{
    part::<M, Vec<Option<Iter<S>>>>(&mut parts[1]).pop();
}

impl<M, S> NodeOps<M> for SplitNode<S>
where
    M: Mode,
    S: Source,
    S::Event: IntoIterator,
    Iter<S>: 'static,
    <S::Event as IntoIterator>::Item: 'static,
{
    const OPS: Ops<M> = Ops {
        eval: eval_split::<M, S>,
        emit_child: emit_split::<M, S>,
        end_children: end_split::<M, S>,
        ..Ops::<M>::DEFAULT
    };
}
