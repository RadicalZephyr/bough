//! Memoized pull (`ensure`) and the value of a cell before the instant
//! (`value`). Stage 2 adds the post-instant read, `prepare` and `post`.

use super::{IN_PROGRESS, Kind, NOOP, cell, in_place};
use crate::build::Build;
use crate::mode::Mode;

impl<M: Mode> Build<M> {
    /// Makes sure `x` has run at this instant, running its dependencies
    /// first. The fallback for the two dynamic cases, a read of a node the
    /// order has not reached yet (stage 5) and nodes created during this
    /// transaction; the evaluation loop never calls it. A node in the order
    /// is found by marking's `pos` and the loop's cursor, and a pulled entry
    /// becomes node 0 so the loop skips it without a check. A node created
    /// during this transaction carries two stamps of its own.
    pub(crate) fn ensure(&mut self, x: u32) {
        let tx = self.tx;
        let h = &self.store.hot[x as usize];
        if h.mark == tx {
            let p = h.pos;
            // START is above every cursor: a started node ran at its send.
            if self.s.order_done || p > self.s.cursor {
                return;
            }
            assert!(
                p != self.s.cursor,
                "bough: a same-instant cycle through a dynamic read at node {x}"
            );
            match self.s.order[p as usize] {
                NOOP => return,
                IN_PROGRESS => {
                    panic!("bough: a same-instant cycle through a dynamic read at node {x}")
                }
                _ => {}
            }
            count!(self.s, pulls);
            self.s.order[p as usize] = IN_PROGRESS;
            self.ensure_deps(x);
            self.eval_node(x);
            self.s.order[p as usize] = NOOP;
        } else if h.created == tx {
            let c = &self.store.cold[x as usize];
            if c.done == tx {
                return;
            }
            assert!(
                c.pulling != tx,
                "bough: a same-instant cycle among nodes created in this transaction at node {x}"
            );
            count!(self.s, new_nodes);
            self.store.cold[x as usize].pulling = tx;
            self.ensure_deps(x);
            self.eval_node(x);
            self.store.cold[x as usize].done = tx;
        }
        // Otherwise the node is not affected at this instant: its slot is
        // stale by stamp.
    }

    fn ensure_deps(&mut self, x: u32) {
        let mut k = 0;
        while k < self.store.relations[x as usize].deps.len() {
            let d = self.store.relations[x as usize].deps[k];
            self.ensure(d);
            k += 1;
        }
    }

    /// The value before the instant, the semantics' `at c t`. Committed
    /// values change only at commit, so every read during a transaction
    /// sees this. Stages 3 and 5 add the loop and switch arms.
    pub(crate) fn value<A: 'static>(&self, i: u32) -> &A {
        let data = &self.store.data[i as usize];
        match self.store.hot[i as usize].kind {
            Kind::Hold | Kind::Constant => &cell::<M, A>(data).value,
            Kind::InPlace => in_place::<M, A>(data),
            Kind::ReadThrough => (self.store.ops[i as usize].value)(self, i)
                .downcast_ref::<A>()
                .expect("bough engine: cell type"),
            k => panic!("bough engine: node {i} ({k:?}) is not a cell"),
        }
    }
}
