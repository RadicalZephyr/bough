//! The same-thread handle: a `Local` graph behind an `Owner`, reached
//! through `Io` handles. A call runs now when the graph is idle and right
//! after the call in progress when it is busy; a read cannot wait.
#![cfg(feature = "std")]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Wake, Waker};

use bough::{Cell, Graph, Input, Io, IoError, NowError, Owner, Source};

type Log = Rc<RefCell<Vec<String>>>;

/// Two inputs, and a hold over each.
type TwoCells = (Input<u32>, Input<u32>, Cell<u32>, Cell<u32>);

fn two_cells() -> (Graph, TwoCells) {
    Graph::build(|build| {
        let (a, a_in) = build.input::<u32>();
        let (b, b_in) = build.input::<u32>();
        (a_in, b_in, a.hold(build, 0), b.hold(build, 0))
    })
}

/// A waker that counts its wakes.
struct Wakes(AtomicUsize);

impl Wake for Wakes {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn a_call_on_an_idle_graph_runs_now() {
    let (graph, (a_in, b_in, a, b)) = two_cells();
    let owner = Owner::new(graph);
    let io = owner.io();
    io.send(a_in, 1).unwrap();
    assert_eq!(io.with_sample(a, |n| *n), Ok(1));
    io.transaction(move |tx| {
        tx.send(a_in, 2);
        tx.send(b_in, 3);
    })
    .unwrap();
    assert_eq!(io.with_sample(a, |n| *n), Ok(2));
    assert_eq!(io.with_sample(b, |n| *n), Ok(3));
}

#[test]
fn a_send_from_a_listener_runs_right_after_its_transaction() {
    let (graph, (a_in, b_in, a, b)) = two_cells();
    let owner = Owner::new(graph);
    let io = owner.io();
    let log: Log = Rc::default();
    io.with_graph(|graph| {
        let (echo, seen) = (io.clone(), log.clone());
        graph
            .listen_steps(a, move |n| {
                seen.borrow_mut().push(format!("a {n}"));
                echo.send(b_in, n * 10).unwrap(); // the graph is busy: this waits
                seen.borrow_mut().push("a asked".into());
            })
            .keep();
        let seen = log.clone();
        graph
            .listen_steps(b, move |n| seen.borrow_mut().push(format!("b {n}")))
            .keep();
    })
    .unwrap();
    io.send(a_in, 1).unwrap();
    // The listener finished before the send it asked for ran, and that ran
    // before the call returned.
    assert_eq!(*log.borrow(), ["a 1", "a asked", "b 10"]);
}

#[test]
fn a_read_while_the_graph_is_busy_is_refused() {
    let (graph, (a_in, _, a, b)) = two_cells();
    let owner = Owner::new(graph);
    let io = owner.io();
    let found = Rc::new(RefCell::new(Vec::new()));
    io.with_graph(|graph| {
        let (inner, found) = (io.clone(), found.clone());
        graph
            .listen_steps(a, move |_| {
                found.borrow_mut().push(inner.with_sample(b, |n| *n));
                found.borrow_mut().push(inner.with_graph(|_| 0));
            })
            .keep();
    })
    .unwrap();
    io.send(a_in, 1).unwrap();
    assert_eq!(*found.borrow(), [Err(NowError::Busy), Err(NowError::Busy)]);
    // A read inside a read is not refused: both only read.
    let nested = io.with_sample(a, |_| io.with_sample(b, |n| *n)).unwrap();
    assert_eq!(nested, Ok(0));
}

#[test]
fn a_listener_that_holds_an_io_does_not_keep_the_graph_alive() {
    let (graph, (a_in, b_in, a, _)) = two_cells();
    let owner = Owner::new(graph);
    let io = owner.io();
    io.with_graph(|graph| {
        let echo = io.clone();
        graph
            .listen_steps(a, move |n| echo.send(b_in, *n).unwrap())
            .keep();
    })
    .unwrap();
    drop(owner);
    // The graph went with the owner, and the listener and its Io with it.
    assert_eq!(io.send(a_in, 1), Err(IoError::Gone));
    assert_eq!(io.with_sample(a, |n| *n), Err(NowError::Gone));
}

/// A listener that always sends: each run of the queue takes the calls
/// queued when it starts, so every call returns, and what is left wakes
/// the driver. What waited runs before a later call, in the order it was
/// asked for.
#[test]
fn a_listener_that_always_sends_cannot_keep_a_call_from_returning() {
    let (graph, (a_in, b_in, a, b)) = two_cells();
    let owner = Owner::new(graph);
    let io = owner.io();
    let wakes = Arc::new(Wakes(AtomicUsize::new(0)));
    let log: Log = Rc::default();
    io.with_graph(|graph| {
        graph.set_waker(Waker::from(wakes.clone()));
        let (again, seen) = (io.clone(), log.clone());
        graph
            .listen_steps(a, move |n| {
                seen.borrow_mut().push(format!("a {n}"));
                again.send(a_in, n + 1).unwrap();
            })
            .keep();
        let seen = log.clone();
        graph
            .listen_steps(b, move |n| seen.borrow_mut().push(format!("b {n}")))
            .keep();
    })
    .unwrap();
    let woken = || wakes.0.load(Ordering::Relaxed);

    io.send(a_in, 0).unwrap();
    // The send, then one run of the queue: the listener's send of 1. The
    // send of 2 waits, and woke the driver.
    assert_eq!(*log.borrow(), ["a 0", "a 1"]);
    assert_eq!(woken(), 1);

    io.send(b_in, 7).unwrap();
    // What waited first, then the call, then one run of what they asked
    // for; the send of 4 waits.
    assert_eq!(*log.borrow(), ["a 0", "a 1", "a 2", "b 7", "a 3"]);
    assert_eq!(woken(), 2);
}

/// Receive, then wire, with a send in between that a listener asked for:
/// the queue's send opens no collection, so the token the listener handed
/// over is still alive when the code outside anchors it.
#[test]
fn no_collection_runs_between_a_transaction_and_the_calls_its_listeners_asked_for() {
    let (mut graph, (open_in, other_in, opened)) = Graph::build(|b| {
        let (open, open_in) = b.input::<u32>();
        let (_, other_in) = b.input::<u32>();
        let opened = open.construct(b, |b, start| {
            let (bumps, bumps_in) = b.input::<u32>();
            (bumps_in, bumps.accumulate(b, start, |n, c| c + n))
        });
        (open_in, other_in, opened)
    });
    graph.set_collect_after_every_transaction(true);
    let owner = Owner::new(graph);
    let io = owner.io();
    let received = Rc::new(RefCell::new(Vec::new()));
    io.with_graph(|graph| {
        let (echo, received) = (io.clone(), received.clone());
        graph
            .listen(opened, move |counter| {
                received.borrow_mut().push(counter);
                echo.send(other_in, 1).unwrap();
            })
            .keep();
    })
    .unwrap();
    // Receive, then wire.
    io.send(open_in, 10).unwrap();
    let counter = received.borrow()[0];
    io.with_graph(|graph| graph.anchor(&counter).keep())
        .unwrap();
    let (bumps_in, count) = counter;
    io.send(bumps_in, 5).unwrap(); // this one collects first
    assert_eq!(io.with_sample(count, |n| *n), Ok(15));
}

/// Graph code gets the handle as an event, since the handle exists only
/// once the graph does. A `map` function and a `construct` closure each
/// try a call that could wait and one that could not.
#[test]
fn a_call_from_graph_code_is_refused() {
    let log: Log = Rc::default();
    let (map_log, construct_log) = (log.clone(), log.clone());
    let (graph, (ios_in, _roots)) = Graph::build(move |b| {
        let (ios, ios_in) = b.input::<Io>();
        let ios = ios.share(b);
        let (numbers, numbers_in) = b.input::<u32>();
        let latest = numbers.hold(b, 0);
        let mapped = ios
            .map(move |io: Io| {
                let sent = io.send(numbers_in, 1);
                let read = io.with_sample(latest, |n| *n);
                map_log.borrow_mut().push(format!("map: {sent:?} {read:?}"));
            })
            .hold(b, ());
        let built = ios.construct(b, move |_, io: Io| {
            let sent = io.transaction(move |tx| tx.send(numbers_in, 2));
            let graph = io.with_graph(|_| ());
            construct_log
                .borrow_mut()
                .push(format!("construct: {sent:?} {graph:?}"));
        });
        (ios_in, (mapped, built.hold(b, ()), latest, numbers_in))
    });
    let owner = Owner::new(graph);
    let io = owner.io();
    io.send(ios_in, io.clone()).unwrap();
    let mut log = log.borrow().clone();
    log.sort();
    assert_eq!(
        log,
        [
            "construct: Err(FromGraphCode) Err(FromGraphCode)",
            "map: Err(FromGraphCode) Err(FromGraphCode)",
        ]
    );
}
