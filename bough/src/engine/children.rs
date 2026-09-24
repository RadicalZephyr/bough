//! Child transactions (the semantics' `Split`, with the oracle's patch F7):
//! the instants `t ++ [n]` of an instant t, run after t's listeners and
//! before `send` returns, depth first, by a loop with an explicit stack
//! rather than by recursion.
//!
//! A split or defer capture that fires at depth d pushes one entry on its
//! own stack, the iterator over the event's elements or the deferred event,
//! and registers in `levels[d]`. Child n of an instant at depth d is an
//! instant at depth d + 1 whose started nodes are the outputs of the
//! captures in `levels[d]`, each with its element n. So two splits that
//! fire in one instant share child indices, and their elements at one index
//! are simultaneous; a defer is a split of one element, so its event is
//! simultaneous with element 0 of the others (finding F13). A capture that
//! fires again inside a child, through a loop, pushes a second entry. The
//! scheduler always runs the innermost level, whose captures' entries are
//! on top, so the grandchildren `t ++ [n] ++ [m]` run before `t ++ [n + 1]`:
//! time order, which the text's `Split` does not give there (F7). When no
//! capture of a level has an element left, each pops its entry and the
//! level ends.
//!
//! The levels in progress are always 0 to the innermost, so its depth is
//! all the stack the loop needs. Each level keeps its capacity, so a graph
//! that is not growing does not allocate here; a level deeper than any
//! before is allocated once. A long chain of child transactions grows these
//! buffers, never the Rust stack.

use alloc::boxed::Box;
use alloc::vec::Vec;

use super::{Data, Kind, Ops};
use crate::build::Build;
use crate::mode::Mode;

impl<M: Mode> Build<M> {
    /// Creates a capture and its output, whose slot `slot` is and whose
    /// functions `output_ops` are. The capture depends on its chain's
    /// dependency and reads the cells the chain reads. The output has no
    /// dependency; it keeps its capture in reach, so collection keeps the
    /// capture while the output is live. Returns the output, the node the
    /// caller's stream token names.
    pub(crate) fn capture_pair(
        &mut self,
        dependency: u32,
        cells: Vec<u32>,
        parts: Box<[M::Carrier]>,
        ops: &'static Ops<M>,
        slot: M::Carrier,
        output_ops: &'static Ops<M>,
    ) -> u32 {
        let capture = self.materialize(
            Kind::SplitCapture,
            Data::Empty,
            parts,
            ops,
            &[dependency],
            0,
        );
        self.set_reach(capture, cells);
        let output = self.materialize(
            Kind::SplitOutput,
            Data::Slot(slot),
            Box::new([]),
            output_ops,
            &[],
            0,
        );
        self.store.cold[capture as usize].partner = output;
        let cold = &mut self.store.cold[output as usize];
        cold.partner = capture;
        cold.reach.push(capture);
        output
    }

    /// Registers a capture that fired in this instant, after it pushed its
    /// entry: its children are this instant's.
    pub(crate) fn capture_fired(&mut self, capture: u32) {
        let depth = self.s.depth;
        if self.s.levels.len() <= depth {
            self.s.levels.resize_with(depth + 1, Vec::new);
        }
        self.s.levels[depth].push(capture);
    }

    /// Starts a capture's output with one element in this child instant.
    /// A capture is in a level once and emits once per child instant, so
    /// its output is never started twice in one.
    pub(crate) fn fire_child<A: 'static>(&mut self, capture: u32, item: A) {
        let output = self.store.cold[capture as usize].partner;
        self.fire_start(output, item)
            .expect("bough engine: a capture's output starts once per child instant");
    }

    /// Runs the children of the instant that just ran, depth first, each a
    /// whole instant: mark, evaluate, new nodes, commit, dispatch. `parent`
    /// is the level whose captures emit; the child instant runs one deeper.
    pub(crate) fn children(&mut self) {
        let mut parent = 0;
        loop {
            // A fresh serial before the emits: an element is an event of
            // the child instant. When no capture has one left, the serial
            // goes unused.
            self.begin_instant();
            let depth = parent + 1;
            self.s.depth = depth;
            if self.s.levels.len() <= depth {
                self.s.levels.resize_with(depth + 1, Vec::new);
            }
            debug_assert!(
                self.s.levels[depth].is_empty(),
                "bough engine: a level starts empty"
            );
            let mut emitted = false;
            let mut k = 0;
            while k < self.s.levels[parent].len() {
                let capture = self.s.levels[parent][k];
                emitted |= self.emit_child(capture);
                k += 1;
            }
            if emitted {
                self.instant();
                if !self.s.levels[depth].is_empty() {
                    // Depth first: this child's children come next.
                    parent = depth;
                }
            } else {
                self.end_level(parent);
                if parent == 0 {
                    break;
                }
                parent -= 1;
            }
        }
        self.s.depth = 0;
    }

    /// One capture's next element, if it has one, starts its output. The
    /// capture's program leaves the arena while it runs, as in evaluation.
    fn emit_child(&mut self, capture: u32) -> bool {
        let n = capture as usize;
        let ops = self.store.ops[n];
        let mut parts = self.store.parts[n]
            .take()
            .expect("bough engine: a node's program is in place");
        let emitted = (ops.emit_child)(&mut parts, self, capture);
        self.store.parts[n] = Some(parts);
        emitted
    }

    /// Every capture of a finished level pops the entry it pushed there.
    fn end_level(&mut self, level: usize) {
        let Build { store, s, .. } = self;
        for &capture in &s.levels[level] {
            let n = capture as usize;
            let parts = store.parts[n]
                .as_deref_mut()
                .expect("bough engine: a node's program is in place");
            (store.ops[n].end_children)(parts);
        }
        s.levels[level].clear();
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use crate::build::Build;
    use crate::engine::part;
    use crate::mode::Local;
    use crate::source::Source;

    /// The entries a capture has not popped.
    fn entries<T: 'static>(b: &mut Build<Local>, output: u32) -> usize {
        let capture = b.store.cold[output as usize].partner as usize;
        let parts = b.store.parts[capture].as_mut().expect("parts in place");
        part::<Local, Vec<Option<T>>>(&mut parts[1]).len()
    }

    /// Every level a transaction used, and every capture's stack, is empty
    /// again when it ends, with nested splits and a split that fires inside
    /// its own children: nothing from one transaction reaches the next, and
    /// the buffers keep only their capacity.
    #[test]
    fn levels_and_capture_stacks_are_empty_between_transactions() {
        type Rows = vec::IntoIter<Vec<u32>>;
        type Items = vec::IntoIter<u32>;
        let mut b = Build::<Local>::new();
        b.begin();
        b.push_scope();
        let (lists, lists_in) = b.input::<Vec<Vec<u32>>>();
        let rows = lists.split(&mut b);
        let rows_out = rows.token.index;
        let rows = rows.share(&mut b);
        let items = rows.split(&mut b).token.index;
        let (fwd, fwd_loop) = b.stream_loop::<Vec<u32>>();
        let looped = fwd.split(&mut b);
        let looped_out = looped.token.index;
        let again = looped
            .share(&mut b)
            .filter(|n| *n < 10)
            .map(|n| vec![n * 10]);
        let definition = rows.or_else(&mut b, again);
        fwd_loop.close(&mut b, definition);
        b.pop_scope();
        b.finish();
        for _ in 0..2 {
            b.begin();
            b.fire_start(lists_in.token.index, vec![vec![1u32, 2], vec![], vec![3]])
                .expect("one send");
            b.finish();
            assert!(!b.in_tx);
            assert!(b.s.levels.len() >= 4, "the loop reached depth 3");
            assert!(b.s.levels.iter().all(Vec::is_empty));
            assert_eq!(b.s.depth, 0);
            assert_eq!(entries::<Rows>(&mut b, rows_out), 0);
            assert_eq!(entries::<Items>(&mut b, items), 0);
            assert_eq!(entries::<Items>(&mut b, looped_out), 0);
        }
    }
}
