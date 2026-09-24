//! The engine: the storage seam, the node kinds, and the transaction.
//!
//! Crate-private. Items are `pub` only where a hidden trait method names
//! them, and the module is private, so they are unnameable outside the
//! crate.
//!
//! A node is an index into seven parallel vectors ([`Store`]): its hot
//! record (stamps, position, kind, flags), its relations (dependencies and
//! dependents), its cold bookkeeping, its data plane (`Data`: the slot,
//! committed value or memo that other nodes read), its program (`parts`: its
//! fused chain and closures, which only its own evaluation touches), its
//! static functions (`ops`) and its listeners. Evaluating node N takes N's
//! parts out of the arena, calls N's `eval` with `&mut Build`, the whole
//! arena, and puts them back. N's data stays in the arena, so N can read
//! itself while it runs. A linear dependency takes the event out of the
//! dependency's slot through `&mut`, a shared one clones it through `&`.
//! Every typed access is a checked downcast, and the engine has no `unsafe`.

/// Bumps a counter of the `statistics` feature; expands to nothing without
/// the feature, so the counters cost nothing when off.
macro_rules! count {
    ($s:expr, $field:ident) => {
        #[cfg(feature = "statistics")]
        {
            $s.statistics.$field += 1;
        }
    };
    ($s:expr, $field:ident, $n:expr) => {
        #[cfg(feature = "statistics")]
        {
            $s.statistics.$field += $n as u64;
        }
    };
}

mod children;
mod loops;
pub(crate) mod nodes;
mod pull;
mod sched;
mod store;
mod tx;

use core::any::Any;
use core::cell::OnceCell;

use alloc::vec::Vec;

use crate::build::Build;
use crate::mode::{Carrier, Mode};
use crate::token::Token;
use crate::trace::Tracer;

pub(crate) use sched::Sched;
#[cfg(feature = "statistics")]
pub use sched::Statistics;
pub(crate) use store::{Store, TokenFault};

/// A transaction serial: one per instant, child instants included, so a
/// stamp equal to the current serial means "in this instant". `u64`, so it
/// never wraps and needs no renormalization. Serial 0 is "never"; the build
/// closure, transaction zero, runs as serial 1.
pub(crate) type Tx = u64;

/// The no-op node at index 0. A node pulled out of order has its order
/// entry overwritten with this, so the evaluation loop needs no check.
pub(crate) const NOOP: u32 = 0;
/// An order entry being pulled right now; meeting it again is a cycle.
pub(crate) const IN_PROGRESS: u32 = u32::MAX;
/// The `pos` of a started node: an input sent in this instant, or a split
/// output the child scheduler fired. Started nodes are never ordered.
pub(crate) const START: u32 = u32::MAX;

/// What a node is, which decides how the evaluation loop, `value`,
/// `prepare`, `post` and commit treat it. Stage 1 creates the first six,
/// stage 2 the next two, stage 3 `Loop` and stage 4 the split pair; the
/// switches are here so that the evaluation loop and the data plane need no
/// rewrite later.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    /// Index 0.
    Noop,
    /// Fired by a send; never evaluated.
    Input,
    /// `never`: no dependencies, never marked.
    Never,
    /// A stream node with a program: `node`, `share`, `merge`, and later
    /// `construct`, `scan`, the steps views, a stream loop.
    Stream,
    /// A hold, an accumulator, an input cell: evaluated into `pending`,
    /// committed at commit.
    Hold,
    /// A constant: never marked.
    Constant,
    /// `accumulate_mut`: the event waits in `pending`; `f` runs at commit.
    InPlace,
    /// `map_cell`, `lift`: settled in order, computed on read.
    ReadThrough,
    /// A cell loop's forward node: no dependency until close, then one,
    /// its definition, which it settles after like a read-through cell and
    /// reads through to.
    Loop,
    /// The capture side of `split` and `defer`: takes the event at t and
    /// keeps it for the child scheduler. No dependents.
    SplitCapture,
    /// The output side of `split` and `defer`: no dependencies; started by
    /// the child scheduler in a child instant, like an input by a send.
    SplitOutput,
    /// `switch_cell`: settled in order, relinked at commit.
    SwitchCell,
    /// `switch_stream`: reads its current inner; its outer is reach plus a
    /// watcher.
    SwitchStream,
}

/// The node has listeners: queue it for dispatch when it fires.
pub(crate) const LISTENERS: u8 = 1 << 0;
/// Marking that reaches the node queues its watchers for relink.
pub(crate) const WATCHED: u8 = 1 << 1;
/// On the marking walk's stack: meeting it again is a cycle.
pub(crate) const ON_STACK: u8 = 1 << 2;
/// The node commits when it fires: holds, accumulators.
pub(crate) const COMMITS: u8 = 1 << 3;
/// A switch has linked its current inner.
pub(crate) const LINKED: u8 = 1 << 4;
/// The slot holds a node that collection has not freed.
pub(crate) const LIVE: u8 = 1 << 5;

/// What marking and the evaluation loop touch: 32 bytes, two per cache
/// line.
pub(crate) struct Hot {
    /// Reached by marking in this transaction.
    pub(crate) mark: Tx,
    /// Fired (a stream) or stepped (a cell) in this transaction. A slot is
    /// valid only while `fired == tx`, so nothing is cleared between
    /// transactions.
    pub(crate) fired: Tx,
    /// The transaction that created the node.
    pub(crate) created: Tx,
    /// Position in `order` when `mark == tx`; `START` for a started node.
    pub(crate) pos: u32,
    pub(crate) kind: Kind,
    pub(crate) flags: u8,
}

const _: () = assert!(core::mem::size_of::<Hot>() == 32);

/// The two relations: dependencies (pull, settle, cycle checks, reach) and
/// dependents (marking).
#[derive(Default)]
pub(crate) struct Relations {
    pub(crate) deps: Vec<u32>,
    /// Keeps its capacity across relinks, so a relink into an inner seen
    /// before does not allocate.
    pub(crate) dependents: Vec<u32>,
}

/// Bookkeeping the fast path never reads.
#[derive(Default)]
pub(crate) struct Cold {
    /// Bumped when collection frees the slot (stage 7), so a stale token
    /// fails its check.
    pub(crate) generation: u32,
    /// A split capture's output and back; a switch_stream's outer.
    pub(crate) partner: u32,
    /// Reach that is not a dependency: snapshot and gate cells, `depends`,
    /// a split output's capture, a switch_stream's outer. Collection only.
    pub(crate) reach: Vec<u32>,
    /// switch_streams to queue for relink when marking reaches this cell.
    pub(crate) watchers: Vec<u32>,
    /// Pull of a node created during this transaction: evaluated, and being
    /// evaluated.
    pub(crate) done: Tx,
    pub(crate) pulling: Tx,
    /// `prepare`: entered and finished, so a cycle through a post-instant
    /// read is a panic rather than a stack overflow, and a diamond prepares
    /// once.
    pub(crate) prep_enter: Tx,
    pub(crate) prep_done: Tx,
    /// Deduplicates the relink list.
    pub(crate) relink: Tx,
    /// Path checks; collection's mark epoch.
    pub(crate) visit: u64,
    /// The switch_stream taking events from this linear stream, if any
    /// (stage 5's one-consumer backstop).
    pub(crate) linear_consumer: u32,
}

/// A stateful cell's value: a hold, an accumulator, a constant, an input
/// cell. Evaluation writes `pending`; commit moves it into `value`, so every
/// read during a transaction sees the value from before the instant.
pub(crate) struct CellValue<A> {
    pub(crate) value: A,
    pub(crate) pending: Option<A>,
}

/// A read-through cell's memo, the value before the instant, in a
/// `OnceCell` so that `sample(&self) -> &A` can compute it; and its
/// post-instant value, written through `&mut` by a steps view and promoted
/// into the memo at commit.
pub(crate) struct Memo<A> {
    pub(crate) value: OnceCell<A>,
    pub(crate) post_value: Option<A>,
}

impl<A> Memo<A> {
    pub(crate) fn new() -> Self {
        Memo {
            value: OnceCell::new(),
            post_value: None,
        }
    }
}

/// A node's data plane: what other nodes, samples and listeners read.
pub(crate) enum Data<M: Mode> {
    /// Node 0, `never`, a cell loop, a stream loop until its close creates
    /// its slot, a split capture, a switch_cell.
    Empty,
    /// `Option<A>`: every stream node.
    Slot(M::Carrier),
    /// `CellValue<A>`: a hold, an accumulator, a constant, an input cell.
    Cell(M::Carrier),
    /// `S` and `Option<E>`: `accumulate_mut`'s state and pending event.
    InPlace {
        state: M::Carrier,
        pending: M::Carrier,
    },
    /// `F` and `Memo<B>`: `map_cell`, `lift`.
    ReadThrough { f: M::Carrier, memo: M::Carrier },
}

/// A node's program runs with the whole build context.
pub(crate) type EvalFn<M> = fn(&mut [<M as Mode>::Carrier], &mut Build<M>, u32);
/// A coalescing input folds a second send into its slot.
pub(crate) type CoalesceFn<M> = fn(&mut [<M as Mode>::Carrier], &mut Data<M>, &mut dyn Any);
/// Collection visits the tokens a committed value holds.
pub(crate) type TraceFn<M> = fn(&Data<M>, &[<M as Mode>::Carrier], &mut Tracer);

/// A node type's static functions, monomorphized at materialization and
/// promoted to `'static`. Entries a kind does not use are no-ops.
pub(crate) struct Ops<M: Mode> {
    /// Runs the node at this instant: program nodes only.
    pub(crate) eval: EvalFn<M>,
    /// Commit: a hold moves `pending` into `value`; an in-place accumulator
    /// runs its function.
    pub(crate) commit: fn(&mut [M::Carrier], &mut Data<M>),
    /// A read-through cell's value before the instant, computing the memo.
    pub(crate) value: for<'a> fn(&'a Build<M>, u32) -> &'a dyn Any,
    /// A read-through cell's post-instant value, computed by `prepare`.
    pub(crate) compute_post: fn(&mut Build<M>, u32),
    /// A stepped read-through cell at commit: promote the post-instant
    /// value into the memo, or clear the memo.
    pub(crate) settle_memo: fn(&mut Data<M>),
    /// A switch: the token inside the outer's value, before (`false`) or
    /// after (`true`) the instant.
    pub(crate) inner: fn(&Build<M>, u32, bool) -> Token,
    /// A split or defer capture: starts its output with the next element
    /// of its top entry, and says whether there was one.
    pub(crate) emit_child: fn(&mut [M::Carrier], &mut Build<M>, u32) -> bool,
    /// A split or defer capture: the level that pushed its top entry is
    /// done, so it pops it.
    pub(crate) end_children: fn(&mut [M::Carrier]),
    /// A coalescing input: folds the new value, an `Option<A>` it takes,
    /// into the slot, first send on the left. The default leaves the value
    /// where it is, which the caller reports as a double send.
    pub(crate) coalesce: CoalesceFn<M>,
    /// Collection: visits the tokens in the committed value.
    pub(crate) trace: TraceFn<M>,
    /// Collection: empties a stream slot, so an event holding a token
    /// neither roots nor dangles.
    pub(crate) clear_slot: fn(&mut Data<M>),
}

fn no_eval<M: Mode>(_: &mut [M::Carrier], _: &mut Build<M>, _: u32) {}
fn no_commit<M: Mode>(_: &mut [M::Carrier], _: &mut Data<M>) {}
fn no_value<M: Mode>(_: &Build<M>, n: u32) -> &dyn Any {
    unreachable!("bough engine: node {n} is not a read-through cell")
}
fn no_compute_post<M: Mode>(_: &mut Build<M>, _: u32) {}
fn no_settle_memo<M: Mode>(_: &mut Data<M>) {}
fn no_inner<M: Mode>(_: &Build<M>, n: u32, _: bool) -> Token {
    unreachable!("bough engine: node {n} is not a switch")
}
fn no_emit_child<M: Mode>(_: &mut [M::Carrier], _: &mut Build<M>, _: u32) -> bool {
    false
}
fn no_end_children<M: Mode>(_: &mut [M::Carrier]) {}
fn no_coalesce<M: Mode>(_: &mut [M::Carrier], _: &mut Data<M>, _: &mut dyn Any) {}
fn no_trace<M: Mode>(_: &Data<M>, _: &[M::Carrier], _: &mut Tracer) {}
fn no_clear_slot<M: Mode>(_: &mut Data<M>) {}

impl<M: Mode> Ops<M> {
    /// Every entry a no-op: node 0, inputs, constants, `never`.
    pub(crate) const DEFAULT: Self = Ops {
        eval: no_eval::<M>,
        commit: no_commit::<M>,
        value: no_value::<M>,
        compute_post: no_compute_post::<M>,
        settle_memo: no_settle_memo::<M>,
        inner: no_inner::<M>,
        emit_child: no_emit_child::<M>,
        end_children: no_end_children::<M>,
        coalesce: no_coalesce::<M>,
        trace: no_trace::<M>,
        clear_slot: no_clear_slot::<M>,
    };
}

/// Implemented by one zero-sized marker type per node type. `&T::OPS` is
/// promoted to `&'static`, so a node carries one pointer to its functions.
pub(crate) trait NodeOps<M: Mode> {
    const OPS: Ops<M>;
}

/// One listener: its flag, its erased closure, and the monomorphized call.
pub(crate) struct Entry<M: Mode> {
    pub(crate) flag: M::Flag,
    pub(crate) f: M::Carrier,
    pub(crate) call: fn(&mut M::Carrier, &mut Build<M>, u32),
}

/// The context of the hidden `Source::pull`: the build context, borrowed
/// for one evaluation. `pub` so the trait can name it; unnameable outside
/// the crate and constructible only by the engine, so graph code cannot
/// call `pull`.
pub struct Cx<'a, M: Mode> {
    pub(crate) b: &'a mut Build<M>,
}

impl<M: Mode> Cx<'_, M> {
    /// A linear dependency: takes the event out of the slot.
    pub(crate) fn take<A: 'static>(&mut self, i: u32) -> Option<A> {
        self.b.take_event::<A>(i)
    }

    /// A shared dependency: clones the event and leaves it for the other
    /// consumers.
    pub(crate) fn cloned<A: Clone + 'static>(&self, i: u32) -> Option<A> {
        self.b.clone_event::<A>(i)
    }

    /// A cell read inside a stream function: the value before the instant.
    pub(crate) fn sample<A: 'static>(&self, i: u32) -> &A {
        self.b.value::<A>(i)
    }
}

// ----- checked downcasts of the data plane and the program -----
//
// A wrong type here is an engine bug. It panics; it never reads the wrong
// memory.

pub(crate) fn slot<M: Mode, A: 'static>(d: &Data<M>) -> &Option<A> {
    match d {
        Data::Slot(c) => c
            .get()
            .downcast_ref::<Option<A>>()
            .expect("bough engine: slot type"),
        _ => panic!("bough engine: not a stream node"),
    }
}

pub(crate) fn slot_mut<M: Mode, A: 'static>(d: &mut Data<M>) -> &mut Option<A> {
    match d {
        Data::Slot(c) => c
            .get_mut()
            .downcast_mut::<Option<A>>()
            .expect("bough engine: slot type"),
        _ => panic!("bough engine: not a stream node"),
    }
}

pub(crate) fn cell<M: Mode, A: 'static>(d: &Data<M>) -> &CellValue<A> {
    match d {
        Data::Cell(c) => c
            .get()
            .downcast_ref::<CellValue<A>>()
            .expect("bough engine: cell type"),
        _ => panic!("bough engine: not a stateful cell"),
    }
}

pub(crate) fn cell_mut<M: Mode, A: 'static>(d: &mut Data<M>) -> &mut CellValue<A> {
    match d {
        Data::Cell(c) => c
            .get_mut()
            .downcast_mut::<CellValue<A>>()
            .expect("bough engine: cell type"),
        _ => panic!("bough engine: not a stateful cell"),
    }
}

pub(crate) fn in_place<M: Mode, S: 'static>(d: &Data<M>) -> &S {
    match d {
        Data::InPlace { state, .. } => state
            .get()
            .downcast_ref::<S>()
            .expect("bough engine: state type"),
        _ => panic!("bough engine: not an in-place accumulator"),
    }
}

/// An in-place accumulator's state and its pending event, both mutable.
pub(crate) fn in_place_mut<M: Mode, S: 'static, E: 'static>(
    d: &mut Data<M>,
) -> (&mut S, &mut Option<E>) {
    match d {
        Data::InPlace { state, pending } => (
            state
                .get_mut()
                .downcast_mut::<S>()
                .expect("bough engine: state type"),
            pending
                .get_mut()
                .downcast_mut::<Option<E>>()
                .expect("bough engine: pending event type"),
        ),
        _ => panic!("bough engine: not an in-place accumulator"),
    }
}

pub(crate) fn memo<M: Mode, A: 'static>(d: &Data<M>) -> &Memo<A> {
    match d {
        Data::ReadThrough { memo, .. } => memo
            .get()
            .downcast_ref::<Memo<A>>()
            .expect("bough engine: memo type"),
        _ => panic!("bough engine: not a read-through cell"),
    }
}

pub(crate) fn memo_mut<M: Mode, A: 'static>(d: &mut Data<M>) -> &mut Memo<A> {
    match d {
        Data::ReadThrough { memo, .. } => memo
            .get_mut()
            .downcast_mut::<Memo<A>>()
            .expect("bough engine: memo type"),
        _ => panic!("bough engine: not a read-through cell"),
    }
}

pub(crate) fn part<M: Mode, T: 'static>(c: &mut M::Carrier) -> &mut T {
    c.get_mut()
        .downcast_mut::<T>()
        .expect("bough engine: part type")
}
