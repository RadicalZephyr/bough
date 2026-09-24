//! Memoized pull (`ensure`), the value of a cell before the instant
//! (`value`), and its value after the instant, in two phases: `prepare`,
//! which may evaluate and computes read-through values, and `post`, which
//! reads.

use super::{IN_PROGRESS, Kind, NOOP, cell, in_place, memo};
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

    /// Makes sure everything the value of `x` after the instant reads has
    /// run at this instant, and computes the values after the instant of
    /// the read-through cells among it. The mutable phase of a read after
    /// the instant; `post` is the shared phase.
    ///
    /// Only a cell that stepped needs work: the value after the instant of
    /// a cell that did not step is its value before it. The kind decides
    /// at run time what the work is, so no flag fixed at materialization
    /// can go stale. Two stamps make a diamond prepare each cell once and
    /// turn a cycle through a read after the instant into a panic, where
    /// plain recursion would overflow the stack.
    pub(crate) fn prepare(&mut self, x: u32) {
        let tx = self.tx;
        let c = &self.store.cold[x as usize];
        if c.prep_done == tx {
            return;
        }
        assert!(
            c.prep_enter != tx,
            "bough: a same-instant cycle through a read after the instant at node {x}"
        );
        self.store.cold[x as usize].prep_enter = tx;
        self.ensure(x);
        if self.store.hot[x as usize].fired == tx {
            match self.store.hot[x as usize].kind {
                Kind::ReadThrough => {
                    let mut k = 0;
                    while k < self.store.relations[x as usize].deps.len() {
                        let d = self.store.relations[x as usize].deps[k];
                        self.prepare(d);
                        k += 1;
                    }
                    let ops = self.store.ops[x as usize];
                    (ops.compute_post)(self, x);
                }
                Kind::Loop => todo!("stage 3: a loop's value after the instant is its target's"),
                Kind::SwitchCell => {
                    todo!("stage 5: prepare the outer and the inner it selects after the instant")
                }
                // A stateful cell's value after the instant is the pending
                // value its evaluation wrote. An in-place accumulator's is
                // never asked for: a State has no stream view.
                _ => {}
            }
        }
        self.store.cold[x as usize].prep_done = tx;
    }

    /// The value after the instant: for a cell that stepped, the value its
    /// step carries, and for any other cell, its value before the instant.
    /// The caller has prepared `x`.
    pub(crate) fn post<A: 'static>(&self, x: u32) -> &A {
        let h = &self.store.hot[x as usize];
        let stepped = h.fired == self.tx;
        let data = &self.store.data[x as usize];
        match h.kind {
            // `Hold a s t0`: the event of `s` at `t`, if there is one.
            Kind::Hold if stepped => cell::<M, A>(data)
                .pending
                .as_ref()
                .expect("bough engine: a hold that stepped has its pending value"),
            Kind::Hold | Kind::Constant => &cell::<M, A>(data).value,
            Kind::ReadThrough if stepped => memo::<M, A>(data)
                .post_value
                .as_ref()
                .expect("bough engine: a read-through cell read after the instant before prepare"),
            Kind::ReadThrough => self.value::<A>(x),
            Kind::InPlace => unreachable!(
                "bough engine: node {x} is an in-place accumulator, whose value after the \
                 instant does not exist before commit; a State has no stream view"
            ),
            Kind::Loop => todo!("stage 3: a loop's value after the instant is its target's"),
            Kind::SwitchCell => todo!("stage 5: the value after the instant of the selected inner"),
            k => panic!("bough engine: node {x} ({k:?}) is not a cell"),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::build::Build;
    use crate::mode::Local;

    /// The re-entry stamp. A cycle through reads after the instant cannot
    /// be built in stage 2, since every node is created after its
    /// dependencies; stage 5's switches can close one. So the cycle is
    /// linked by hand, in an instant where neither cell is marked or new,
    /// so that no pull runs and only `prepare` recurses.
    #[test]
    #[should_panic(expected = "a same-instant cycle through a read after the instant")]
    fn preparing_a_cell_again_while_it_is_being_prepared_panics() {
        let mut b = Build::<Local>::new();
        b.begin();
        let (x, _x_in) = b.input_cell(1u32);
        let m1 = x.map_cell(&mut b, |n| n + 1);
        let m2 = m1.map_cell(&mut b, |n| n * 2);
        b.finish();
        b.begin();
        let (m1, m2) = (m1.token.index, m2.token.index);
        b.link(m2, m1);
        b.set_fired(m1);
        b.set_fired(m2);
        b.prepare(m2);
    }
}
