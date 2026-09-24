//! The guard of RFD 6: a remote send from the driver thread while graph
//! code runs, evaluation and commit, is `InsideTransaction` in both builds;
//! from a listener, from the closure of a transaction and from another
//! thread it queues. And the poison mirror: once an entry finds the graph
//! poisoned, every remote send fails.
#![cfg(feature = "std")]

use std::any::Any;
use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::thread;

use bough::{
    Graph, Input, PoisonedError, PumpError, Remote, RemoteSendError, RemoteTransactionError,
    SendError, Source,
};

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

/// What a remote send from graph code found, as a cell value can hold it.
fn inside(result: Result<(), RemoteSendError>) -> bool {
    result == Err(RemoteSendError::InsideTransaction)
}

type Results = Rc<RefCell<Vec<Result<(), RemoteSendError>>>>;

/// A listener that sends back into the graph through a remote, below 10.
fn feedback(remote: Remote, input: Input<u32>, results: Results) -> impl FnMut(u32) + 'static {
    move |n| {
        if n < 10 {
            results.borrow_mut().push(remote.try_send(input, n + 10));
        }
    }
}

/// A map closure and an in-place accumulator's function, each trying a
/// remote send and a remote transaction, record whether each was refused
/// as `InsideTransaction`, in a transaction `send` opened and in one a
/// unit opened. The graph code gets its remote as an event, since the
/// remote of a graph exists only once the graph does.
#[test]
fn a_remote_send_from_a_map_closure_or_accumulate_mut_is_inside_transaction() {
    let (mut graph, (remotes_in, numbers_in, mapped, accumulated)) = Graph::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let (remotes, remotes_in) = b.input::<Remote>();
        let remotes = remotes.share(b);
        let mapped = remotes
            .map(move |r| {
                let transaction = r.try_transaction(move |tx| tx.send(numbers_in, 2));
                (
                    inside(r.try_send(numbers_in, 1)),
                    transaction == Err(RemoteTransactionError::InsideTransaction),
                )
            })
            .hold(b, (false, false));
        let accumulated = remotes.accumulate_mut(b, Vec::new(), move |r, log: &mut Vec<bool>| {
            log.push(inside(r.try_send(numbers_in, 3)));
        });
        b.depends(&mapped, &[&numbers]);
        (remotes_in, numbers_in, mapped, accumulated)
    });
    let remote = graph.remote();
    graph.send(remotes_in, remote.clone());
    assert_eq!(*graph.sample(mapped), (true, true), "evaluation");
    assert_eq!(*graph.sample(accumulated), [true], "commit");
    remote.send(remotes_in, remote.clone());
    graph.pump();
    assert_eq!(
        *graph.sample(accumulated),
        [true, true],
        "a unit's transaction"
    );
    assert_eq!(graph.try_pump(), Ok(()), "nothing was queued");
    graph.send(numbers_in, 4);
}

/// The panicking send from graph code panics in both builds, and the panic
/// escapes the transaction, so it poisons the graph.
#[test]
fn a_panicking_remote_send_from_graph_code_poisons_the_graph() {
    let (mut graph, (remotes_in, numbers_in, _sent)) = Graph::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let (remotes, remotes_in) = b.input::<Remote>();
        let sent = remotes.map(move |r| r.send(numbers_in, 1)).hold(b, ());
        b.depends(&sent, &[&numbers]);
        (remotes_in, numbers_in, sent)
    });
    let remote = graph.remote();
    let message = panic_message(|| graph.send(remotes_in, remote.clone()));
    assert!(
        message.contains("a remote send from graph code"),
        "{message}"
    );
    assert_eq!(graph.try_send(numbers_in, 2), Err(SendError::Poisoned));
}

/// Graph code a `construct` closure runs is guarded, and so is a child
/// instant's, a split's iterator included.
#[test]
fn construct_closures_and_child_instants_are_guarded() {
    struct Probing {
        remote: Remote,
        target: Input<u32>,
        left: u32,
    }
    impl Iterator for Probing {
        type Item = bool;
        fn next(&mut self) -> Option<bool> {
            self.left = self.left.checked_sub(1)?;
            Some(inside(self.remote.try_send(self.target, 0)))
        }
    }
    let (mut graph, (remotes_in, built, deferred, split)) = Graph::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let (remotes, remotes_in) = b.input::<Remote>();
        let remotes = remotes.share(b);
        let none = b.constant(false);
        let built = remotes
            .construct(b, move |b, r| b.constant(inside(r.try_send(numbers_in, 1))))
            .hold(b, none)
            .switch_cell(b);
        let deferred = remotes
            .defer(b)
            .map(move |r| inside(r.try_send(numbers_in, 2)))
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
    let remote = graph.remote();
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
    let (mut graph, (numbers_in, numbers, later)) = Graph::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let numbers = numbers.share(b);
        let later = numbers.defer(b);
        (numbers_in, numbers, later)
    });
    let remote = graph.remote();
    let results = Results::default();
    let now = feedback(remote.clone(), numbers_in, results.clone());
    graph.listen(numbers, now).keep();
    let in_child = feedback(remote.clone(), numbers_in, results.clone());
    graph.listen(later, in_child).keep();
    let queued = graph.transaction(|tx| {
        tx.send(numbers_in, 1);
        remote.try_send(numbers_in, 2)
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
    let (mut graph, (remotes_in, found, numbers)) = Graph::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let (remotes, remotes_in) = b.input::<Remote>();
        let found = remotes
            .map(move |remote| {
                let here = inside(remote.try_send(numbers_in, 8));
                let there = thread::spawn(move || remote.try_send(numbers_in, 7));
                (here, there.join().unwrap() == Ok(()))
            })
            .hold(b, (false, false));
        (remotes_in, found, numbers.node(b))
    });
    let heard = Rc::new(RefCell::new(Vec::new()));
    let sink = heard.clone();
    graph
        .listen(numbers, move |n| sink.borrow_mut().push(n))
        .keep();
    let remote = graph.remote();
    graph.send(remotes_in, remote);
    assert_eq!(*graph.sample(found), (true, true));
    graph.pump();
    assert_eq!(*heard.borrow(), [7]);
}

/// The guard knows its own graph's transactions only: graph code of one
/// graph sending through another graph's remote queues there.
#[test]
fn the_guard_is_per_graph() {
    let (mut other, (other_in, other_total)) = Graph::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.accumulate(b, 0u32, |n, t| t + n))
    });
    let elsewhere = other.remote();
    let (mut graph, (numbers_in, sent)) = Graph::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let sent = numbers
            .map(move |n| elsewhere.try_send(other_in, n).is_ok())
            .hold(b, false);
        (numbers_in, sent)
    });
    graph.send(numbers_in, 5);
    assert!(*graph.sample(sent), "queued in the other graph's inbox");
    other.pump();
    assert_eq!(*other.sample(other_total), 5);
}

/// Poison mirroring. A panic that escapes graph code leaves the graph
/// poisoned, and the first entry that finds it mirrors the bit into the
/// inbox; from then on every remote send and remote transaction fails,
/// from every thread, and no remote can be made. Before that entry,
/// another thread still queues, and the driver thread, whose graph code
/// never finished, is inside the transaction as far as the guard can tell.
#[test]
fn poisoning_makes_every_later_remote_send_fail() {
    let (mut graph, (numbers_in, _checked)) = Graph::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let checked = numbers
            .map(|n| {
                assert!(n != 13, "thirteen");
                n
            })
            .node(b);
        (numbers_in, checked)
    });
    let remote = graph.remote();
    remote.send(numbers_in, 1);
    let message = panic_message(|| graph.send(numbers_in, 13));
    assert!(message.contains("thirteen"), "{message}");
    let other = remote.clone();
    let before = thread::spawn(move || other.try_send(numbers_in, 2));
    assert_eq!(before.join().unwrap(), Ok(()));
    assert_eq!(
        remote.try_send(numbers_in, 3),
        Err(RemoteSendError::InsideTransaction)
    );
    assert_eq!(graph.try_pump(), Err(PumpError::Poisoned));
    assert_eq!(
        remote.try_send(numbers_in, 4),
        Err(RemoteSendError::Poisoned)
    );
    let other = remote.clone();
    let after = thread::spawn(move || other.try_send(numbers_in, 5));
    assert_eq!(after.join().unwrap(), Err(RemoteSendError::Poisoned));
    assert_eq!(
        remote.try_transaction(|_| ()),
        Err(RemoteTransactionError::Poisoned)
    );
    assert!(panic_message(|| remote.send(numbers_in, 6)).contains("poisoned"));
    assert!(panic_message(|| remote.transaction(|_| ())).contains("poisoned"));
    assert_eq!(graph.try_remote().err(), Some(PoisonedError));
    assert!(panic_message(|| graph.remote()).contains("poisoned"));
}
