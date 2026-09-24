//! Memoized pull (`ensure`), the value of a cell before the instant
//! (`value`), and its value after the instant, in two phases: `prepare`,
//! which may evaluate and computes read-through values, and `post`, which
//! reads.

use super::{IN_PROGRESS, Kind, LINKED, NOOP, cell, in_place, memo};
use crate::build::Build;
use crate::mode::Mode;

/// What a read of a cell's value has passed on its way down, for Brent's
/// cycle detection over the switch_cells whose selection it follows before
/// any cycle check has seen that selection. It is an argument, on the call
/// stack, since a read through `&Build` can mark nothing in the arena.
///
/// A read follows a switch_cell to the inner its outer selects, not to its
/// link, and a switch_cell built in this instant has no link until its
/// first evaluation, which is what checks that the inner does not depend
/// on it. Before that, a read can go around a cycle the check would refuse:
/// a loop closed with the switch, which its outer selects (F49). Every
/// other step of a read follows a dependency, and the dependency graph is
/// acyclic, so a read that goes around a cycle passes such a switch on
/// every round. A read's steps depend only on the graph, and a read-through
/// cell on the way is still filling its memo, so a read that meets a
/// switch it passed before, further up its own path, goes around for ever.
/// Brent's algorithm finds the meeting within a few rounds of the cycle.
#[derive(Clone, Copy)]
pub(crate) struct Passed {
    /// The switch the read saved, to meet again on a cycle.
    saved: u32,
    /// Switches passed since it saved one, and how many to pass before it
    /// saves the next.
    since: u32,
    power: u32,
}

impl Passed {
    /// A read that has passed nothing.
    pub(crate) const NOTHING: Passed = Passed {
        saved: NOOP,
        since: 0,
        power: 1,
    };

    /// The read passes switch_cell `i`, or, `None`, meets the switch it
    /// saved: it has gone around a cycle.
    fn pass(self, i: u32) -> Option<Passed> {
        if i == self.saved {
            return None;
        }
        let since = self.since + 1;
        Some(if since == self.power {
            Passed {
                saved: i,
                since: 0,
                power: self.power.saturating_mul(2),
            }
        } else {
            Passed { since, ..self }
        })
    }
}

impl<M: Mode> Build<M> {
    /// Makes sure `x` has run at this instant, running its dependencies
    /// first. The fallback for the two dynamic cases, a read of a node the
    /// order has not reached yet (a switch_cell's new inner, read after the
    /// instant) and nodes created during this transaction; the evaluation
    /// loop never calls it. A node in the order
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
    /// sees this. A cell loop's forward reads through to its definition,
    /// and has no value before it is closed. A switch_cell's is `at (at c
    /// t) t`: two chases through its outer and no memo; it never reads its
    /// own link, which exists for marking.
    pub(crate) fn value<A: 'static>(&self, i: u32) -> &A {
        self.value_through::<A>(i, Passed::NOTHING)
    }

    /// `value`, as a step of a read that has passed what `passed` says. A
    /// read that goes around a cycle through a switch_cell's selection
    /// before the switch's first link panics, which poisons the graph
    /// inside a transaction, where it would recurse until the stack
    /// overflowed (`Passed`).
    pub(crate) fn value_through<A: 'static>(&self, i: u32, passed: Passed) -> &A {
        let data = &self.store.data[i as usize];
        match self.store.hot[i as usize].kind {
            Kind::Hold | Kind::Constant => &cell::<M, A>(data).value,
            Kind::InPlace => in_place::<M, A>(data),
            Kind::ReadThrough => (self.store.ops[i as usize].value)(self, i, passed)
                .downcast_ref::<A>()
                .expect("bough engine: cell type"),
            Kind::Loop => self.value_through::<A>(self.loop_target(i), passed),
            Kind::SwitchCell => {
                let outer = self.store.relations[i as usize].deps[0];
                let inner = (self.store.ops[i as usize].inner)(self, outer, false, passed);
                let inner = self.check(inner);
                let passed = if self.store.hot[i as usize].flags & LINKED == 0 {
                    passed.pass(i).unwrap_or_else(|| {
                        panic!(
                            "bough: a same-instant cycle through a switch_cell read before its \
                             first link, at node {i}. The cell a switch selects may not depend \
                             on the switch at the same instant"
                        )
                    })
                } else {
                    passed
                };
                self.value_through::<A>(inner, passed)
            }
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
                // A loop steps iff its definition did, and its value after
                // the instant is its definition's.
                Kind::Loop => {
                    let target = self.loop_target(x);
                    self.prepare(target);
                }
                // The value after the instant is the value after it of the
                // inner the outer holds after it: at a switch instant the
                // new inner, which is not a dependency and may not have run
                // yet. Preparing it runs it by memoized pull: the one read
                // of the future.
                Kind::SwitchCell => {
                    let outer = self.store.relations[x as usize].deps[0];
                    self.prepare(outer);
                    let inner = self.selected_after(x);
                    self.prepare(inner);
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
            // Whether or not the loop stepped: its definition's value after
            // the instant is the definition's own value when it did not.
            Kind::Loop => self.post::<A>(self.loop_target(x)),
            // `normalize` and `chopBack` in the semantics: at a switch
            // instant only the new inner counts. When the switch did not
            // step, neither did its outer, whose value after the instant is
            // then its value, the current inner, which did not step either.
            Kind::SwitchCell => self.post::<A>(self.selected_after(x)),
            k => panic!("bough engine: node {x} ({k:?}) is not a cell"),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::build::Build;
    use crate::mode::Local;

    /// The re-entry stamp, alone. Graph code closes a cycle through reads
    /// after the instant only through a switch_cell (R10, in the switch
    /// tests), where pull runs too. Here the cycle is linked by hand, in an
    /// instant where neither cell is marked or new, so that no pull runs
    /// and only `prepare` recurses.
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
