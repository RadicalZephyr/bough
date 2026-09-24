//! The storage seam: seven parallel vectors indexed by node, and the only
//! code that creates a node or validates a token. Everything else addresses
//! nodes by `u32` through these methods, so a bounded backend (RFD 7) can
//! replace the vectors without touching the protocol.

use alloc::boxed::Box;
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
    /// Live nodes, node 0 excluded. Collection (stage 7) decrements it and
    /// adds a first-in first-out free list and slot retirement here.
    pub(crate) live: usize,
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
            live: 0,
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
        store
    }

    pub(crate) fn alloc(
        &mut self,
        kind: Kind,
        data: Data<M>,
        parts: Box<[M::Carrier]>,
        ops: &'static Ops<M>,
        created: Tx,
        flags: u8,
    ) -> u32 {
        let index = u32::try_from(self.hot.len())
            .ok()
            .filter(|&i| i != u32::MAX)
            .expect("bough: more than u32::MAX - 1 nodes in one graph");
        self.hot.push(Hot {
            mark: 0,
            fired: 0,
            created,
            pos: 0,
            kind,
            flags: flags | LIVE,
        });
        self.relations.push(Relations::default());
        self.cold.push(Cold::default());
        self.data.push(data);
        self.parts.push(Some(parts));
        self.ops.push(ops);
        self.listeners.push(Vec::new());
        self.live += 1;
        index
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
