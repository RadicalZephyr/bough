//! `Remote` (RFD 6): a send or a remote transaction is queued as one unit,
//! and `pump` runs each unit as one transaction, in arrival order, after
//! the slots. A unit is never split and never merged; a unit whose send
//! fails is dropped whole at `pump`, and the rest stay queued.
#![cfg(feature = "std")]

use std::any::Any;
use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, mpsc};
use std::task::{Wake, Waker};
use std::thread;
use std::time::Duration;

use bough::{
    Input, InputSlot, PumpError, Remote, RemoteSendError, RemoteTransactionError, Runtime, Source,
    Stream,
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

/// Every event a stream carries, in order.
fn log<A: Clone + 'static>(graph: &mut Runtime, stream: Stream<A>) -> Rc<RefCell<Vec<A>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    graph
        .listen(stream, move |v| sink.borrow_mut().push(v))
        .keep();
    seen
}

#[test]
fn a_remote_is_send_sync_and_clone() {
    fn assert_send_sync_clone<T: Send + Sync + Clone + 'static>() {}
    assert_send_sync_clone::<Remote>();
}

/// Two inputs, and their merge, whose function marks a simultaneous pair.
type Pair = (Input<u32>, Input<u32>, Stream<u32>);

fn pair() -> (Runtime, Pair) {
    let (graph, edge) = Runtime::build(|b| {
        let (left, left_in) = b.input::<u32>();
        let (right, right_in) = b.input::<u32>();
        let merged = left.merge(b, right, |l, r| l * 1000 + r);
        (left_in, right_in, merged)
    });
    (graph, edge.keep())
}

#[test]
fn a_remote_send_waits_for_the_pump_and_is_one_transaction() {
    let (mut graph, (left_in, right_in, merged)) = pair();
    let seen = log(&mut graph, merged);
    let remote = graph.remote();
    remote.send(left_in, 1);
    remote.send(right_in, 2);
    assert!(
        seen.borrow().is_empty(),
        "nothing runs until the driver pumps"
    );
    graph.pump();
    assert_eq!(*seen.borrow(), [1, 2], "two units are never merged");
    graph.pump();
    assert_eq!(seen.borrow().len(), 2, "a unit runs once");
}

#[test]
fn a_remote_transaction_is_one_unit_whose_sends_are_simultaneous() {
    let (mut graph, (left_in, right_in, merged)) = pair();
    let seen = log(&mut graph, merged);
    let remote = graph.remote();
    remote.transaction(move |tx| {
        tx.send(right_in, 2);
        tx.send(left_in, 1);
    });
    remote.send(left_in, 3);
    graph.pump();
    assert_eq!(
        *seen.borrow(),
        [1002, 3],
        "the merge saw both; order of sends is moot"
    );
}

/// A remote transaction may send twice to a coalescing input: the input's
/// function folds them, first send on the left, as in a local transaction.
#[test]
fn a_unit_coalesces_what_its_input_coalesces() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (words, words_in) = b.input_coalescing(|a: String, w: String| a + " " + &w);
        (words_in, words.node(b))
    });
    let (words_in, words) = edge.keep();
    let seen = log(&mut graph, words);
    graph.remote().transaction(move |tx| {
        tx.send(words_in, "one".to_string());
        tx.send(words_in, "unit".to_string());
    });
    graph.pump();
    assert_eq!(*seen.borrow(), ["one unit"]);
}

/// The slots first, each a transaction of its own in connection order, and
/// then the units in arrival order, whatever order the writes came in.
#[test]
fn slots_drain_before_units() {
    static SENSOR: InputSlot<u32> = InputSlot::keep_latest();
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        b.connect(numbers_in, &SENSOR);
        (numbers_in, numbers.node(b))
    });
    let (numbers_in, numbers) = edge.keep();
    let seen = log(&mut graph, numbers);
    let remote = graph.remote();
    remote.send(numbers_in, 1);
    remote.send(numbers_in, 2);
    SENSOR.send(3);
    graph.pump();
    assert_eq!(*seen.borrow(), [3, 1, 2]);
}

/// A remote works with a `Local` graph, whose listeners keep their `Rc`s:
/// only what crosses threads, the values, must be `Send`. Each thread's
/// units arrive in its order; units of different threads interleave by
/// arrival.
#[test]
fn a_local_graph_takes_units_from_several_threads() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<(u32, u32)>();
        (numbers_in, numbers.node(b))
    });
    let (numbers_in, numbers) = edge.keep();
    let seen = log(&mut graph, numbers);
    let remote = graph.remote();
    let senders: Vec<_> = (0..4u32)
        .map(|k| {
            let remote = remote.clone();
            thread::spawn(move || {
                for n in 0..50 {
                    remote.send(numbers_in, (k, n));
                }
            })
        })
        .collect();
    for sender in senders {
        sender.join().unwrap();
    }
    graph.pump();
    let seen = seen.borrow();
    assert_eq!(seen.len(), 200);
    for k in 0..4 {
        let mine: Vec<u32> = seen.iter().filter(|(t, _)| *t == k).map(|e| e.1).collect();
        assert_eq!(mine, (0..50).collect::<Vec<_>>(), "thread {k}'s order kept");
    }
}

/// A remote send never waits for the graph: another thread sends while the
/// driver is inside a transaction, here held in a listener until the send
/// has returned.
#[test]
fn a_remote_send_never_blocks_on_the_graph() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.node(b))
    });
    let (numbers_in, numbers) = edge.keep();
    let remote = graph.remote();
    let (sent, has_sent) = mpsc::channel::<()>();
    let start = Arc::new(Barrier::new(2));
    let other = {
        let (remote, start) = (remote.clone(), start.clone());
        thread::spawn(move || {
            start.wait();
            remote.send(numbers_in, 2);
            sent.send(()).unwrap();
        })
    };
    let heard = Rc::new(RefCell::new(Vec::new()));
    let sink = heard.clone();
    graph
        .listen(numbers, move |v| {
            if v == 1 {
                start.wait();
                has_sent
                    .recv_timeout(Duration::from_secs(10))
                    .expect("the send returned while the driver was in a transaction");
            }
            sink.borrow_mut().push(v);
        })
        .keep();
    remote.send(numbers_in, 1);
    graph.pump();
    other.join().unwrap();
    graph.pump();
    assert_eq!(*heard.borrow(), [1, 2]);
}

/// A remote send from a listener is the sanctioned feedback path: it
/// queues a later transaction, never a nested one, and the next pump runs
/// it. The listener runs while the pump holds no lock.
#[test]
fn a_remote_send_from_a_listener_is_a_later_transaction() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.node(b))
    });
    let (numbers_in, numbers) = edge.keep();
    let remote = graph.remote();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    graph
        .listen(numbers, move |v| {
            sink.borrow_mut().push(v);
            if v < 3 {
                remote.send(numbers_in, v + 1);
            }
        })
        .keep();
    graph.send(numbers_in, 1);
    assert_eq!(*seen.borrow(), [1], "queued, not nested");
    graph.pump();
    assert_eq!(
        *seen.borrow(),
        [1, 2],
        "one pump runs what was queued before it"
    );
    graph.pump();
    assert_eq!(*seen.borrow(), [1, 2, 3]);
    graph.pump();
    assert_eq!(seen.borrow().len(), 3);
}

/// The closure of a remote transaction runs on the driver, and is I/O code
/// there: a remote send from it queues a later unit.
#[test]
fn a_remote_transaction_runs_on_the_driver() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.node(b))
    });
    let (numbers_in, numbers) = edge.keep();
    let seen = log(&mut graph, numbers);
    let remote = graph.remote();
    let (ran_on, ran) = mpsc::channel();
    let again = remote.clone();
    thread::spawn(move || {
        remote.transaction(move |tx| {
            ran_on.send(thread::current().id()).unwrap();
            tx.send(numbers_in, 1);
            again.send(numbers_in, 2);
        });
    })
    .join()
    .unwrap();
    assert!(ran.try_recv().is_err(), "queued, not run on the sender");
    graph.pump();
    assert_eq!(ran.try_recv().unwrap(), thread::current().id());
    assert_eq!(*seen.borrow(), [1]);
    graph.pump();
    assert_eq!(*seen.borrow(), [1, 2]);
}

#[derive(Default)]
struct Counter(AtomicUsize);

impl Wake for Counter {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn a_remote_send_wakes_the_waker_the_driver_registered() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.hold(b, 0u32))
    });
    let (numbers_in, _numbers) = edge.keep();
    let remote = graph.remote();
    remote.send(numbers_in, 1);
    let counter = Arc::new(Counter::default());
    graph.set_waker(Waker::from(counter.clone()));
    assert_eq!(counter.0.load(Ordering::SeqCst), 0, "no waker then");
    remote.send(numbers_in, 2);
    remote.transaction(move |tx| tx.send(numbers_in, 3));
    assert_eq!(counter.0.load(Ordering::SeqCst), 2);
}

/// A double send inside a unit is an error in both builds: `try_pump`
/// reports it and drops the unit whole, none of its sends run, and the
/// units behind it stay queued; the panicking pump panics and leaves the
/// graph usable.
#[test]
fn a_double_send_inside_a_unit_drops_the_unit_whole() {
    let (mut graph, (left_in, right_in, merged)) = pair();
    let seen = log(&mut graph, merged);
    let remote = graph.remote();
    remote.transaction(move |tx| {
        tx.send(right_in, 1);
        tx.send(left_in, 2);
        tx.send(left_in, 3);
    });
    remote.send(left_in, 4);
    assert_eq!(graph.try_pump(), Err(PumpError::DoubleSend));
    assert!(seen.borrow().is_empty(), "no send of the unit ran");
    assert_eq!(graph.try_pump(), Ok(()));
    assert_eq!(*seen.borrow(), [4], "the rest stayed queued");
    remote.transaction(move |tx| {
        tx.send(right_in, 5);
        tx.send(right_in, 6);
    });
    remote.send(right_in, 7);
    let message = panic_message(|| graph.pump());
    assert!(message.contains("a second send"), "{message}");
    graph.pump();
    assert_eq!(*seen.borrow(), [4, 7], "usable, and the rest ran next");
}

/// A token from another graph: a single remote send is refused when it is
/// queued, since the remote knows its graph; a remote transaction's closure
/// runs on the driver, so its foreign token is found at `pump`.
#[test]
fn a_foreign_token_is_refused_when_queued_or_at_pump() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.node(b))
    });
    let (numbers_in, numbers) = edge.keep();
    let (_other, edge) = Runtime::build(|b| b.input::<u32>().1);
    let foreign_in = edge.keep();
    let seen = log(&mut graph, numbers);
    let remote = graph.remote();
    assert_eq!(
        remote.try_send(foreign_in, 1),
        Err(RemoteSendError::ForeignGraph)
    );
    assert!(panic_message(|| remote.send(foreign_in, 1)).contains("another graph"));
    remote.transaction(move |tx| {
        tx.send(numbers_in, 1);
        tx.send(foreign_in, 2);
    });
    remote.send(numbers_in, 3);
    assert_eq!(graph.try_pump(), Err(PumpError::ForeignGraph));
    assert!(seen.borrow().is_empty());
    remote.transaction(move |tx| tx.send(foreign_in, 4));
    let message = panic_message(|| graph.pump());
    assert!(message.contains("another graph"), "{message}");
    graph.pump();
    assert_eq!(*seen.borrow(), [3], "usable, and the rest ran");
}

/// RFD 5: a remote send whose input was collected before the driver pumps
/// is found at `pump`. `try_pump` reports `Stale` and drops the unit whole;
/// the panicking pump panics in a debug build and, in a release build,
/// counts the send and runs the unit's other sends, since the stale send
/// alone is what the semantics cannot observe.
#[test]
fn a_remote_send_to_an_input_collected_before_the_pump_is_stale() {
    // The lost input's token leaves the build closure by a side door, so
    // that the return value does not root it.
    let mut lost = None;
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let _unrooted = numbers.hold(b, 0u32);
        lost = Some(numbers_in);
        let (kept, kept_in) = b.input::<u32>();
        (kept_in, kept.node(b))
    });
    let (kept_in, kept) = edge.keep();
    let lost_in = lost.unwrap();
    let seen = log(&mut graph, kept);
    let remote = graph.remote();
    // Queued before any transaction; the pump's first collects the input.
    remote.send(lost_in, 1);
    remote.transaction(move |tx| {
        tx.send(kept_in, 2);
        tx.send(lost_in, 3);
    });
    remote.send(kept_in, 4);
    assert_eq!(graph.try_pump(), Err(PumpError::Stale));
    assert_eq!(graph.try_pump(), Err(PumpError::Stale));
    assert!(seen.borrow().is_empty(), "the whole unit was dropped");
    assert_eq!(graph.try_pump(), Ok(()));
    assert_eq!(*seen.borrow(), [4]);
    remote.transaction(move |tx| {
        tx.send(kept_in, 5);
        tx.send(lost_in, 6);
    });
    remote.send(kept_in, 7);
    if cfg!(debug_assertions) {
        let message = panic_message(|| graph.pump());
        assert!(message.contains("a send to a collected input"), "{message}");
        assert_eq!(graph.stale_operations(), 0);
        graph.pump();
        assert_eq!(*seen.borrow(), [4, 7]);
    } else {
        graph.pump();
        assert_eq!(graph.stale_operations(), 1);
        assert_eq!(*seen.borrow(), [4, 5, 7]);
    }
}

/// A dropped graph closes its inbox: what was queued is dropped with it,
/// and a remote send is refused rather than queued where nothing drains.
#[test]
fn a_dropped_graph_refuses_remote_sends() {
    struct Counted(Arc<AtomicUsize>);
    impl Drop for Counted {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let drops = Arc::new(AtomicUsize::new(0));
    let (graph, edge) = Runtime::build(|b| b.input::<Counted>().1);
    let things_in = edge.keep();
    let remote = graph.remote();
    remote.send(things_in, Counted(drops.clone()));
    drop(graph);
    assert_eq!(
        drops.load(Ordering::SeqCst),
        1,
        "the queued unit was dropped"
    );
    assert_eq!(
        remote.try_send(things_in, Counted(drops.clone())).err(),
        Some(RemoteSendError::GraphDropped)
    );
    assert_eq!(
        remote.try_transaction(|_| ()),
        Err(RemoteTransactionError::GraphDropped)
    );
    if cfg!(debug_assertions) {
        let message = panic_message(|| remote.send(things_in, Counted(drops.clone())));
        assert!(message.contains("a dropped graph"), "{message}");
    } else {
        remote.send(things_in, Counted(drops.clone()));
        remote.transaction(|_| ());
    }
    assert_eq!(
        drops.load(Ordering::SeqCst),
        3,
        "refused values are dropped"
    );
}

/// A panic in a remote transaction's closure, or in the transaction a unit
/// runs, escapes the transaction and poisons the graph, as through `send`.
#[test]
fn a_panic_in_a_unit_poisons_the_graph() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.node(b))
    });
    let (numbers_in, _numbers) = edge.keep();
    let remote = graph.remote();
    remote.transaction(move |tx| {
        tx.send(numbers_in, 1);
        panic!("a remote transaction's closure panicked");
    });
    remote.send(numbers_in, 2);
    let message = panic_message(|| graph.pump());
    assert!(message.contains("closure panicked"), "{message}");
    assert_eq!(graph.try_pump(), Err(PumpError::Poisoned));
}
