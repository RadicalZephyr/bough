//! The rollback probe (the handoff of 2026-10-04, in the RFD repository's
//! notes): refusing a transaction that fails, and leaving the graph as it
//! was before it, where the engine would otherwise poison. Two
//! mechanisms, each a cargo feature. `undo`, mechanism A, catches the
//! panic and undoes from a log everything the instant did, commit
//! included. `stage`, mechanism B, moves every check ahead of commit and
//! lets a construct closure return an error, so a refusal only throws
//! away what the instant made, before anything links to it. Both wait on
//! [`Runtime::set_rollback`](crate::Runtime::set_rollback), off by
//! default, so a runtime that doesn't ask poisons as before.
//!
//! What the two share is here: the policy, the log, and the roll back
//! that reads it. Every hook the engine calls is an empty inline function
//! without the features, so the unchanged engine is what they add to.

#[cfg(any(feature = "undo", feature = "stage"))]
use alloc::string::String;
#[cfg(any(feature = "undo", feature = "stage"))]
use alloc::vec::Vec;

use crate::build::Build;
#[cfg(any(feature = "undo", feature = "stage"))]
use crate::error::Refusal;
use crate::mode::Mode;

#[cfg(any(feature = "undo", feature = "stage"))]
use super::{Kind, LISTENERS, NOOP, WATCHED};

/// A switch's move at relink, as `move_inner` makes it.
#[cfg(any(feature = "undo", feature = "stage"))]
pub(crate) struct Move {
    switch: u32,
    /// Where the switch keeps its inner among its dependencies.
    at: usize,
    old: u32,
    new: u32,
    /// The switch's place in the old inner's dependents, which marking
    /// follows, so the order comes back too.
    position: usize,
    /// The linear claims on the old and the new inner before the move.
    old_claim: u32,
    new_claim: u32,
}

/// The probe's state, in the scheduler's buffers. The log is the
/// instant's: a new instant clears it.
#[cfg(any(feature = "undo", feature = "stage"))]
#[derive(Default)]
pub(crate) struct Probe {
    /// `Runtime::set_rollback`: refuse what can be undone.
    #[cfg(any(feature = "undo", feature = "stage"))]
    pub(crate) on: bool,
    /// The switches relink moved.
    #[cfg(any(feature = "undo", feature = "stage"))]
    pub(crate) moves: Vec<Move>,
    /// The anchors' length when the instant began: a construct's anchors
    /// come after it.
    #[cfg(any(feature = "undo", feature = "stage"))]
    pub(crate) anchors: usize,
    /// Each node a listener was registered on during the instant, with
    /// the length of its list before: a once-listener tied to the unit.
    #[cfg(any(feature = "undo", feature = "stage"))]
    pub(crate) listened: Vec<(u32, usize)>,
    /// The refusal the last unit ended in, for the runtime to return.
    #[cfg(any(feature = "undo", feature = "stage"))]
    pub(crate) refused: Option<Refusal>,
    /// Whether a panic now can be undone: from the instant's start until
    /// commit runs code no log undoes, an in-place accumulator's function
    /// or a replaced value's `Drop`.
    #[cfg(feature = "undo")]
    pub(crate) undoable: bool,
    /// How many of the instant's commits have run, each parking the value
    /// it replaced in the cell's pending slot.
    #[cfg(feature = "undo")]
    pub(crate) committed: usize,
    /// The innermost node whose code is running, or node 0: what a
    /// refusal names. A panic skips the restore, so it's left naming the
    /// node that panicked.
    #[cfg(feature = "undo")]
    pub(crate) running: core::cell::Cell<u32>,
    /// A refusal found without a panic, by a check moved ahead of commit
    /// or a construct closure's error: the node, and what went wrong.
    #[cfg(feature = "stage")]
    pub(crate) refusing: Option<(Option<u32>, String)>,
    /// Links from a node that existed before the instant to one made in
    /// it, which wait for commit.
    #[cfg(feature = "stage")]
    pub(crate) staged: Vec<(u32, u32)>,
}

impl<M: Mode> Build<M> {
    /// A new instant: nothing to undo yet.
    #[inline]
    pub(crate) fn probe_instant(&mut self) {
        #[cfg(any(feature = "undo", feature = "stage"))]
        {
            let p = &mut self.s.probe;
            p.moves.clear();
            p.listened.clear();
            p.anchors = self.anchors.len();
            #[cfg(feature = "undo")]
            {
                p.undoable = p.on;
                p.committed = 0;
            }
            #[cfg(feature = "stage")]
            {
                p.refusing = None;
                p.staged.clear();
            }
        }
    }

    /// Records switch `n`'s move from `old` to `new`, before relink makes
    /// it.
    #[inline]
    pub(crate) fn record_move(&mut self, n: u32, at: usize, old: u32, new: u32) {
        // A switch made at this instant goes with it: nothing to put back.
        #[cfg(any(feature = "undo", feature = "stage"))]
        if self.s.probe.on && self.store.hot[n as usize].created != self.tx {
            let position = self.store.relations[old as usize]
                .dependents
                .iter()
                .position(|&d| d == n)
                .expect("bough engine: a switch is a dependent of its inner");
            let old_claim = self.store.cold[old as usize].linear_consumer;
            let new_claim = self.store.cold[new as usize].linear_consumer;
            self.s.probe.moves.push(Move {
                switch: n,
                at,
                old,
                new,
                position,
                old_claim,
                new_claim,
            });
        }
        #[cfg(not(any(feature = "undo", feature = "stage")))]
        let _ = (n, at, old, new);
    }

    /// Records a listener about to be registered on node `i` while a
    /// transaction is open: a once-listener tied to the unit.
    #[inline]
    pub(crate) fn record_listener(&mut self, i: u32) {
        #[cfg(any(feature = "undo", feature = "stage"))]
        if self.s.probe.on && self.in_tx {
            let len = self.store.listeners[i as usize].len();
            self.s.probe.listened.push((i, len));
        }
        #[cfg(not(any(feature = "undo", feature = "stage")))]
        let _ = i;
    }

    /// The refusal the last unit ended in, if it was refused.
    #[cfg(any(feature = "undo", feature = "stage"))]
    pub(crate) fn take_refusal(&mut self) -> Option<Refusal> {
        self.s.probe.refused.take()
    }

    /// Rolls back the running instant, whatever phase it stopped in, and
    /// closes the unit: the instant's committed values, memos and switch
    /// moves, what it made and linked, its anchors, scopes, child levels
    /// and tied listeners, and its sends, whose events are dropped. The
    /// serial isn't rolled back, so every stamp the instant left is stale.
    /// Leaves the refusal for the runtime to take. A panic in here, in the
    /// `Drop` of what it frees, happens with the flag still set, so it
    /// poisons.
    #[cfg(any(feature = "undo", feature = "stage"))]
    pub(crate) fn roll_back(&mut self, node: Option<u32>, message: String) {
        let inputs = self.s.starts.clone();
        self.undo_commits();
        self.undo_memos();
        self.undo_moves();
        self.undo_listeners();
        self.end_levels();
        self.discard_created();
        #[cfg(feature = "stage")]
        self.s.probe.staged.clear();
        self.anchors.truncate(self.s.probe.anchors);
        self.s.scopes.clear();
        self.s.open_loops.clear();
        #[cfg(feature = "undo")]
        {
            self.s.probe.running.set(NOOP);
            self.s.probe.undoable = false;
        }
        self.s.depth = 0;
        self.disarm();
        self.cancel();
        self.s.probe.refused = Some(Refusal {
            node,
            message,
            inputs,
        });
    }

    /// Each cell that fired at the instant: one whose commit has run, under
    /// `undo`, gets back the value it parked, and one whose hasn't drops
    /// its pending value, which collection would trace.
    #[cfg(any(feature = "undo", feature = "stage"))]
    fn undo_commits(&mut self) {
        let Build { store, s, .. } = self;
        #[cfg(feature = "undo")]
        let committed = s.probe.committed;
        #[cfg(not(feature = "undo"))]
        let committed = 0;
        for (k, &n) in s.commits.iter().enumerate() {
            let i = n as usize;
            if k < committed {
                #[cfg(feature = "undo")]
                (store.ops[i].unpark)(&mut store.data[i]);
            } else {
                (store.ops[i].clear_pending)(&mut store.data[i]);
            }
        }
    }

    /// Each read-through cell that stepped forgets the value after the
    /// instant beside its memo, which a later commit would promote, and
    /// its memo, which commit may have promoted already; the next read
    /// computes it again from what is committed.
    #[cfg(any(feature = "undo", feature = "stage"))]
    fn undo_memos(&mut self) {
        let Build { store, s, .. } = self;
        for &n in &s.memos {
            (store.ops[n as usize].abort_memo)(&mut store.data[n as usize]);
        }
    }

    /// Moves every switch relink moved back, latest first, to its old
    /// inner, at its old place among that inner's dependents, with the
    /// linear claims as they were.
    #[cfg(any(feature = "undo", feature = "stage"))]
    fn undo_moves(&mut self) {
        while let Some(m) = self.s.probe.moves.pop() {
            let relations = &mut self.store.relations;
            let taken = &mut relations[m.new as usize].dependents;
            let p = taken
                .iter()
                .rposition(|&d| d == m.switch)
                .expect("bough engine: a moved switch is a dependent of its new inner");
            taken.remove(p);
            relations[m.old as usize]
                .dependents
                .insert(m.position, m.switch);
            relations[m.switch as usize].deps[m.at] = m.old;
            self.store.cold[m.old as usize].linear_consumer = m.old_claim;
            self.store.cold[m.new as usize].linear_consumer = m.new_claim;
        }
    }

    /// Drops the entries of the listeners the unit tied to itself.
    #[cfg(any(feature = "undo", feature = "stage"))]
    fn undo_listeners(&mut self) {
        while let Some((i, len)) = self.s.probe.listened.pop() {
            let list = &mut self.store.listeners[i as usize];
            list.truncate(len);
            if list.is_empty() {
                self.store.hot[i as usize].flags &= !LISTENERS;
            }
        }
    }

    /// Ends every child level, as each would have ended: each capture that
    /// fired pops the entry it pushed.
    #[cfg(any(feature = "undo", feature = "stage"))]
    fn end_levels(&mut self) {
        for level in 0..self.s.levels.len() {
            self.end_level(level);
        }
    }

    /// Unlinks every node the instant made from the nodes it depends on,
    /// from the watchers of a switch's outer and from the linear claims
    /// it made, and frees it now, so nothing reaches it and the live count
    /// is back where it was.
    #[cfg(any(feature = "undo", feature = "stage"))]
    fn discard_created(&mut self) {
        let created = core::mem::take(&mut self.s.created);
        for &n in &created {
            let at = n as usize;
            let mut k = 0;
            while k < self.store.relations[at].deps.len() {
                let d = self.store.relations[at].deps[k] as usize;
                // `stage` links a new node into an older node's dependents
                // only at commit, so there it has nothing to unlink.
                #[cfg(feature = "stage")]
                debug_assert!(
                    !self.s.probe.on
                        || self.store.hot[d].created == self.tx
                        || !self.store.relations[d].dependents.contains(&n),
                    "bough engine: node {n}, made at a refused instant, joined node {d}'s \
                     dependents before commit"
                );
                self.store.relations[d].dependents.retain(|&x| x != n);
                if self.store.cold[d].linear_consumer == n {
                    self.store.cold[d].linear_consumer = NOOP;
                }
                k += 1;
            }
            if self.store.hot[at].kind == Kind::SwitchStream {
                let outer = self.store.cold[at].partner as usize;
                let watchers = &mut self.store.cold[outer].watchers;
                watchers.retain(|&w| w != n);
                if watchers.is_empty() {
                    self.store.hot[outer].flags &= !WATCHED;
                }
            }
        }
        for &n in &created {
            self.store.free(n);
        }
        self.s.created = created;
        self.s.created.clear();
    }
}

impl<M: Mode> Build<M> {
    /// `undo`: whether a panic now can be rolled back, which is while a
    /// transaction runs graph code, until commit runs code no log undoes.
    #[cfg(feature = "undo")]
    pub(crate) fn undoable(&self) -> bool {
        self.in_tx && self.s.probe.undoable
    }

    /// `undo`: rolls back after a panic, naming the node whose code was
    /// running, if one was, with the panic's message.
    #[cfg(feature = "undo")]
    pub(crate) fn roll_back_panic(&mut self, payload: &(dyn core::any::Any + Send)) {
        use alloc::string::ToString;
        let message = match payload.downcast_ref::<String>() {
            Some(message) => message.clone(),
            None => match payload.downcast_ref::<&str>() {
                Some(message) => message.to_string(),
                None => "a panic".to_string(),
            },
        };
        let node = match self.s.probe.running.get() {
            NOOP => None,
            n => Some(n),
        };
        self.roll_back(node, message);
    }
}

/// Every live node's structure, for the probe's tests to compare before a
/// refused transaction and after it: its kind and flags, its dependencies,
/// its dependents in order, and the rest of its bookkeeping; and the
/// anchored nodes and the live count. Values aren't in it; the tests read
/// those.
#[cfg(any(feature = "undo", feature = "stage"))]
#[derive(Debug, PartialEq, Eq)]
pub struct Topology {
    nodes: Vec<Shape>,
    anchors: Vec<u32>,
    live: usize,
}

/// One live node, as [`Topology`] records it.
#[cfg(any(feature = "undo", feature = "stage"))]
#[derive(Debug, PartialEq, Eq)]
struct Shape {
    node: u32,
    generation: u32,
    kind: Kind,
    flags: u8,
    deps: Vec<u32>,
    dependents: Vec<u32>,
    reach: Vec<u32>,
    watchers: Vec<u32>,
    partner: u32,
    linear_consumer: u32,
    listeners: usize,
}

impl<M: Mode> Build<M> {
    /// The graph's [`Topology`].
    #[cfg(any(feature = "undo", feature = "stage"))]
    pub(crate) fn topology(&self) -> Topology {
        let store = &self.store;
        let nodes = (1..store.hot.len())
            .filter(|&n| store.hot[n].flags & super::LIVE != 0)
            .map(|n| {
                let (hot, cold, relations) = (&store.hot[n], &store.cold[n], &store.relations[n]);
                Shape {
                    node: n as u32,
                    generation: cold.generation,
                    kind: hot.kind,
                    flags: hot.flags & !super::ON_STACK,
                    deps: relations.deps.clone(),
                    dependents: relations.dependents.clone(),
                    reach: cold.reach.clone(),
                    watchers: cold.watchers.clone(),
                    partner: cold.partner,
                    linear_consumer: cold.linear_consumer,
                    listeners: store.listeners[n].len(),
                }
            })
            .collect();
        Topology {
            nodes,
            anchors: self.anchors.iter().map(|&(i, _)| i).collect(),
            live: store.live,
        }
    }
}

impl<M: Mode> Build<M> {
    /// `stage`: a construct closure at node `n` returned an error. With
    /// rollback on, the instant is refused before commit, naming the
    /// first; without it, the error poisons, as a panic in graph code
    /// does.
    #[cfg(feature = "stage")]
    pub(crate) fn refuse(&mut self, n: u32, message: String) {
        assert!(
            self.s.probe.on,
            "bough: a construct closure at node {n} returned an error: {message}"
        );
        if self.s.probe.refusing.is_none() {
            self.s.probe.refusing = Some((Some(n), message));
        }
    }

    /// `stage`: rolls back the instant if a construct closure refused it.
    /// Returns whether it did.
    #[cfg(feature = "stage")]
    pub(crate) fn refused_early(&mut self) -> bool {
        match self.s.probe.refusing.take() {
            Some((node, message)) => {
                self.roll_back(node, message);
                true
            }
            None => false,
        }
    }

    /// `stage`, with rollback on: moves every queued switch to the inner its
    /// outer holds after the instant, and checks the moves together, as
    /// relink does at commit, but before anything commits. A move that
    /// closes a cycle refuses the instant, and the roll back moves the
    /// switches back. Returns whether it refused.
    #[cfg(feature = "stage")]
    pub(crate) fn relink_early(&mut self) -> bool {
        if !self.s.probe.on {
            return false;
        }
        let mut moved = 0;
        let mut k = 0;
        while k < self.s.relinks.len() {
            let n = self.s.relinks[k];
            if self.move_inner(n, true) {
                self.s.relinks[moved] = n;
                moved += 1;
            }
            k += 1;
        }
        self.s.relinks.truncate(moved);
        let mut k = 0;
        while k < self.s.relinks.len() {
            let n = self.s.relinks[k];
            if let Some(cycle) = self.check_moved_or_refuse(n) {
                self.roll_back(None, cycle);
                return true;
            }
            k += 1;
        }
        false
    }

    /// `stage`: the links that waited for commit, in the order they were
    /// made.
    #[cfg(feature = "stage")]
    pub(crate) fn link_staged(&mut self) {
        let mut staged = core::mem::take(&mut self.s.probe.staged);
        for &(from, to) in &staged {
            self.store.relations[from as usize].dependents.push(to);
        }
        staged.clear();
        self.s.probe.staged = staged;
    }
}
