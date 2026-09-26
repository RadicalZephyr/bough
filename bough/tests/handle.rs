//! The same-thread handle: a `Local` graph behind an `Owner`, reached
//! through `Io` handles. A call runs now when the graph is idle and right
//! after the call in progress when it is busy; a read cannot wait.
#![cfg(feature = "std")]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Wake, Waker};

use bough::{Cell, Input, Io, IoError, NowError, Owner, Runtime, Source, Stream};

type Log = Rc<RefCell<Vec<String>>>;

/// Two inputs, and a hold over each.
type TwoCells = (Input<u32>, Input<u32>, Cell<u32>, Cell<u32>);

fn two_cells() -> (Runtime, TwoCells) {
    let (graph, edge) = Runtime::build(|build| {
        let (a, a_in) = build.input::<u32>();
        let (b, b_in) = build.input::<u32>();
        (a_in, b_in, a.hold(build, 0), b.hold(build, 0))
    });
    (graph, edge.keep())
}

/// A counter: an input of its own, and the running total of what it gets.
type Counter = (Input<u32>, Cell<u32>);

/// Each event opens a counter that starts from the event's value.
fn counters() -> (Runtime, (Input<u32>, Stream<Counter>)) {
    let (graph, edge) = Runtime::build(|b| {
        let (open, open_in) = b.input::<u32>();
        let opened = open.construct(b, |b, start| {
            let (bumps, bumps_in) = b.input::<u32>();
            (bumps_in, bumps.accumulate(b, start, |n, c| c + n))
        });
        (open_in, opened)
    });
    (graph, edge.keep())
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
fn a_call_that_cannot_wait_is_refused_while_the_graph_is_busy() {
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
                found.borrow_mut().push(inner.pump().map(|()| 0));
            })
            .keep();
    })
    .unwrap();
    io.send(a_in, 1).unwrap();
    assert_eq!(*found.borrow(), [Err(NowError::Busy); 3]);
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

/// So that a driver waiting for work learns the graph is gone and ends.
#[test]
fn dropping_the_owner_wakes_the_driver() {
    let (graph, _) = two_cells();
    let owner = Owner::new(graph);
    let io = owner.io();
    let wakes = Arc::new(Wakes(AtomicUsize::new(0)));
    io.with_graph(|graph| graph.set_waker(Waker::from(wakes.clone())))
        .unwrap();
    assert_eq!(wakes.0.load(Ordering::Relaxed), 0);
    drop(owner);
    assert_eq!(wakes.0.load(Ordering::Relaxed), 1);
    assert_eq!(io.pump(), Err(NowError::Gone));
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
    let (mut graph, edge) = Runtime::build(|b| {
        let (open, open_in) = b.input::<u32>();
        let (_, other_in) = b.input::<u32>();
        let opened = open.construct(b, |b, start| {
            let (bumps, bumps_in) = b.input::<u32>();
            (bumps_in, bumps.accumulate(b, start, |n, c| c + n))
        });
        (open_in, other_in, opened)
    });
    let (open_in, other_in, opened) = edge.keep();
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
    io.with_graph(|graph| graph.anchor(counter).keep()).unwrap();
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
    let (graph, edge) = Runtime::build(move |b| {
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
    let (ios_in, _roots) = edge.keep();
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

/// A listener hears of a counter and wires it. The listener it asks for is
/// registered right after the transaction, before any collection, and its
/// first call runs then.
#[test]
fn a_listener_can_wire_what_it_hears_of() {
    let (mut graph, (open_in, opened)) = counters();
    graph.set_collect_after_every_transaction(true);
    let owner = Owner::new(graph);
    let io = owner.io();
    let log: Log = Rc::default();
    let wired = Rc::new(RefCell::new(Vec::new()));
    let (inner, seen, rows) = (io.clone(), log.clone(), wired.clone());
    io.listen(opened, move |(bumps_in, count): Counter| {
        seen.borrow_mut().push("opened".into());
        let shown = seen.clone();
        let listener = inner
            .listen_cell(count, move |n| {
                shown.borrow_mut().push(format!("count {n}"))
            })
            .unwrap();
        seen.borrow_mut().push("asked".into());
        rows.borrow_mut().push((bumps_in, listener));
    })
    .unwrap()
    .keep();
    io.send(open_in, 10).unwrap();
    assert_eq!(*log.borrow(), ["opened", "asked", "count 10"]);
    let bumps_in = wired.borrow()[0].0;
    io.send(bumps_in, 5).unwrap(); // this one collects first
    assert_eq!(*log.borrow(), ["opened", "asked", "count 10", "count 15"]);
}

#[test]
fn a_listener_dropped_before_it_is_registered_never_is() {
    let (graph, (a_in, b_in, a, b)) = two_cells();
    let owner = Owner::new(graph);
    let io = owner.io();
    let log: Log = Rc::default();
    let (inner, seen) = (io.clone(), log.clone());
    io.listen_steps(a, move |_| {
        let shown = seen.clone();
        let dropped = inner.listen_cell(b, move |n| {
            shown.borrow_mut().push(format!("dropped {n}"));
        });
        drop(dropped);
        let shown = seen.clone();
        inner
            .listen_cell(b, move |n| shown.borrow_mut().push(format!("kept {n}")))
            .unwrap()
            .keep();
    })
    .unwrap()
    .keep();
    io.send(a_in, 1).unwrap();
    io.send(b_in, 2).unwrap();
    assert_eq!(*log.borrow(), ["kept 0", "kept 2"]);
}

#[test]
fn an_anchor_from_a_listener_keeps_what_it_anchors() {
    let (mut graph, (open_in, opened)) = counters();
    graph.set_collect_after_every_transaction(true);
    let owner = Owner::new(graph);
    let io = owner.io();
    let anchored = Rc::new(RefCell::new(Vec::new()));
    let (inner, kept) = (io.clone(), anchored.clone());
    io.listen(opened, move |counter: Counter| {
        let anchor = inner.anchor(counter).unwrap();
        kept.borrow_mut().push((counter, anchor));
    })
    .unwrap()
    .keep();
    io.send(open_in, 10).unwrap();
    io.send(open_in, 20).unwrap(); // collects first: the first counter is anchored
    let counters: Vec<Counter> = anchored.borrow().iter().map(|(c, _)| *c).collect();
    for (bumps_in, _) in &counters {
        io.send(*bumps_in, 1).unwrap(); // a stale input would panic here
    }
    let totals: Vec<u32> = counters
        .iter()
        .map(|(_, count)| io.with_sample(*count, |n| *n).unwrap())
        .collect();
    assert_eq!(totals, [11, 21]);
}

/// The cost of waiting: a listener registered from a listener misses the
/// events of the child transactions its transaction started.
#[test]
fn a_listener_registered_from_a_listener_misses_its_transactions_child_instants() {
    let (graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let numbers = numbers.share(b);
        let later = numbers.defer(b).share(b);
        (numbers_in, numbers, later)
    });
    let (numbers_in, numbers, later) = edge.keep();
    let owner = Owner::new(graph);
    let io = owner.io();
    let log: Log = Rc::default();
    let (inner, seen) = (io.clone(), log.clone());
    let mut first = true;
    io.listen(numbers, move |n| {
        seen.borrow_mut().push(format!("now {n}"));
        if std::mem::take(&mut first) {
            let shown = seen.clone();
            inner
                .listen(later, move |n| {
                    shown.borrow_mut().push(format!("later {n}"))
                })
                .unwrap()
                .keep();
        }
    })
    .unwrap()
    .keep();
    io.send(numbers_in, 1).unwrap();
    io.send(numbers_in, 2).unwrap();
    assert_eq!(*log.borrow(), ["now 1", "now 2", "later 2"]);
}

/// The pump case of receive, then wire. Two remote units each open a
/// counter, and the listener anchors it through the handle. The queue runs
/// after each unit, before the next unit's collection, so both survive.
#[test]
fn a_pump_runs_the_queue_between_units() {
    let (mut graph, (open_in, opened)) = counters();
    graph.set_collect_after_every_transaction(true);
    let remote = graph.remote();
    let owner = Owner::new(graph);
    let io = owner.io();
    let anchored = Rc::new(RefCell::new(Vec::new()));
    let (inner, kept) = (io.clone(), anchored.clone());
    io.listen(opened, move |counter: Counter| {
        let anchor = inner.anchor(counter).unwrap();
        kept.borrow_mut().push((counter, anchor));
    })
    .unwrap()
    .keep();
    remote.send(open_in, 10);
    remote.send(open_in, 20);
    io.pump().unwrap();
    let counters: Vec<Counter> = anchored.borrow().iter().map(|(c, _)| *c).collect();
    for (bumps_in, _) in &counters {
        io.send(*bumps_in, 1).unwrap(); // a stale input would panic here
    }
    let totals: Vec<u32> = counters
        .iter()
        .map(|(_, count)| io.with_sample(*count, |n| *n).unwrap())
        .collect();
    assert_eq!(totals, [11, 21]);
}

#[test]
fn what_one_unit_asks_for_runs_before_the_next_unit() {
    let (graph, (a_in, b_in, a, b)) = two_cells();
    let remote = graph.remote();
    let owner = Owner::new(graph);
    let io = owner.io();
    let log: Log = Rc::default();
    let (echo, seen) = (io.clone(), log.clone());
    io.listen_steps(a, move |n| {
        seen.borrow_mut().push(format!("a {n}"));
        echo.send(b_in, n * 10).unwrap();
    })
    .unwrap()
    .keep();
    let seen = log.clone();
    io.listen_steps(b, move |n| seen.borrow_mut().push(format!("b {n}")))
        .unwrap()
        .keep();
    remote.send(a_in, 1);
    remote.send(a_in, 2);
    io.pump().unwrap();
    assert_eq!(*log.borrow(), ["a 1", "b 10", "a 2", "b 20"]);
}
