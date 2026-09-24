//! Every per-transaction buffer. Cleared by setting the length to zero and
//! never freed, so a graph that is not growing does not allocate per
//! transaction.

use alloc::vec::Vec;

/// The scheduler's buffers and settings.
#[derive(Default)]
pub(crate) struct Sched {
    /// The started nodes of this instant, in send order.
    pub(crate) starts: Vec<u32>,
    /// The marking walk's post-order; evaluated from the end.
    pub(crate) order: Vec<u32>,
    /// The order position the evaluation loop is at.
    pub(crate) cursor: u32,
    /// The evaluation loop has finished for this instant.
    pub(crate) order_done: bool,
    /// The marking walk's stack: a node and how many of its dependents it
    /// has visited.
    pub(crate) stack: Vec<(u32, u32)>,
    /// Nodes created in this instant, in creation order.
    pub(crate) created: Vec<u32>,
    /// Nodes that commit at the end of this instant.
    pub(crate) commits: Vec<u32>,
    /// Read-through cells that stepped in this instant.
    pub(crate) memos: Vec<u32>,
    /// Switches to relink at commit.
    pub(crate) relinks: Vec<u32>,
    /// Nodes with listeners that fired, in evaluation order.
    pub(crate) dispatch: Vec<u32>,
    /// `levels[d]`: the split captures that fired in the instant at child
    /// depth `d` (stage 4).
    pub(crate) levels: Vec<Vec<u32>>,
    /// The child scheduler's explicit stack of depths (stage 4).
    pub(crate) frames: Vec<usize>,
    /// The child depth of the running instant; 0 for a top-level one.
    pub(crate) depth: usize,
    /// The path checks' stack (stages 3 and 5).
    pub(crate) search: Vec<(u32, u32)>,
    pub(crate) visit_epoch: u64,
    /// Loops declared and not yet closed; `scopes` holds offsets into it.
    pub(crate) open_loops: Vec<u32>,
    pub(crate) scopes: Vec<usize>,
    /// RFD 1's shuffle affordance: the seed, or `None` for the plain order.
    pub(crate) shuffle: Option<u64>,
    #[cfg(feature = "statistics")]
    pub(crate) statistics: Statistics,
}

/// Counters by phase since the graph was built, so that a regression can be
/// attributed to a phase. Only with the `statistics` feature.
#[cfg(feature = "statistics")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Statistics {
    /// Instants run: each transaction, transaction zero and child
    /// transactions included.
    pub transactions: u64,
    /// Nodes the marking walk reached and ordered.
    pub ordered: u64,
    /// Nodes the evaluation loop ran in order.
    pub evaluations: u64,
    /// Nodes run early, out of order, by memoized pull.
    pub pulls: u64,
    /// Nodes created during a transaction and run in its new-node phase.
    pub new_nodes: u64,
    /// Cells committed.
    pub commits: u64,
    /// Switches relinked.
    pub relinks: u64,
    /// Listener calls.
    pub listener_calls: u64,
}
