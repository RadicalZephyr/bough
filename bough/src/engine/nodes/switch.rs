//! Switches: nodes whose dependency on an inner cell or stream changes at
//! run time, the semantics' `SwitchC` (with the oracle's patch F6) and
//! `SwitchS`.
//!
//! A switch reads the token its outer cell holds, before or after the
//! instant, through its node type's `inner` function. `value::<A>` of a
//! switch_cell reads its outer as a `Cell<A>` through that function pointer
//! rather than by calling `value::<Cell<A>>` itself, which would never
//! finish monomorphizing.
//!
//! A switch links its current inner at its first evaluation, which is at
//! its creation instant: the inner its outer selected before that instant.
//! So creating a switch reads nothing, and a switch over a loop cell that
//! is not closed yet works (R8). When the outer holds another inner after
//! an instant, commit moves the switch there, after holds commit and memos
//! settle, so it reads the committed value. The first link and every move
//! check that the dependency graph stays acyclic, walking upstream from the
//! new inner (`refuse_switch_cycle`).
//!
//! A switch_cell depends on its outer, since at a switch instant its value
//! after the instant is the new inner's, and on its current inner, so that
//! it steps when that inner steps. It has no data and no program: the
//! evaluation loop settles it, and every read chases its outer.
//!
//! A switch_stream depends on its current inner alone: at instant t it
//! forwards the events of the inner its outer selected before t, so a
//! selection at t takes effect after t, and its outer is not a dependency
//! (F14). A loop through its selection is therefore legal. Its outer keeps
//! it in reach and in a watcher list instead: marking that reaches the
//! outer queues the switch for relink and does not descend into it, so a
//! selection moves the switch even at an instant its old inner is quiet,
//! when nothing else would reach it.
//!
//! A linear stream has one consumer (RFD 4), so a cell holding linear
//! streams may have one switch_stream. A second on the same cell is refused
//! when it is built, and so is one on a cell that is the same cell by
//! construction, a cell loop's forward and its definition, then or when the
//! loop closes. A switch_cell can still select, at run time, a cell whose
//! streams another switch_stream takes from, so each linear stream records
//! the switch_stream consuming it, set when a switch links it and cleared
//! when the switch moves away, and a link to a stream another switch
//! consumes is a panic that poisons the graph. Relink clears every claim
//! the instant's moves give up before it makes any, so two switches that
//! trade streams in one instant are accepted in either order.

use alloc::boxed::Box;
use alloc::vec::Vec;

use super::Marker;
use crate::build::Build;
use crate::cell::CellRef;
use crate::engine::{Cx, Data, Kind, LINKED, NodeOps, Ops, TAKES_LINEAR, WATCHED};
use crate::mode::Mode;
use crate::source::Node;
use crate::token::Token;

/// `switch_cell` over an outer holding `C` tokens: a `Cell` or a `State`.
pub(crate) struct SwitchCellNode<C>(Marker<C>);

/// The token of the cell the outer holds before the instant, or after it.
/// The caller has prepared the outer before asking for the value after.
fn inner_cell<M: Mode, C: CellRef>(b: &Build<M>, outer: u32, post: bool) -> Token {
    let inner = if post {
        b.post::<C>(outer)
    } else {
        b.value::<C>(outer)
    };
    inner.token()
}

impl<M: Mode, C: CellRef> NodeOps<M> for SwitchCellNode<C> {
    const OPS: Ops<M> = Ops {
        inner: inner_cell::<M, C>,
        ..Ops::<M>::DEFAULT
    };
}

/// `switch_stream` over an outer holding `S` tokens: a linear `Stream`,
/// whose event it takes, or a `Shared` one, whose event it clones.
pub(crate) struct SwitchStreamNode<S>(Marker<S>);

/// The program of a switch_stream. Its first evaluation, at its creation
/// instant, links the inner its outer selected before the instant, runs it
/// at this instant, since it was no dependency when this node's
/// dependencies were made sure of, and queues a relink, since the outer
/// may step at this instant too, which nothing else would notice. Then, at
/// every instant, it forwards its current inner's event.
fn eval_switch_stream<M: Mode, S: Node>(_: &mut [M::Carrier], b: &mut Build<M>, me: u32)
where
    S::Event: 'static,
{
    if b.store.hot[me as usize].flags & LINKED == 0 {
        b.link_inner(me);
        let inner = b.store.relations[me as usize].deps[0];
        b.ensure(inner);
        b.queue_relink(me);
    }
    let inner = b.store.relations[me as usize].deps[0];
    if let Some(v) = S::pull_inner(&mut Cx { b: &mut *b }, inner) {
        b.put_event(me, v);
    }
}

/// The token of the stream the outer holds before the instant, or after
/// it.
fn inner_stream<M: Mode, S: Node>(b: &Build<M>, outer: u32, post: bool) -> Token {
    let inner = if post {
        b.post::<S>(outer)
    } else {
        b.value::<S>(outer)
    };
    inner.node_token()
}

impl<M: Mode, S: Node> NodeOps<M> for SwitchStreamNode<S>
where
    S::Event: 'static,
{
    const OPS: Ops<M> = Ops {
        eval: eval_switch_stream::<M, S>,
        inner: inner_stream::<M, S>,
        ..Ops::<M>::DEFAULT
    };
}

/// Where a switch keeps its current inner among its dependencies: a
/// switch_cell's first dependency is its outer, and a switch_stream's
/// inner is its only one.
fn inner_at(kind: Kind) -> usize {
    match kind {
        Kind::SwitchCell => 1,
        Kind::SwitchStream => 0,
        k => unreachable!("bough engine: a {k:?} node is not a switch"),
    }
}

impl<M: Mode> Build<M> {
    /// A switch_cell over `outer`, a cell holding `C` tokens. Its one
    /// dependency until its first evaluation is the outer.
    pub(crate) fn switch_cell_node<C: CellRef>(&mut self, outer: Token) -> Token {
        let outer = self.check(outer);
        let ops = &<SwitchCellNode<C> as NodeOps<M>>::OPS;
        let n = self.materialize(
            Kind::SwitchCell,
            Data::Empty,
            Box::new([]),
            ops,
            &[outer],
            0,
        );
        self.token(n)
    }

    /// A switch_stream over `outer`, a cell holding `S` tokens, with the
    /// slot its `Accepts` bound made. It has no dependency until its first
    /// evaluation. The outer keeps it in reach and watches for it. Over
    /// linear streams, refused if the outer, or a cell that is the same by
    /// construction, has a switch_stream already.
    pub(crate) fn switch_stream_node<S: Node>(&mut self, outer: Token, slot: M::Carrier) -> Token
    where
        S::Event: 'static,
    {
        let outer = self.check(outer);
        if S::LINEAR {
            assert!(
                !self.has_linear_switch(outer),
                "bough: a cell holding linear streams may have exactly one switch_stream, and \
                 this cell, or a cell loop that is the same cell, has one already. A linear \
                 stream has one consumer; share the streams to switch to them from several places"
            );
        }
        let ops = &<SwitchStreamNode<S> as NodeOps<M>>::OPS;
        let flags = if S::LINEAR { TAKES_LINEAR } else { 0 };
        let n = self.materialize(
            Kind::SwitchStream,
            Data::Slot(slot),
            Box::new([]),
            ops,
            &[],
            flags,
        );
        let cold = &mut self.store.cold[n as usize];
        cold.partner = outer;
        cold.reach.push(outer);
        self.store.cold[outer as usize].watchers.push(n);
        self.store.hot[outer as usize].flags |= WATCHED;
        self.token(n)
    }

    /// The cell a switch reads its inner from: a switch_cell's first
    /// dependency, a switch_stream's partner.
    fn outer_of(&self, n: u32) -> u32 {
        match self.store.hot[n as usize].kind {
            Kind::SwitchCell => self.store.relations[n as usize].deps[0],
            Kind::SwitchStream => self.store.cold[n as usize].partner,
            k => unreachable!("bough engine: node {n} ({k:?}) is not a switch"),
        }
    }

    /// The node a switch's outer selects, before the instant or after it.
    fn selected(&self, n: u32, post: bool) -> u32 {
        let outer = self.outer_of(n);
        let inner = (self.store.ops[n as usize].inner)(self, outer, post);
        self.check(inner)
    }

    /// The inner a switch's outer selects after the instant, which may not
    /// have run yet: `prepare` of a switch_cell that stepped makes it run,
    /// by memoized pull, and returns it.
    pub(crate) fn selected_after(&self, n: u32) -> u32 {
        self.selected(n, true)
    }

    /// A switch's first evaluation links the inner its outer selected
    /// before the instant. Refused, with the cycle's nodes, if that inner
    /// depends on the switch, and over linear streams if another
    /// switch_stream consumes it.
    pub(crate) fn link_inner(&mut self, n: u32) {
        let inner = self.selected(n, false);
        self.refuse_switch_cycle(inner, n);
        self.claim_linear(inner, n);
        self.link(inner, n);
        self.store.hot[n as usize].flags |= LINKED;
    }

    /// A switch_stream over linear streams becomes the one consumer of
    /// `stream`. A stream another switch_stream consumes can only have been
    /// selected through a switch_cell, which no build-time check sees.
    fn claim_linear(&mut self, stream: u32, n: u32) {
        if self.store.hot[n as usize].flags & TAKES_LINEAR == 0 {
            return;
        }
        let consumer = &mut self.store.cold[stream as usize].linear_consumer;
        assert!(
            *consumer == 0 || *consumer == n,
            "bough: a linear stream selected by a second switch_stream: the switch_stream at \
             node {n} selected the stream at node {stream}, which the switch_stream at node \
             {other} takes from. A cell holding linear streams may have exactly one \
             switch_stream, and a switch_cell selected such a cell for another; share the \
             streams to switch to them from several places",
            other = *consumer
        );
        *consumer = n;
    }

    /// Whether a switch_stream over linear streams watches `cell`, or a
    /// cell that is the same by construction: a closed cell loop's forward
    /// and its definition, followed down to the definition that is no
    /// forward and back up through every forward closed with it.
    pub(crate) fn has_linear_switch(&self, cell: u32) -> bool {
        let mut root = cell;
        while self.store.hot[root as usize].kind == Kind::Loop {
            match self.store.relations[root as usize].deps.first() {
                Some(&target) => root = target,
                None => break,
            }
        }
        // A loop's one dependency is its definition, so the loops in a
        // node's dependents are exactly the forwards closed with it, and
        // closing refuses cycles: no node is reached twice.
        let mut same = Vec::from([root]);
        while let Some(c) = same.pop() {
            let linear = self.store.cold[c as usize]
                .watchers
                .iter()
                .any(|&w| self.store.hot[w as usize].flags & TAKES_LINEAR != 0);
            if linear {
                return true;
            }
            same.extend(
                self.store.relations[c as usize]
                    .dependents
                    .iter()
                    .copied()
                    .filter(|&d| self.store.hot[d as usize].kind == Kind::Loop),
            );
        }
        false
    }

    /// Queues a switch for relink at this instant's commit, once.
    pub(crate) fn queue_relink(&mut self, n: u32) {
        let tx = self.tx;
        let cold = &mut self.store.cold[n as usize];
        if cold.relink != tx {
            cold.relink = tx;
            self.s.relinks.push(n);
        }
    }

    /// The evaluation loop's arm for a switch_cell, and its pull: links it
    /// at its first evaluation, then settles it without user code. It steps
    /// at its creation instant, at every instant its outer steps, even when
    /// the new inner is quiet, and at every instant its current inner
    /// steps; an outer that stepped queues a relink.
    pub(crate) fn settle_switch_cell(&mut self, n: u32) {
        if self.store.hot[n as usize].flags & LINKED == 0 {
            self.link_inner(n);
        }
        let tx = self.tx;
        let deps = &self.store.relations[n as usize].deps;
        let (outer, inner) = (deps[0], deps[1]);
        let hot = &self.store.hot;
        let switched = hot[outer as usize].fired == tx;
        if switched || hot[inner as usize].fired == tx || hot[n as usize].created == tx {
            if switched {
                self.queue_relink(n);
            }
            self.set_fired(n);
        }
    }

    /// The first pass of relink at commit: moves a switch to the inner its
    /// outer holds now, if that is another node, giving up its claim on
    /// the old one. Returns whether it moved. A dependents list keeps its
    /// capacity, so moving back and forth between inners seen before
    /// allocates nothing.
    pub(crate) fn move_inner(&mut self, n: u32) -> bool {
        let new = self.selected(n, false);
        let at = inner_at(self.store.hot[n as usize].kind);
        let old = self.store.relations[n as usize].deps[at];
        if new == old {
            return false;
        }
        count!(self.s, relinks);
        let linear = &mut self.store.cold[old as usize].linear_consumer;
        if *linear == n {
            *linear = 0;
        }
        let dependents = &mut self.store.relations[old as usize].dependents;
        let p = dependents
            .iter()
            .position(|&d| d == n)
            .expect("bough engine: a switch is a dependent of its inner");
        dependents.remove(p);
        self.store.relations[new as usize].dependents.push(n);
        self.store.relations[n as usize].deps[at] = new;
        true
    }

    /// The second pass of relink at commit, once every switch of the
    /// instant has moved and given up its old stream: the dependency graph
    /// must still be acyclic, and a linear stream may have one consumer.
    pub(crate) fn check_moved(&mut self, n: u32) {
        let at = inner_at(self.store.hot[n as usize].kind);
        let inner = self.store.relations[n as usize].deps[at];
        self.refuse_switch_cycle(inner, n);
        self.claim_linear(inner, n);
    }
}
