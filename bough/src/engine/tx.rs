//! The transaction: begin, the sends, mark, evaluate, the new-node phase,
//! commit, dispatch, the child transactions, finish (RFD 5).
//!
//! The poison is the transaction-in-progress flag itself: `begin` sets it
//! and only `finish` clears it, after the last child transaction, so any
//! panic in between, in user code, in a check or in a listener, in the
//! instant or in any of its children, leaves it set, and every later entry
//! reports `Poisoned`. There is no drop guard, which is what the abort
//! targets need.

use core::mem;

use super::{COMMITS, Kind, LISTENERS, ON_STACK, START, Tx, WATCHED, slot, slot_mut};
use crate::build::Build;
use crate::mode::{FlagOps, Mode};

/// A second send to a non-coalescing input in one transaction.
#[derive(Debug)]
pub(crate) struct DoubleSend;

/// Salt for the rotation of a node's listeners under the shuffle, so it is
/// not the rotation of the same node's dependents.
const LISTENER_SALT: u64 = 0x5EED_115E_7E4E_D5A1;

/// A seeded rotation in `0..len` for node `n` in instant `tx`: splitmix64
/// over the three. Any depth-first order of the dependents gives a valid
/// order, so rotating where each list starts moves evaluation and dispatch
/// order and leaves the semantics alone.
fn rotation(seed: u64, tx: Tx, n: u32, len: usize) -> usize {
    let mut z = seed
        .wrapping_add(tx.wrapping_mul(0x9E37_79B9_7F4A_7C15))
        .wrapping_add(u64::from(n).wrapping_mul(0xD1B5_4A32_D192_ED03));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z % len as u64) as usize
}

impl<M: Mode> Build<M> {
    // ----------------------------------------------------------- slots

    /// A linear read: the event, moved out of the slot, if the node fired
    /// in this instant. A stale slot is never touched.
    pub(crate) fn take_event<A: 'static>(&mut self, i: u32) -> Option<A> {
        if self.store.hot[i as usize].fired != self.tx {
            return None;
        }
        slot_mut::<M, A>(&mut self.store.data[i as usize]).take()
    }

    /// A shared read: a clone of the event, left in the slot for the other
    /// consumers, if the node fired in this instant.
    pub(crate) fn clone_event<A: Clone + 'static>(&self, i: u32) -> Option<A> {
        if self.store.hot[i as usize].fired != self.tx {
            return None;
        }
        slot::<M, A>(&self.store.data[i as usize]).clone()
    }

    /// A stream node fires: its event goes in its slot.
    pub(crate) fn put_event<A: 'static>(&mut self, me: u32, v: A) {
        *slot_mut::<M, A>(&mut self.store.data[me as usize]) = Some(v);
        self.set_fired(me);
    }

    /// A stateful cell steps: its new value waits in `pending` until commit.
    pub(crate) fn put_pending<A: 'static>(&mut self, me: u32, v: A) {
        super::cell_mut::<M, A>(&mut self.store.data[me as usize]).pending = Some(v);
        self.set_fired(me);
    }

    /// Records that `me` fired (a stream) or stepped (a cell) in this
    /// instant, and queues what commit and dispatch need. Dispatch order is
    /// therefore evaluation order.
    pub(crate) fn set_fired(&mut self, me: u32) {
        let tx = self.tx;
        let h = &mut self.store.hot[me as usize];
        debug_assert!(
            h.fired != tx,
            "bough engine: a node fired twice in one instant"
        );
        h.fired = tx;
        let (kind, flags) = (h.kind, h.flags);
        if flags & COMMITS != 0 {
            self.s.commits.push(me);
        }
        if kind == Kind::ReadThrough {
            self.s.memos.push(me);
        }
        if flags & LISTENERS != 0 {
            self.s.dispatch.push(me);
        }
    }

    // ----------------------------------------------------------- the transaction

    /// Opens a transaction. The flag set here is the poison: only `finish`
    /// clears it.
    pub(crate) fn begin(&mut self) {
        assert!(
            !self.in_tx,
            "bough: the graph is poisoned: a panic escaped an earlier transaction"
        );
        self.in_tx = true;
        self.s.depth = 0;
        self.begin_instant();
    }

    /// A fresh serial and empty buffers, for a transaction or a child
    /// transaction. Nothing in the arena is cleared: stale stamps and slots
    /// are ignored by stamp.
    pub(super) fn begin_instant(&mut self) {
        self.tx += 1;
        let s = &mut self.s;
        s.starts.clear();
        s.order.clear();
        s.created.clear();
        s.commits.clear();
        s.memos.clear();
        s.relinks.clear();
        s.dispatch.clear();
        s.cursor = 0;
        s.order_done = false;
    }

    /// An input, or a split output, starts the instant with an event.
    /// Started nodes are never ordered: the marking walk starts at their
    /// dependents. A second send in the same instant is folded in by a
    /// coalescing input, first send on the left, and is a double send for
    /// any other.
    pub(crate) fn fire_start<A: 'static>(&mut self, i: u32, v: A) -> Result<(), DoubleSend> {
        let tx = self.tx;
        let n = i as usize;
        if self.store.hot[n].fired == tx {
            let mut v = Some(v);
            let ops = self.store.ops[n];
            let parts = self.store.parts[n]
                .as_deref_mut()
                .expect("bough engine: a node's program is in place");
            (ops.coalesce)(parts, &mut self.store.data[n], &mut v);
            // The coalescing function took the value; the default left it.
            return match v {
                None => Ok(()),
                Some(_) => Err(DoubleSend),
            };
        }
        let h = &mut self.store.hot[n];
        h.mark = tx;
        h.pos = START;
        self.put_event(i, v);
        self.s.starts.push(i);
        Ok(())
    }

    /// Everything after the sends: the instant, then its child
    /// transactions, depth first, each a whole instant with its own commit
    /// and listeners. Clears the poison last, so `send` returns after the
    /// last child and a sample then reads what that child committed.
    pub(crate) fn finish(&mut self) {
        self.instant();
        if self.s.levels.first().is_some_and(|l| !l.is_empty()) {
            self.children();
        }
        self.in_tx = false;
    }

    /// The phases of one instant, a transaction or a child transaction,
    /// after its started nodes fired.
    pub(super) fn instant(&mut self) {
        count!(self.s, transactions);
        self.mark();
        self.evaluate();
        self.new_nodes();
        self.commit();
        self.dispatch();
    }

    /// Orders exactly the region the started nodes reach. With the shuffle
    /// off the plain walk runs, so the affordance costs nothing.
    fn mark(&mut self) {
        match self.s.shuffle {
            None => self.mark_walk::<false>(0),
            Some(seed) => self.mark_walk::<true>(seed),
        }
    }

    /// An iterative depth-first walk from each started node's dependents.
    /// Its post-order read backwards is a topological order of exactly the
    /// affected region. Every marked node is ordered, read-through cells
    /// included: marking reaches more than what steps (a hold behind a
    /// filter that rejects is marked and does not step), so whether a node
    /// fired is decided in dependency order. A node with watchers queues its
    /// switch_streams for relink and does not descend into them, since they
    /// are not its dependents.
    ///
    /// The node being walked and its position in its dependents list stay
    /// in locals; only its ancestors wait on the reused stack, so a node
    /// with one dependent costs one push and one pop.
    fn mark_walk<const SHUFFLE: bool>(&mut self, seed: u64) {
        let tx = self.tx;
        let Build { store, s, .. } = self;
        let starts = s.starts.len();
        let first = if SHUFFLE && starts > 1 {
            rotation(seed, tx, u32::MAX, starts)
        } else {
            0
        };
        for r in 0..starts {
            let start = s.starts[if SHUFFLE { (r + first) % starts } else { r }];
            let (mut n, mut k) = (start, 0usize);
            loop {
                let dependents = &store.relations[n as usize].dependents;
                let len = dependents.len();
                if k < len {
                    let at = if SHUFFLE {
                        (k + rotation(seed, tx, n, len)) % len
                    } else {
                        k
                    };
                    let d = dependents[at];
                    k += 1;
                    let h = &mut store.hot[d as usize];
                    if h.mark != tx {
                        h.mark = tx;
                        h.flags |= ON_STACK;
                        if h.flags & WATCHED != 0 {
                            // The watcher hook: switch_streams (stage 5).
                            for w in 0..store.cold[d as usize].watchers.len() {
                                let w = store.cold[d as usize].watchers[w];
                                let c = &mut store.cold[w as usize];
                                if c.relink != tx {
                                    c.relink = tx;
                                    s.relinks.push(w);
                                }
                            }
                        }
                        s.stack.push((n, k as u32));
                        (n, k) = (d, 0);
                    } else {
                        // A backstop: closing a loop and relinking a switch
                        // refuse cycles first.
                        assert!(
                            h.flags & ON_STACK == 0,
                            "bough: a same-instant cycle reached marking at node {d}"
                        );
                    }
                } else {
                    // `n` is finished. The start, at the bottom, is not
                    // ordered.
                    let Some((parent, visited)) = s.stack.pop() else {
                        break;
                    };
                    let h = &mut store.hot[n as usize];
                    h.flags &= !ON_STACK;
                    h.pos = s.order.len() as u32;
                    s.order.push(n);
                    count!(s, ordered);
                    (n, k) = (parent, visited as usize);
                }
            }
        }
    }

    /// A flat loop over the order, backwards. No recursion and no memo
    /// check: a node pulled early had its entry overwritten with node 0.
    fn evaluate(&mut self) {
        let mut i = self.s.order.len();
        while i > 0 {
            i -= 1;
            self.s.cursor = i as u32;
            let n = self.s.order[i];
            #[cfg(feature = "statistics")]
            if n != super::NOOP {
                self.s.statistics.evaluations += 1;
            }
            self.eval_node(n);
        }
        self.s.order_done = true;
    }

    /// Nodes created during this transaction exist from this instant: each
    /// runs once, after its dependencies, by memoized pull, never in
    /// creation order, since a loop's forward is created before its
    /// definition. The list grows while it is walked when a construct in it
    /// fires. Transaction zero has no started nodes, so this phase runs
    /// everything the build closure created.
    fn new_nodes(&mut self) {
        let mut j = 0;
        while j < self.s.created.len() {
            let n = self.s.created[j];
            self.ensure(n);
            j += 1;
        }
    }

    /// Runs one node for this instant, by kind.
    pub(crate) fn eval_node(&mut self, n: u32) {
        match self.store.hot[n as usize].kind {
            Kind::Noop | Kind::Input | Kind::Never | Kind::Constant | Kind::SplitOutput => {}
            Kind::ReadThrough | Kind::Loop => {
                // Settle: stepped iff a dependency fired. No user code runs.
                let tx = self.tx;
                let stepped = self.store.relations[n as usize]
                    .deps
                    .iter()
                    .any(|&d| self.store.hot[d as usize].fired == tx);
                if stepped {
                    self.set_fired(n);
                }
            }
            Kind::SwitchCell => self.settle_switch_cell(n),
            Kind::Stream | Kind::Hold | Kind::InPlace | Kind::SwitchStream | Kind::SplitCapture => {
                // The program leaves the arena while it runs, so it can be
                // handed the whole build context; its data stays behind.
                let ops = self.store.ops[n as usize];
                let mut parts = self.store.parts[n as usize]
                    .take()
                    .expect("bough engine: a node's program is in place");
                (ops.eval)(&mut parts, self, n);
                self.store.parts[n as usize] = Some(parts);
            }
        }
    }

    /// Holds commit; stepped read-through cells promote their post-instant
    /// value or clear their memo; switches relink.
    fn commit(&mut self) {
        {
            let Build { store, s, .. } = self;
            count!(s, commits, s.commits.len());
            for &n in &s.commits {
                let parts = store.parts[n as usize]
                    .as_deref_mut()
                    .expect("bough engine: a node's program is in place");
                (store.ops[n as usize].commit)(parts, &mut store.data[n as usize]);
            }
            for &n in &s.memos {
                (store.ops[n as usize].settle_memo)(&mut store.data[n as usize]);
            }
        }
        self.relink();
    }

    /// Moves every queued switch to the inner its outer holds after commit,
    /// reading the committed values, so after holds and memos. Two passes,
    /// so that the outcome does not depend on the order the switches were
    /// queued in, which the shuffle moves: every switch moves first, and
    /// then each move is checked on the graph they made together. A check
    /// after each move could see a cycle through an edge that a later move
    /// of the same instant removes.
    fn relink(&mut self) {
        let mut moved = 0;
        let mut k = 0;
        while k < self.s.relinks.len() {
            let n = self.s.relinks[k];
            if self.move_inner(n) {
                self.s.relinks[moved] = n;
                moved += 1;
            }
            k += 1;
        }
        self.s.relinks.truncate(moved);
        let mut k = 0;
        while k < self.s.relinks.len() {
            let n = self.s.relinks[k];
            self.check_moved(n);
            k += 1;
        }
    }

    /// Listeners in evaluation order, after commit, with no graph access.
    /// A handle dropped inside a listener only clears a flag, checked before
    /// each call. Ties within a node follow registration order, rotated by
    /// the shuffle when it is on.
    fn dispatch(&mut self) {
        let salt = self.s.shuffle.map(|seed| seed ^ LISTENER_SALT);
        let mut k = 0;
        while k < self.s.dispatch.len() {
            let n = self.s.dispatch[k];
            let mut list = mem::take(&mut self.store.listeners[n as usize]);
            let len = list.len();
            let first = match salt {
                Some(seed) if len > 1 => rotation(seed, self.tx, n, len),
                _ => 0,
            };
            for j in 0..len {
                let at = if first + j < len {
                    first + j
                } else {
                    first + j - len
                };
                let e = &mut list[at];
                if e.flag.is_live() {
                    count!(self.s, listener_calls);
                    (e.call)(&mut e.f, self, n);
                }
            }
            list.retain(|e| e.flag.is_live());
            if list.is_empty() {
                self.store.hot[n as usize].flags &= !LISTENERS;
            }
            self.store.listeners[n as usize] = list;
            k += 1;
        }
    }
}
