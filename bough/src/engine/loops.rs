//! Loops (RFD 2): the scopes that must close every loop they declare, and
//! the check that closing a loop leaves the dependency graph acyclic.
//!
//! A loop's forward token names a node of its own, created open. A cell
//! loop's is a `Loop` node with no dependency; a stream loop's is a stream
//! node whose program panics if run. Closing adds the one dependency that
//! makes the loop: a cell loop's node depends on its definition and from
//! then on settles like a read-through cell and reads through to it, and a
//! stream loop's node takes the definition's chain as its program and
//! depends on the chain's dependency.
//!
//! The rule is acyclicity of the dependency graph (finding F3, which
//! replaces RFD 2's "every path passes through a hold, an accumulator, a
//! split or a defer"). Only dependencies count. A read of a cell's value
//! from before the instant (`snapshot`, `gate`, `sample`) is not one, nor
//! is a switch_stream's selection, nor `depends`; and a split's output does
//! not depend on its capture, since the output fires in a later child
//! instant. So a loop through any of those is legal. A hold does not delay
//! its steps view, so a loop through a hold's `steps` is a same-instant
//! cycle, which no order can evaluate and the semantics do not define.
//!
//! Before the dependency is added, a walk downstream from the forward over
//! dependents looks for the node the loop will depend on: the forward's
//! dependents are what was built with it, usually few, and a forward read
//! only by snapshots has none. A path is a cycle, and the panic names its
//! nodes. The check comes first, so a refused close leaves the graph as it
//! was.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;

use super::{Data, Kind, Ops};
use crate::build::Build;
use crate::mode::Mode;
use crate::token::Token;

/// The relation a path check walks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Walk {
    /// From a node to the nodes that depend on it: at close, downstream
    /// from the forward.
    Dependents,
    /// From a node to the nodes it depends on: at a switch's first link
    /// and at relink, upstream from the new inner (stage 5).
    Dependencies,
}

/// The nodes of a cycle, each with its kind, printed as a closed path.
struct Cycle(Vec<(u32, Kind)>);

impl fmt::Display for Cycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (n, kind) in &self.0 {
            write!(f, "node {n} ({kind:?}) -> ")?;
        }
        match self.0.first() {
            Some((n, _)) => write!(f, "node {n}"),
            None => Ok(()),
        }
    }
}

impl<M: Mode> Build<M> {
    /// Opens a scope: the build closure, or one run of a construct closure.
    pub(crate) fn push_scope(&mut self) {
        self.s.scopes.push(self.s.open_loops.len());
    }

    /// Closes a scope. A loop declared in it and still open is a panic,
    /// which poisons the graph when it happens inside a transaction; at the
    /// end of the build closure no graph exists yet, so `Graph::build`
    /// panics.
    pub(crate) fn pop_scope(&mut self) {
        let start = self.s.scopes.pop().expect("bough engine: a scope is open");
        assert!(
            self.s.open_loops.len() == start,
            "bough: a loop declared in this scope was never closed"
        );
    }

    /// Records a new forward's node as open in the current scope.
    pub(crate) fn open_loop(&mut self, forward: u32) {
        self.s.open_loops.push(forward);
    }

    /// A cell loop's forward: a `Loop` node with no dependency and no data,
    /// open in the current scope. Created now, so it runs in this
    /// transaction's new-node phase, by pull after its definition.
    pub(crate) fn loop_node(&mut self) -> Token {
        let n = self.materialize(
            Kind::Loop,
            Data::Empty,
            Box::new([]),
            &Ops::<M>::DEFAULT,
            &[],
            0,
        );
        self.open_loop(n);
        self.token(n)
    }

    /// Closes a cell loop: its node depends on `definition` from now on.
    pub(crate) fn close_cell_loop(&mut self, forward: Token, definition: Token) {
        let l = self.check(forward);
        let d = self.check(definition);
        self.close_in_scope(l);
        self.refuse_cycle(l, d);
        self.link(d, l);
    }

    /// Takes a forward's node off the current scope's open loops. A loop
    /// declared in another scope, or already closed, is not there.
    pub(crate) fn close_in_scope(&mut self, forward: u32) {
        let start = *self.s.scopes.last().expect("bough engine: a scope is open");
        // The current scope is the innermost, so its loops run to the end
        // of the list and a swap_remove keeps every scope's offset.
        let at = self.s.open_loops[start..]
            .iter()
            .position(|&l| l == forward)
            .expect("bough: a loop must close in the scope that declared it");
        self.s.open_loops.swap_remove(start + at);
    }

    /// Closing makes `forward` depend on `dependency`. Panics, naming the
    /// cycle's nodes, if `forward` already reaches `dependency` over
    /// dependents, which is exactly when the new dependency closes a cycle.
    pub(crate) fn refuse_cycle(&mut self, forward: u32, dependency: u32) {
        if let Some(path) = self.path(forward, dependency, Walk::Dependents) {
            let cycle = Cycle(
                path.iter()
                    .map(|&n| (n, self.store.hot[n as usize].kind))
                    .collect(),
            );
            panic!(
                "bough: closing this loop makes a same-instant cycle: {cycle}. A definition \
                 may reach its own forward token only through a read of a cell's value \
                 (snapshot, gate, sample), a switch_stream's selection, or a split or defer"
            );
        }
    }

    /// Whether `from` reaches `target` over `walk`, and the path from one to
    /// the other if so. An iterative depth-first search with the reused
    /// stack and a visit epoch, so it allocates only to return a path. A
    /// cell read, a watcher and a split's capture-to-output pair are in
    /// neither relation, so a cycle through them is never found.
    pub(crate) fn path(&mut self, from: u32, target: u32, walk: Walk) -> Option<Vec<u32>> {
        self.s.visit_epoch += 1;
        let epoch = self.s.visit_epoch;
        let Build { store, s, .. } = self;
        s.search.clear();
        s.search.push((from, 0));
        store.cold[from as usize].visit = epoch;
        while let Some(top) = s.search.last_mut() {
            let (n, k) = *top;
            if n == target {
                let path = s.search.iter().map(|&(n, _)| n).collect();
                s.search.clear();
                return Some(path);
            }
            let next = match walk {
                Walk::Dependents => &store.relations[n as usize].dependents,
                Walk::Dependencies => &store.relations[n as usize].deps,
            };
            if (k as usize) < next.len() {
                top.1 += 1;
                let d = next[k as usize];
                let cold = &mut store.cold[d as usize];
                if cold.visit != epoch {
                    cold.visit = epoch;
                    s.search.push((d, 0));
                }
            } else {
                s.search.pop();
            }
        }
        None
    }

    /// A cell loop's node: the one node it reads through to, its
    /// definition. Before close it has none, and nothing can be read.
    pub(crate) fn loop_target(&self, forward: u32) -> u32 {
        *self.store.relations[forward as usize]
            .deps
            .first()
            .expect("bough: a cell loop sampled before it is closed")
    }
}

#[cfg(test)]
mod tests {
    use super::Walk;
    use crate::build::Build;
    use crate::mode::Local;
    use crate::source::Source;

    /// A construct closure runs in a scope of its own (stage 6). A loop
    /// declared in the enclosing scope cannot be closed from it.
    #[test]
    #[should_panic(expected = "a loop must close in the scope that declared it")]
    fn a_loop_closes_only_in_the_scope_that_declared_it() {
        let mut b = Build::<Local>::new();
        b.begin();
        b.push_scope();
        let (_count, closer) = b.cell_loop::<u32>();
        let one = b.constant(1u32);
        b.push_scope();
        closer.close(&mut b, one);
    }

    /// Scopes nest: an inner scope checks only the loops it declared, and
    /// the outer scope's open loop may close after the inner scope ends.
    #[test]
    fn an_inner_scope_checks_only_its_own_loops() {
        let mut b = Build::<Local>::new();
        b.begin();
        b.push_scope();
        let (_outer, outer_loop) = b.cell_loop::<u32>();
        b.push_scope();
        let (_inner, inner_loop) = b.cell_loop::<u32>();
        let (_second, second_loop) = b.cell_loop::<u32>();
        let one = b.constant(1u32);
        // Closing out of declaration order: the swap_remove stays inside
        // the inner scope's part of the list.
        inner_loop.close(&mut b, one);
        second_loop.close(&mut b, one);
        b.pop_scope();
        outer_loop.close(&mut b, one);
        b.pop_scope();
        assert!(b.s.open_loops.is_empty() && b.s.scopes.is_empty());
    }

    #[test]
    #[should_panic(expected = "a loop declared in this scope was never closed")]
    fn an_inner_scope_with_an_open_loop_panics_when_it_ends() {
        let mut b = Build::<Local>::new();
        b.begin();
        b.push_scope();
        b.push_scope();
        let (_inner, _inner_loop) = b.cell_loop::<u32>();
        b.pop_scope();
    }

    /// The path check in both directions, and its reused buffers: the
    /// second search in a row finds nothing stale from the first.
    #[test]
    fn a_path_is_found_over_either_relation_and_reads_no_stale_visits() {
        let mut b = Build::<Local>::new();
        b.begin();
        let (numbers, _numbers_in) = b.input::<u32>();
        let input = numbers.token.index;
        let held = numbers.map(|n| n + 1).hold(&mut b, 0u32);
        let doubled = held.map_cell(&mut b, |n| n * 2);
        let (h, d) = (held.token.index, doubled.token.index);
        assert_eq!(
            b.path(input, d, Walk::Dependents),
            Some(alloc::vec![input, h, d])
        );
        assert_eq!(
            b.path(d, input, Walk::Dependencies),
            Some(alloc::vec![d, h, input])
        );
        assert_eq!(b.path(d, input, Walk::Dependents), None);
        assert_eq!(
            b.path(input, input, Walk::Dependents),
            Some(alloc::vec![input])
        );
        assert!(b.s.search.is_empty());
    }
}
