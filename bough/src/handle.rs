//! The same-thread handle: a `Local` graph shared with I/O code that runs
//! while the graph is busy.
//!
//! A GTK signal handler or a DOM closure is `Fn + 'static`, and the host
//! can run it while the graph is busy: a listener writes to a widget, and
//! the widget runs its handler at once. Code there cannot hold
//! `&mut Graph`. So an [`Owner`] moves the graph behind an `Rc`, and I/O
//! code holds an [`Io`]: a weak, `Clone`, same-thread handle to it.
//!
//! A call through an `Io` runs as soon as the graph is free: now if it is
//! idle, and right after the call in progress if not, which for a listener
//! is right after its transaction. It is Sodium's `Transaction.post` rule,
//! applied to every call. The only way to see that a call has not run yet
//! is to read the graph, and a read cannot wait, so a read while the graph
//! is busy is refused with [`NowError::Busy`].
//!
//! # The queue
//!
//! The calls that wait go into one queue, in the order they were made.
//! After a call on an idle graph, the queue runs: first what waited from
//! before, then the call itself, then what its listeners asked for. Each
//! run takes the calls queued when it starts. A call made during a run
//! waits for the next one and wakes the driver, so a listener that always
//! sends cannot keep a call from returning, as with
//! [`pump`](Graph::pump). [`Io::pump`] also runs the queue after each slot
//! and each remote unit, so a listener can wire what one unit built before
//! the next unit's collection.
//!
//! # Listeners and anchors
//!
//! A listener or an anchor asked for while the graph is busy waits like
//! any other call, but its handle is handed out at once: dropping it
//! before the registration runs means it never does. A `listen_cell`'s
//! first call runs when the listener is registered. A listener registered
//! after a transaction misses that transaction's events, and those of the
//! child transactions a `split` or a `defer` started in it.
//!
//! # Graph code
//!
//! An `Io` is `Clone + 'static`, so graph code can capture one: a `map`
//! function, a `construct` closure. A call from there would be I/O inside
//! FRP logic, so it is refused with [`IoError::FromGraphCode`], by the
//! thread token that guards [`Remote`](crate::Remote) (RFD 6). A listener
//! is I/O code, so its calls wait instead.
//!
//! # Collection
//!
//! No collection runs while the queue does. A collection runs as a
//! transaction opens, and never between a transaction and the I/O code its
//! listeners report to; the calls in the queue are that code. So a token a
//! listener hands over stays alive until the calls it asked for have run,
//! and after them until the next transaction the code outside opens.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::{Rc, Weak};
use alloc::sync::Arc;
use core::cell::{Cell as CoreCell, RefCell};

use crate::cell::CellRef;
use crate::engine::edge::Inbox;
use crate::error::{IoError, NowError};
use crate::graph::{Anchor, Graph, Listener, Transaction};
use crate::mode::{FlagOps, Local, LocalFlag};
use crate::source::Node;
use crate::token::Input;
use crate::trace::Trace;

/// A call that waits for the graph.
type Call = Box<dyn FnOnce(&mut Graph<Local>)>;

struct Inner {
    graph: RefCell<Graph<Local>>,
    /// The calls made while the graph was busy, in the order they were made.
    queue: RefCell<VecDeque<Call>>,
    /// The graph's inbox, whose poison mirror and guard can be read while
    /// the graph is busy.
    inbox: Arc<Inbox>,
    /// The graph's count of released handles, which the flag of a listener
    /// or an anchor points to, so that one can be made while the graph is
    /// busy.
    released: Rc<CoreCell<usize>>,
}

impl Inner {
    /// Runs the calls queued when it starts, in order, each with the graph.
    /// A call they make waits for the next run.
    fn run_queue(&self, graph: &mut Graph<Local>) {
        let queued = self.queue.borrow().len();
        for _ in 0..queued {
            let Some(call) = self.queue.borrow_mut().pop_front() else {
                break;
            };
            call(graph);
        }
    }

    /// The end of a call on an idle graph: runs what it asked for, and
    /// wakes the driver if calls are left for a later run.
    fn finish(&self, graph: &mut Graph<Local>) {
        self.run_queue(graph);
        if !self.queue.borrow().is_empty() {
            graph.wake();
        }
    }
}

/// Keeps a shared `Local` graph alive, and hands out [`Io`] handles to it.
///
/// It is the graph's only strong reference: dropping it drops the graph,
/// once no call is in progress. An `Io` is weak, so a listener can hold one
/// without keeping the graph alive through itself.
pub struct Owner(Rc<Inner>);

impl Owner {
    /// Moves a built graph behind the owner.
    pub fn new(graph: Graph<Local>) -> Owner {
        let (inbox, released) = (graph.inbox(), graph.released());
        Owner(Rc::new(Inner {
            graph: RefCell::new(graph),
            queue: RefCell::new(VecDeque::new()),
            inbox,
            released,
        }))
    }

    /// A handle for I/O code.
    pub fn io(&self) -> Io {
        Io(Rc::downgrade(&self.0))
    }
}

/// A same-thread handle to a shared graph, for I/O code that the host may
/// run while the graph is busy. Weak, `Clone`, and not `Send`.
///
/// A call runs now if the graph is idle, and right after the call in
/// progress if not; a read cannot wait, so it is refused while the graph
/// is busy. Here a listener sends, and its send runs right after the
/// transaction that ran the listener:
///
/// ```
/// use bough::{Graph, Owner, Source};
///
/// let (graph, (a_in, b_in, a, b)) = Graph::build(|build| {
///     let (a, a_in) = build.input::<u32>();
///     let (b, b_in) = build.input::<u32>();
///     (a_in, b_in, a.hold(build, 0), b.hold(build, 0))
/// });
/// let owner = Owner::new(graph);
/// let io = owner.io();
/// let echo = io.clone();
/// // The graph is busy while its listeners run, so this send waits.
/// io.listen_steps(a, move |n| echo.send(b_in, n * 10).unwrap())
///     .unwrap()
///     .keep();
/// io.send(a_in, 1).unwrap(); // the graph is idle: this runs now
/// assert_eq!(io.with_sample(b, |n| *n), Ok(10));
/// ```
///
/// An `Io` stays on its graph's thread:
///
/// ```compile_fail,E0277
/// fn assert_send<T: Send>() {}
/// assert_send::<bough::Io>(); // error: `Weak<..>` cannot be sent between threads
/// ```
#[derive(Clone)]
pub struct Io(Weak<Inner>);

impl Io {
    /// Sends one value in a transaction of its own, now or right after the
    /// call in progress. When it runs now, a collection that is due runs
    /// first, as for [`Graph::send`]; a send that waited runs without one.
    ///
    /// A send to a collected input, or a token from another graph, follows
    /// [`Graph::send`] when the send runs, which may be later.
    pub fn send<A: 'static>(&self, input: Input<A>, value: A) -> Result<(), IoError> {
        self.request(
            true,
            Box::new(move |graph| graph.send_without_collecting(input, value)),
        )
    }

    /// Several sends in one instant, as [`Graph::transaction`], now or
    /// right after the call in progress.
    pub fn transaction(
        &self,
        f: impl FnOnce(&mut Transaction<'_, Local>) + 'static,
    ) -> Result<(), IoError> {
        self.request(
            true,
            Box::new(move |graph| graph.transaction_without_collecting(f)),
        )
    }

    /// Listens to a materialized node, as [`Graph::listen`], now or right
    /// after the call in progress. The [`Listener`] is handed out at once,
    /// so dropping it before the listener is registered means it never is.
    ///
    /// A listener registered after a transaction misses that transaction's
    /// events, and those of the child transactions a `split` or a `defer`
    /// started in it.
    pub fn listen<S, F>(&self, source: S, f: F) -> Result<Listener, IoError>
    where
        S: Node,
        S::Event: 'static,
        F: FnMut(S::Event) + 'static,
    {
        self.register(move |graph, flag| graph.listen_flagged(flag, source, f))
            .map(Listener::from_flag)
    }

    /// Listens to a cell, as [`Graph::listen_cell`], now or right after the
    /// call in progress. The first call, with the current value, runs when
    /// the listener is registered.
    pub fn listen_cell<C, F>(&self, cell: C, f: F) -> Result<Listener, IoError>
    where
        C: CellRef + 'static,
        F: FnMut(&C::Value) + 'static,
    {
        self.register(move |graph, flag| graph.listen_cell_flagged(flag, cell, f))
            .map(Listener::from_flag)
    }

    /// Listens to a cell's steps, as [`Graph::listen_steps`], now or right
    /// after the call in progress.
    pub fn listen_steps<C, F>(&self, cell: C, f: F) -> Result<Listener, IoError>
    where
        C: CellRef + 'static,
        F: FnMut(&C::Value) + 'static,
    {
        self.register(move |graph, flag| graph.listen_steps_flagged(flag, cell, f))
            .map(Listener::from_flag)
    }

    /// Anchors what `value` holds, as [`Graph::anchor`], now or right after
    /// the call in progress. No collection runs in between, so a listener
    /// can anchor what it is handed.
    pub fn anchor<T: Trace + 'static>(&self, value: T) -> Result<Anchor, IoError> {
        self.register(move |graph, flag| graph.anchor_flagged(flag, &value))
            .map(Anchor::from_flag)
    }

    /// Reads a cell's current value by reference, if the graph is not busy.
    /// What waited runs first, so the read sees it. A read inside another
    /// read sees the same committed values.
    ///
    /// Panics on a stale or foreign token, as [`Graph::sample`] does.
    pub fn with_sample<C: CellRef, R>(
        &self,
        cell: C,
        f: impl FnOnce(&C::Value) -> R,
    ) -> Result<R, NowError> {
        let inner = self.0.upgrade().ok_or(NowError::Gone)?;
        if inner.inbox.inside() {
            return Err(NowError::FromGraphCode);
        }
        if let Ok(mut graph) = inner.graph.try_borrow_mut() {
            if graph.poisoned() {
                return Err(NowError::Poisoned);
            }
            inner.run_queue(&mut graph);
        }
        let graph = inner.graph.try_borrow().map_err(|_| NowError::Busy)?;
        if graph.poisoned() {
            return Err(NowError::Poisoned);
        }
        let r = f(graph.sample(cell));
        drop(graph);
        // What `f` asked for, unless this read is inside another.
        if let Ok(mut graph) = inner.graph.try_borrow_mut() {
            inner.finish(&mut graph);
        }
        Ok(r)
    }

    /// The graph itself, if it is idle: for setup, and for what the handle
    /// does not wrap. What waited runs first, and what `f` asked for after.
    pub fn with_graph<R>(&self, f: impl FnOnce(&mut Graph<Local>) -> R) -> Result<R, NowError> {
        let inner = self.0.upgrade().ok_or(NowError::Gone)?;
        if inner.inbox.inside() {
            return Err(NowError::FromGraphCode);
        }
        let mut graph = inner.graph.try_borrow_mut().map_err(|_| NowError::Busy)?;
        if graph.poisoned() {
            return Err(NowError::Poisoned);
        }
        inner.run_queue(&mut graph);
        let r = f(&mut graph);
        inner.finish(&mut graph);
        Ok(r)
    }

    /// Runs every pending slot and remote unit, as [`Graph::pump`], if the
    /// graph is not busy. The queue runs after each slot's and each unit's
    /// transaction, before the next one opens and before its collection:
    /// a listener can wire what one unit built before the next unit runs.
    pub fn pump(&self) -> Result<(), NowError> {
        let inner = self.0.upgrade().ok_or(NowError::Gone)?;
        if inner.inbox.inside() {
            return Err(NowError::FromGraphCode);
        }
        let mut graph = inner.graph.try_borrow_mut().map_err(|_| NowError::Busy)?;
        if graph.poisoned() {
            return Err(NowError::Poisoned);
        }
        inner.run_queue(&mut graph);
        graph.pump_between(&mut |graph| inner.run_queue(graph));
        inner.finish(&mut graph);
        Ok(())
    }

    /// Makes the flag a handle shares with its registration, and asks for
    /// the registration.
    fn register(
        &self,
        register: impl FnOnce(&mut Graph<Local>, LocalFlag) + 'static,
    ) -> Result<LocalFlag, IoError> {
        let released = self.0.upgrade().ok_or(IoError::Gone)?.released.clone();
        let flag = LocalFlag::live(&released);
        let shared = flag.clone();
        self.request(false, Box::new(move |graph| register(graph, shared)))?;
        Ok(flag)
    }

    /// Runs `call` now if the graph is idle, and queues it if not. Now,
    /// what waited runs first, then a collection that is due if the call
    /// opens a transaction, then the call, then what it asked for.
    fn request(&self, transaction: bool, call: Call) -> Result<(), IoError> {
        let inner = self.0.upgrade().ok_or(IoError::Gone)?;
        if inner.inbox.inside() {
            return Err(IoError::FromGraphCode);
        }
        if inner.inbox.is_poisoned() {
            return Err(IoError::Poisoned);
        }
        let Ok(mut graph) = inner.graph.try_borrow_mut() else {
            inner.queue.borrow_mut().push_back(call);
            return Ok(());
        };
        if graph.poisoned() {
            return Err(IoError::Poisoned);
        }
        inner.run_queue(&mut graph);
        if transaction {
            graph.collect_if_due();
        }
        call(&mut graph);
        inner.finish(&mut graph);
        Ok(())
    }
}
