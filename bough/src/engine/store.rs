//! The storage seam: seven parallel vectors indexed by node, and the only
//! code that creates a node or validates a token. Everything else addresses
//! nodes by `u32` through these methods, so a bounded backend (RFD 7) can
//! replace the vectors without touching the protocol.
//!
//! Collection (RFD 3) frees a slot by dropping what it stores, clearing its
//! `LIVE` flag and bumping its generation, so a token naming the old node
//! fails its check. A freed slot goes on the back of a first-in first-out
//! free list and a new node takes the slot at its front, so the churn
//! spreads across every slot; a slot whose generation reaches `u32::MAX` is
//! retired instead and never reused, so a stale token from before a wrap
//! can never validate (RFD 7).

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;

use super::{Cold, Data, Entry, Hot, Kind, LIVE, NOOP, Ops, Relations, Tx};
use crate::build::Build;
use crate::mode::Mode;
use crate::token::Token;

/// The arena. Node 0 is the no-op node, so an order entry overwritten with
/// 0 runs nothing and needs no check.
pub(crate) struct Store<M: Mode> {
    pub(crate) hot: Vec<Hot>,
    pub(crate) relations: Vec<Relations>,
    pub(crate) cold: Vec<Cold>,
    pub(crate) data: Vec<Data<M>>,
    /// The program: taken out while it runs.
    pub(crate) parts: Vec<Option<Box<[M::Carrier]>>>,
    pub(crate) ops: Vec<&'static Ops<M>>,
    pub(crate) listeners: Vec<Vec<Entry<M>>>,
    /// Freed slots, oldest first: a new node takes the front.
    pub(crate) free: VecDeque<u32>,
    /// Live nodes, node 0 excluded.
    pub(crate) live: usize,
    /// Nodes allocated since the last collection, for the automatic policy.
    pub(crate) allocated: usize,
    /// Slots retired at the maximum generation, never to be reused.
    pub(crate) retired: usize,
}

impl<M: Mode> Store<M> {
    pub(crate) fn new() -> Self {
        let mut store = Store {
            hot: Vec::new(),
            relations: Vec::new(),
            cold: Vec::new(),
            data: Vec::new(),
            parts: Vec::new(),
            ops: Vec::new(),
            listeners: Vec::new(),
            free: VecDeque::new(),
            live: 0,
            allocated: 0,
            retired: 0,
        };
        let noop = store.alloc(
            Kind::Noop,
            Data::Empty,
            Box::new([]),
            &Ops::<M>::DEFAULT,
            0,
            0,
        );
        debug_assert_eq!(noop, NOOP);
        store.live = 0;
        store.allocated = 0;
        store
    }

    /// A slot for a new node: the oldest freed slot, or a new one at the
    /// end. A reused slot keeps its generation, which its free bumped, and
    /// the capacity of its lists.
    pub(crate) fn alloc(
        &mut self,
        kind: Kind,
        data: Data<M>,
        parts: Box<[M::Carrier]>,
        ops: &'static Ops<M>,
        created: Tx,
        flags: u8,
    ) -> u32 {
        let hot = Hot {
            mark: 0,
            fired: 0,
            created,
            pos: 0,
            kind,
            flags: flags | LIVE,
        };
        self.live += 1;
        self.allocated += 1;
        if let Some(index) = self.free.pop_front() {
            let n = index as usize;
            debug_assert!(self.hot[n].flags & LIVE == 0 && self.listeners[n].is_empty());
            self.hot[n] = hot;
            let relations = &mut self.relations[n];
            relations.deps.clear();
            relations.dependents.clear();
            self.cold[n].reset();
            self.data[n] = data;
            self.parts[n] = Some(parts);
            self.ops[n] = ops;
            return index;
        }
        let index = u32::try_from(self.hot.len())
            .ok()
            .filter(|&i| i != u32::MAX)
            .expect("bough: more than u32::MAX - 1 nodes in one graph");
        self.hot.push(hot);
        self.relations.push(Relations::default());
        self.cold.push(Cold::default());
        self.data.push(data);
        self.parts.push(Some(parts));
        self.ops.push(ops);
        self.listeners.push(Vec::new());
        index
    }

    /// Frees a node that collection found unreachable: drops what it
    /// stores, its values, closures and listeners, clears `LIVE` and bumps
    /// the generation, so that every token naming it fails its check. The
    /// slot goes on the back of the free list, or is retired if its
    /// generation has reached the maximum: one compare. Its relations are
    /// left for the pruning pass and for reuse to clear.
    pub(crate) fn free(&mut self, index: u32) {
        let n = index as usize;
        debug_assert!(index != NOOP && self.hot[n].flags & LIVE != 0);
        self.hot[n].flags = 0;
        self.live -= 1;
        let cold = &mut self.cold[n];
        cold.generation += 1;
        let retire = cold.generation == u32::MAX;
        self.ops[n] = &Ops::<M>::DEFAULT;
        // User code runs here, in the values' and closures' `Drop`: the
        // node is already freed, so whatever it does sees a consistent slot.
        self.data[n] = Data::Empty;
        self.parts[n] = None;
        self.listeners[n].clear();
        if retire {
            self.retired += 1;
        } else {
            self.free.push_back(index);
        }
    }
}

/// Why a token does not name a live node of this graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TokenFault {
    /// The token belongs to another graph.
    Foreign,
    /// The token's node was collected.
    Stale,
}

impl<M: Mode> Build<M> {
    /// Validates a token: graph id, liveness, generation.
    pub(crate) fn lookup(&self, t: Token) -> Result<u32, TokenFault> {
        if t.graph != self.graph_id {
            return Err(TokenFault::Foreign);
        }
        let i = t.index as usize;
        let live = i != NOOP as usize
            && i < self.store.hot.len()
            && self.store.hot[i].flags & LIVE != 0
            && self.store.cold[i].generation == t.generation;
        if live {
            Ok(t.index)
        } else {
            Err(TokenFault::Stale)
        }
    }

    /// Validates a token where misuse panics: materialization, and the
    /// panicking I/O entries.
    pub(crate) fn check(&self, t: Token) -> u32 {
        match self.lookup(t) {
            Ok(i) => i,
            Err(TokenFault::Foreign) => panic!("bough: a token from another graph"),
            Err(TokenFault::Stale) => panic!("bough: a stale token: its node was collected"),
        }
    }

    /// The token naming a node of this graph.
    pub(crate) fn token(&self, index: u32) -> Token {
        Token {
            index,
            generation: self.store.cold[index as usize].generation,
            graph: self.graph_id,
        }
    }

    /// Creates a node at the current transaction and links it to its
    /// dependencies at once. Linking during a transaction is harmless:
    /// marking has finished, and evaluation follows the order and the
    /// dependencies, not the dependents lists. The node is evaluated in the
    /// new-node phase of this transaction, after its dependencies.
    pub(crate) fn materialize(
        &mut self,
        kind: Kind,
        data: Data<M>,
        parts: Box<[M::Carrier]>,
        ops: &'static Ops<M>,
        deps: &[u32],
        flags: u8,
    ) -> u32 {
        let n = self.store.alloc(kind, data, parts, ops, self.tx, flags);
        for &d in deps {
            self.link(d, n);
        }
        self.s.created.push(n);
        n
    }

    /// `to` depends on `from`.
    pub(crate) fn link(&mut self, from: u32, to: u32) {
        self.store.relations[from as usize].dependents.push(to);
        self.store.relations[to as usize].deps.push(from);
    }
}
