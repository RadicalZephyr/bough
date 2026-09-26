//! Collection (RFD 3): liveness is reachability from explicit roots.
//!
//! The roots are every listener whose handle is live and every live
//! anchor, the build closure's return value among them. What a node keeps alive, its reach, is its dependencies, the
//! tokens a `Trace` walk finds in its committed value (a hold's, an
//! accumulator's, an in-place accumulator's state, `scan`'s state), and
//! what `cold.reach` records: what a chain's `Trace` visits when its node
//! is built (the cells it snapshots or gates on and `map_to`'s value),
//! `depends` declarations, a split output's capture and a switch_stream's
//! outer. A switch's current inner is a dependency, and is also in its
//! outer's value. Reach is wider than dependency, and only collection
//! follows it: marking a transaction follows dependents alone.
//!
//! A collection empties every stream slot, so that an event holding a
//! token neither roots nor dangles and an event type needs no `Trace`;
//! marks from the roots with a reused stack and the visit epoch the path
//! checks use, skipping any token that fails the graph or generation check;
//! frees every live node it did not mark, which bumps the slot's
//! generation and puts it on the free list or retires it (`Store::free`);
//! and prunes the dead out of the survivors' dependents lists, watcher
//! lists and linear-stream claims. Nothing is counted, so a cycle through
//! values is collected like anything else.
//!
//! It never runs inside a transaction. While it runs it holds the
//! transaction-in-progress flag, which is the poison: user code runs in it,
//! the `Drop` of the events, values and closures it frees, and a panic
//! there leaves the arena half swept, so it poisons the graph like a panic
//! in a transaction.

use alloc::vec::Vec;
use core::mem;

use super::{Data, LISTENERS, LIVE, NOOP, WATCHED, cell, in_place, slot_mut};
use crate::build::Build;
use crate::io::IoQueue;
use crate::mode::Mode;
use crate::trace::{Trace, Tracer};

/// `ops.clear_slot` of a stream node whose events are `A`s.
pub(crate) fn clear_slot<M: Mode, A: 'static>(d: &mut Data<M>) {
    *slot_mut::<M, A>(d) = None;
}

/// `ops.trace` of a stateful cell holding `A`s: its committed value, and
/// its pending value, which is empty between transactions.
pub(crate) fn trace_cell<M: Mode, A: Trace + 'static>(
    d: &Data<M>,
    _: &[M::Carrier],
    tracer: &mut Tracer,
) {
    let c = cell::<M, A>(d);
    c.value.trace(tracer);
    if let Some(pending) = &c.pending {
        pending.trace(tracer);
    }
}

/// `ops.trace` of an in-place accumulator: its state.
pub(crate) fn trace_in_place<M: Mode, S: Trace + 'static>(
    d: &Data<M>,
    _: &[M::Carrier],
    tracer: &mut Tracer,
) {
    in_place::<M, S>(d).trace(tracer);
}

impl<M: Mode> Build<M> {
    /// Collects every node that no root reaches, and returns how many it
    /// freed. The released anchors are taken out of `anchors` here.
    pub(crate) fn collect(&mut self) -> usize {
        assert!(
            !self.in_tx,
            "bough: the graph is poisoned: a panic escaped an earlier transaction"
        );
        // The poison: set while user code can run in a `Drop`.
        self.in_tx = true;
        let before = self.store.live;
        self.clear_slots();
        self.s.visit_epoch += 1;
        let epoch = self.s.visit_epoch;
        let mut gray = mem::take(&mut self.s.gray);
        self.anchors.retain(|(_, flag)| flag.is_live());
        for k in 0..self.anchors.len() {
            let i = self.anchors[k].0;
            self.shade(&mut gray, epoch, i);
        }
        // What the calls waiting in the handles' queues name. A token that
        // was stale when its call was made names nothing.
        let mut waiting = Vec::new();
        self.io.roots(&mut waiting);
        #[cfg(all(
            target_has_atomic = "ptr",
            any(feature = "std", feature = "critical-section")
        ))]
        self.edge.inbox.roots(&mut waiting);
        for token in waiting {
            if let Ok(i) = self.lookup(token) {
                self.shade(&mut gray, epoch, i);
            }
        }
        self.shade_listened(&mut gray, epoch);
        self.mark_reach(&mut gray, epoch);
        self.s.gray = gray;
        self.sweep(epoch);
        let freed = before - self.store.live;
        if freed > 0 {
            self.prune();
        }
        self.in_tx = false;
        freed
    }

    /// Empties the slot of every live stream node.
    fn clear_slots(&mut self) {
        let store = &mut self.store;
        for n in 1..store.hot.len() {
            if store.hot[n].flags & LIVE != 0 {
                (store.ops[n].clear_slot)(&mut store.data[n]);
            }
        }
    }

    /// Marks node `i` reached, and queues it to have its reach marked.
    fn shade(&mut self, gray: &mut Vec<u32>, epoch: u64, i: u32) {
        debug_assert!(self.store.hot[i as usize].flags & LIVE != 0);
        let cold = &mut self.store.cold[i as usize];
        if cold.visit != epoch {
            cold.visit = epoch;
            gray.push(i);
        }
    }

    /// Drops every listener whose handle was dropped, and marks every node
    /// that still has a live one.
    fn shade_listened(&mut self, gray: &mut Vec<u32>, epoch: u64) {
        for n in 1..self.store.hot.len() {
            if self.store.hot[n].flags & LISTENERS == 0 {
                continue;
            }
            let list = &mut self.store.listeners[n];
            list.retain(|e| e.flag.is_live());
            if list.is_empty() {
                self.store.hot[n].flags &= !LISTENERS;
            } else {
                self.shade(gray, epoch, n as u32);
            }
        }
    }

    /// Marks everything the queued nodes reach: dependencies, recorded
    /// reach, and the tokens in committed values.
    fn mark_reach(&mut self, gray: &mut Vec<u32>, epoch: u64) {
        let mut traced = mem::take(&mut self.s.traced);
        while let Some(n) = gray.pop() {
            let at = n as usize;
            let mut k = 0;
            while k < self.store.relations[at].deps.len() {
                let d = self.store.relations[at].deps[k];
                self.shade(gray, epoch, d);
                k += 1;
            }
            let mut k = 0;
            while k < self.store.cold[at].reach.len() {
                let r = self.store.cold[at].reach[k];
                self.shade(gray, epoch, r);
                k += 1;
            }
            let mut tracer = Tracer { visited: traced };
            let parts = self.store.parts[at].as_deref().unwrap_or(&[]);
            (self.store.ops[at].trace)(&self.store.data[at], parts, &mut tracer);
            traced = tracer.visited;
            for token in traced.drain(..) {
                // A token of another graph, or one whose node is gone, is
                // no reach: an undeclared capture can put such a token in a
                // value, and its next use is the error.
                if let Ok(i) = self.lookup(token) {
                    self.shade(gray, epoch, i);
                }
            }
        }
        self.s.traced = traced;
    }

    /// Frees every live node the marking did not reach, in index order.
    fn sweep(&mut self, epoch: u64) {
        for n in 1..self.store.hot.len() {
            if self.store.hot[n].flags & LIVE != 0 && self.store.cold[n].visit != epoch {
                self.store.free(n as u32);
            }
        }
    }

    /// Takes the freed nodes out of what the survivors record: dependents,
    /// which marking follows, watchers, and linear-stream claims, so that a
    /// reused slot is never mistaken for the node it held before. A live
    /// node's dependencies and reach are live, since it reached them.
    fn prune(&mut self) {
        let store = &mut self.store;
        let hot = &mut store.hot;
        for n in 1..hot.len() {
            if hot[n].flags & LIVE == 0 {
                continue;
            }
            let live = |i: &u32| hot[*i as usize].flags & LIVE != 0;
            store.relations[n].dependents.retain(live);
            let cold = &mut store.cold[n];
            let consumer = cold.linear_consumer;
            if consumer != NOOP && !live(&consumer) {
                cold.linear_consumer = NOOP;
            }
            if hot[n].flags & WATCHED != 0 {
                cold.watchers.retain(live);
                if cold.watchers.is_empty() {
                    hot[n].flags &= !WATCHED;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use crate::build::Build;
    use crate::engine::TokenFault;
    use crate::guard::Liveness;
    use crate::mode::Local;
    use crate::token::Token;

    /// Builds `n` constants in a transaction of their own and returns their
    /// tokens.
    fn constants(b: &mut Build<Local>, n: u32) -> Vec<Token> {
        b.begin();
        b.push_scope();
        let tokens = (0..n).map(|k| b.constant(k).token).collect();
        b.pop_scope();
        b.finish();
        tokens
    }

    /// Collects with `roots` anchored for this collection alone.
    fn collect_with(b: &mut Build<Local>, roots: &[Token]) -> usize {
        let flag = Liveness::new(&b.released);
        for &token in roots {
            let i = b.lookup(token).expect("a live root");
            b.anchors.push((i, flag.clone()));
        }
        let freed = b.collect();
        flag.release();
        freed
    }

    fn indices(tokens: &[Token]) -> Vec<u32> {
        tokens.iter().map(|t| t.index).collect()
    }

    /// A freed slot goes on the back of the free list and a new node takes
    /// the front, so the slot freed first is reused first, whatever its
    /// index: node 3, freed by the first collection, before node 1, freed
    /// by the second. A last-in first-out list, or one that takes the
    /// lowest index, would reuse node 1 first. A reused slot carries the
    /// next generation, so the old token is stale.
    #[test]
    fn freed_slots_are_reused_oldest_first_under_a_new_generation() {
        let mut b = Build::<Local>::new();
        let first = constants(&mut b, 3);
        assert_eq!(indices(&first), [1, 2, 3]);
        assert_eq!(collect_with(&mut b, &first[..2]), 1);
        assert_eq!(collect_with(&mut b, &first[1..2]), 1);
        assert_eq!(b.store.free.iter().copied().collect::<Vec<_>>(), [3, 1]);
        let again = constants(&mut b, 3);
        assert_eq!(indices(&again), [3, 1, 4]);
        assert_eq!(again[0].generation, first[2].generation + 1);
        assert_eq!(b.lookup(first[2]), Err(TokenFault::Stale));
        assert_eq!(b.lookup(again[0]), Ok(3));
        assert_eq!(b.store.live, 4);
    }

    /// A slot whose generation reaches `u32::MAX` when it is freed is
    /// retired: it is never reused, so no token from before a wrap can
    /// validate. Four billion collections of one slot are forced here by
    /// setting its generation directly.
    #[test]
    fn a_slot_freed_at_the_maximum_generation_is_retired() {
        let mut b = Build::<Local>::new();
        let first = constants(&mut b, 2);
        let slot = first[0].index;
        b.store.cold[slot as usize].generation = u32::MAX - 1;
        let old = b.token(slot);
        assert_eq!(collect_with(&mut b, &first[1..]), 1);
        assert_eq!(b.store.retired, 1);
        assert!(b.store.free.is_empty(), "a retired slot is not free");
        assert_eq!(b.store.cold[slot as usize].generation, u32::MAX);
        assert_eq!(b.lookup(old), Err(TokenFault::Stale));
        let again = constants(&mut b, 1);
        assert_eq!(indices(&again), [3], "a new slot, not the retired one");
        // Retired for good: the next free and reuse go elsewhere too.
        assert_eq!(collect_with(&mut b, &again), 1);
        assert_eq!(indices(&constants(&mut b, 1)), [2]);
    }
}
