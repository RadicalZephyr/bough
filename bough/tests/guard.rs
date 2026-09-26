//! The guard of RFD 6: a remote call from the driver thread while graph
//! code runs, evaluation and commit, is refused with `FromGraphCode` in
//! both builds; from a listener, from the closure of a transaction and
//! from another thread it queues. And the poison mirror: once an entry
//! finds the graph poisoned, every remote call fails.
#![cfg(feature = "std")]

use std::any::Any;
use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::thread;

use bough::{Input, IoError, PumpError, RemoteIo, Runtime, Source};

fn panic_message<R>(f: impl FnOnce() -> R) -> String {
    let payload = match catch_unwind(AssertUnwindSafe(f)) {
        Ok(_) => panic!("expected a panic"),
        Err(payload) => payload,
    };
    text(&*payload)
}

fn text(payload: &(dyn Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else {
        String::new()
    }
}

/// Whether a remote call from graph code was refused, as a cell value can
/// hold it.
fn inside(result: Result<(), IoError>) -> bool {
    result == Err(IoError::FromGraphCode)
}

type Results = Rc<RefCell<Vec<Result<(), IoError>>>>;

/// A listener that sends back into the graph through a remote, below 10.
fn feedback(remote: RemoteIo, input: Input<u32>, results: Results) -> impl FnMut(u32) + 'static {
    move |n| {
        if n < 10 {
            results.borrow_mut().push(remote.send(input, n + 10));
        }
    }
}

/// A map closure and an in-place accumulator's function, each trying a
/// remote send and a remote transaction, record whether each was refused
/// with `FromGraphCode`, in a transaction `send` opened and in one a unit
/// opened. The graph code gets its remote as an event, since the
/// remote of a graph exists only once the graph does.
#[test]
fn a_remote_send_from_a_map_closure_or_accumulate_mut_is_inside_transaction() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let (remotes, remotes_in) = b.input::<RemoteIo>();
        let remotes = remotes.share(b);
        let mapped = remotes
            .map(move |r| {
                let transaction = r.transaction(move |tx| tx.send(numbers_in, 2));
                (
                    inside(r.send(numbers_in, 1)),
                    transaction == Err(IoError::FromGraphCode),
                )
            })
            .hold(b, (false, false));
        let accumulated = remotes.accumulate_mut(b, Vec::new(), move |r, log: &mut Vec<bool>| {
            log.push(inside(r.send(numbers_in, 3)));
        });
        b.depends(&mapped, &[&numbers]);
        (remotes_in, numbers_in, mapped, accumulated)
    });
    let (remotes_in, numbers_in, mapped, accumulated) = edge.keep();
    let remote = graph.remote_io();
    graph.send(remotes_in, remote.clone());
    assert_eq!(*graph.sample(mapped), (true, true), "evaluation");
    assert_eq!(*graph.sample(accumulated), [true], "commit");
    remote.send(remotes_in, remote.clone()).unwrap();
    graph.pump();
    assert_eq!(
        *graph.sample(accumulated),
        [true, true],
        "a unit's transaction"
    );
    assert_eq!(graph.try_pump(), Ok(()), "nothing was queued");
    graph.send(numbers_in, 4);
}

/// Graph code a `construct` closure runs is guarded, and so is a child
/// instant's, a split's iterator included.
#[test]
fn construct_closures_and_child_instants_are_guarded() {
    struct Probing {
        remote: RemoteIo,
        target: Input<u32>,
        left: u32,
    }
    impl Iterator for Probing {
        type Item = bool;
        fn next(&mut self) -> Option<bool> {
            self.left = self.left.checked_sub(1)?;
            Some(inside(self.remote.send(self.target, 0)))
        }
    }
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let (remotes, remotes_in) = b.input::<RemoteIo>();
        let remotes = remotes.share(b);
        let none = b.constant(false);
        let built = remotes
            .construct(b, move |b, r| b.constant(inside(r.send(numbers_in, 1))))
            .hold(b, none)
            .switch_cell(b);
        let deferred = remotes
            .defer(b)
            .map(move |r| inside(r.send(numbers_in, 2)))
            .hold(b, false);
        let split = remotes
            .map(move |remote| Probing {
                remote,
                target: numbers_in,
                left: 2,
            })
            .split(b)
            .accumulate(b, Vec::new(), |found, all: &Vec<bool>| {
                let mut all = all.clone();
                all.push(found);
                all
            });
        b.depends(&built, &[&numbers]);
        (remotes_in, built, deferred, split)
    });
    let (remotes_in, built, deferred, split) = edge.keep();
    let remote = graph.remote_io();
    graph.send(remotes_in, remote);
    assert!(*graph.sample(built), "a construct closure");
    assert!(*graph.sample(deferred), "a child instant's map");
    assert_eq!(*graph.sample(split), [true, true], "a split's iterator");
}

/// The sanctioned paths queue: a listener, a child instant's listener, and
/// the closure of a local transaction, which is I/O code that runs before
/// the instant does. The next pump runs what they queued.
#[test]
fn listeners_and_transaction_closures_queue() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let numbers = numbers.share(b);
        let later = numbers.defer(b);
        (numbers_in, numbers, later)
    });
    let (numbers_in, numbers, later) = edge.keep();
    let remote = graph.remote_io();
    let results = Results::default();
    let now = feedback(remote.clone(), numbers_in, results.clone());
    graph.listen(numbers, now).keep();
    let in_child = feedback(remote.clone(), numbers_in, results.clone());
    graph.listen(later, in_child).keep();
    let queued = graph.transaction(|tx| {
        tx.send(numbers_in, 1);
        remote.send(numbers_in, 2)
    });
    assert_eq!(queued, Ok(()), "a transaction's closure");
    assert_eq!(
        *results.borrow(),
        [Ok(()), Ok(())],
        "the listener and the child instant's listener"
    );
    let heard = Rc::new(RefCell::new(Vec::new()));
    let sink = heard.clone();
    graph
        .listen(numbers, move |n| sink.borrow_mut().push(n))
        .keep();
    graph.pump();
    assert_eq!(
        *heard.borrow(),
        [2, 11, 11],
        "three units, three transactions"
    );
    assert!(results.borrow().iter().all(Result::is_ok));
}

/// The guard is per thread: while the driver runs graph code, another
/// thread's remote send queues. Here a map closure hands its remote to a
/// thread and waits for that send to return.
#[test]
fn another_thread_queues_while_the_driver_runs_graph_code() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let (remotes, remotes_in) = b.input::<RemoteIo>();
        let found = remotes
            .map(move |remote| {
                let here = inside(remote.send(numbers_in, 8));
                let there = thread::spawn(move || remote.send(numbers_in, 7));
                (here, there.join().unwrap() == Ok(()))
            })
            .hold(b, (false, false));
        (remotes_in, found, numbers.node(b))
    });
    let (remotes_in, found, numbers) = edge.keep();
    let heard = Rc::new(RefCell::new(Vec::new()));
    let sink = heard.clone();
    graph
        .listen(numbers, move |n| sink.borrow_mut().push(n))
        .keep();
    let remote = graph.remote_io();
    graph.send(remotes_in, remote);
    assert_eq!(*graph.sample(found), (true, true));
    graph.pump();
    assert_eq!(*heard.borrow(), [7]);
}

/// The guard knows its own graph's transactions only: graph code of one
/// graph sending through another graph's remote queues there.
#[test]
fn the_guard_is_per_graph() {
    let (mut other, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.accumulate(b, 0u32, |n, t| t + n))
    });
    let (other_in, other_total) = edge.keep();
    let elsewhere = other.remote_io();
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let sent = numbers
            .map(move |n| elsewhere.send(other_in, n).is_ok())
            .hold(b, false);
        (numbers_in, sent)
    });
    let (numbers_in, sent) = edge.keep();
    graph.send(numbers_in, 5);
    assert!(*graph.sample(sent), "queued in the other graph's inbox");
    other.pump();
    assert_eq!(*other.sample(other_total), 5);
}

/// A call checks in the order an `Io`'s does: the runtime has dropped, is
/// poisoned, runs graph code on this thread, or a token the call names is
/// another graph's. So graph code naming a foreign token gets
/// `FromGraphCode`, and a poisoned runtime that has dropped gets `Gone`.
#[test]
fn a_remote_call_reports_the_first_failure_in_a_fixed_order() {
    let (_other, other_edge) = Runtime::build(|b| b.input::<u32>().1);
    let foreign_in = other_edge.keep();
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let (remotes, remotes_in) = b.input::<RemoteIo>();
        let found = remotes
            .map(move |r| r.send(foreign_in, 1) == Err(IoError::FromGraphCode))
            .hold(b, false);
        let checked = numbers
            .map(|n| {
                assert!(n != 13, "thirteen");
                n
            })
            .node(b);
        (numbers_in, remotes_in, found, checked)
    });
    let (numbers_in, remotes_in, found, _checked) = edge.keep();
    let remote = graph.remote_io();
    graph.send(remotes_in, remote.clone());
    assert!(*graph.sample(found), "FromGraphCode before ForeignGraph");
    let _ = catch_unwind(AssertUnwindSafe(|| graph.send(numbers_in, 13)));
    assert_eq!(graph.try_pump(), Err(PumpError::Poisoned));
    assert_eq!(remote.send(numbers_in, 2), Err(IoError::Poisoned));
    drop(graph);
    assert_eq!(remote.send(numbers_in, 3), Err(IoError::Gone));
}

/// Poison mirroring. A panic that escapes graph code leaves the graph
/// poisoned, and the first entry that finds it mirrors the bit into the
/// inbox; from then on every remote send and remote transaction fails,
/// from every thread and through every `RemoteIo`, a new one included.
/// Before that entry, another thread still queues, and the driver thread,
/// whose graph code never finished, is in graph code as far as the guard
/// can tell.
#[test]
fn poisoning_makes_every_later_remote_send_fail() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let checked = numbers
            .map(|n| {
                assert!(n != 13, "thirteen");
                n
            })
            .node(b);
        (numbers_in, checked)
    });
    let (numbers_in, _checked) = edge.keep();
    let remote = graph.remote_io();
    remote.send(numbers_in, 1).unwrap();
    let message = panic_message(|| graph.send(numbers_in, 13));
    assert!(message.contains("thirteen"), "{message}");
    let other = remote.clone();
    let before = thread::spawn(move || other.send(numbers_in, 2));
    assert_eq!(before.join().unwrap(), Ok(()));
    assert_eq!(remote.send(numbers_in, 3), Err(IoError::FromGraphCode));
    assert_eq!(graph.try_pump(), Err(PumpError::Poisoned));
    assert_eq!(remote.send(numbers_in, 4), Err(IoError::Poisoned));
    let other = remote.clone();
    let after = thread::spawn(move || other.send(numbers_in, 5));
    assert_eq!(after.join().unwrap(), Err(IoError::Poisoned));
    assert_eq!(remote.transaction(|_| ()), Err(IoError::Poisoned));
    assert_eq!(
        graph.remote_io().send(numbers_in, 6),
        Err(IoError::Poisoned)
    );
}
