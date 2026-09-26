//! `RemoteIo` (RFD 6): a send or a remote transaction is queued as one unit,
//! and `pump` runs each unit as one transaction, in arrival order, after
//! the slots. A unit is never split and never merged; a unit whose send
//! fails is dropped whole at `pump`, and the rest stay queued.
#![cfg(feature = "std")]

use std::any::Any;
use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex, mpsc};
use std::task::{Wake, Waker};
use std::thread;
use std::time::Duration;

use bough::{
    Cell, Input, InputSlot, IoError, PumpError, RemoteIo, Runtime, SendError, Source, Stream,
    TokenError,
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
    assert_send_sync_clone::<RemoteIo>();
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
    let remote = graph.remote_io();
    remote.send(left_in, 1).unwrap();
    remote.send(right_in, 2).unwrap();
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
    let remote = graph.remote_io();
    remote
        .transaction(move |tx| {
            tx.send(right_in, 2);
            tx.send(left_in, 1);
        })
        .unwrap();
    remote.send(left_in, 3).unwrap();
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
    graph
        .remote_io()
        .transaction(move |tx| {
            tx.send(words_in, "one".to_string());
            tx.send(words_in, "unit".to_string());
        })
        .unwrap();
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
    let remote = graph.remote_io();
    remote.send(numbers_in, 1).unwrap();
    remote.send(numbers_in, 2).unwrap();
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
    let remote = graph.remote_io();
    let senders: Vec<_> = (0..4u32)
        .map(|k| {
            let remote = remote.clone();
            thread::spawn(move || {
                for n in 0..50 {
                    remote.send(numbers_in, (k, n)).unwrap();
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
    let remote = graph.remote_io();
    let (sent, has_sent) = mpsc::channel::<()>();
    let start = Arc::new(Barrier::new(2));
    let other = {
        let (remote, start) = (remote.clone(), start.clone());
        thread::spawn(move || {
            start.wait();
            remote.send(numbers_in, 2).unwrap();
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
    remote.send(numbers_in, 1).unwrap();
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
    let remote = graph.remote_io();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    graph
        .listen(numbers, move |v| {
            sink.borrow_mut().push(v);
            if v < 3 {
                remote.send(numbers_in, v + 1).unwrap();
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
    let remote = graph.remote_io();
    let (ran_on, ran) = mpsc::channel();
    let again = remote.clone();
    thread::spawn(move || {
        remote
            .transaction(move |tx| {
                ran_on.send(thread::current().id()).unwrap();
                tx.send(numbers_in, 1);
                again.send(numbers_in, 2).unwrap();
            })
            .unwrap();
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

/// A remote call wakes the waker the driver registered, unless another
/// has since the last pump began: one wake per burst. A call made before
/// any waker was registered wakes nothing.
#[test]
fn a_remote_call_wakes_the_driver_once_per_burst() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.hold(b, 0u32))
    });
    let (numbers_in, _numbers) = edge.keep();
    let remote = graph.remote_io();
    remote.send(numbers_in, 1).unwrap();
    let counter = Arc::new(Counter::default());
    let wakes = || counter.0.load(Ordering::SeqCst);
    graph.set_waker(Waker::from(counter.clone()));
    assert_eq!(wakes(), 0, "no waker then");
    remote.send(numbers_in, 2).unwrap();
    remote
        .transaction(move |tx| tx.send(numbers_in, 3))
        .unwrap();
    assert_eq!(wakes(), 1, "one burst, one wake");
    graph.pump();
    let other = remote.clone();
    thread::spawn(move || other.send(numbers_in, 4).unwrap())
        .join()
        .unwrap();
    remote.send(numbers_in, 5).unwrap();
    assert_eq!(wakes(), 2, "the pump let the next call wake");
    let replacement = Arc::new(Counter::default());
    graph.set_waker(Waker::from(replacement.clone()));
    remote.send(numbers_in, 6).unwrap();
    assert_eq!(
        replacement.0.load(Ordering::SeqCst),
        1,
        "a new waker gets the next call's wake"
    );
}

/// Both handles' calls run in one order, the order they were made, however
/// they interleave: here an `Io` send, a remote send from a thread joined
/// before the next call, a remote listener, and an `Io` send, which the
/// listener registered before it hears.
#[test]
fn calls_through_both_handles_run_in_the_order_they_were_made() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.share(b))
    });
    let (numbers_in, numbers) = edge.keep();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    graph
        .listen(numbers, move |n| sink.borrow_mut().push(n))
        .keep();
    let io = graph.io();
    let remote = graph.remote_io();
    let (heard, hearing) = mpsc::channel();
    io.send(numbers_in, 1).unwrap();
    let listener = thread::spawn(move || {
        remote.send(numbers_in, 2).unwrap();
        remote
            .listen(numbers, move |n| heard.send(n).unwrap())
            .unwrap()
    })
    .join()
    .unwrap();
    io.send(numbers_in, 3).unwrap();
    graph.pump();
    assert_eq!(*seen.borrow(), [1, 2, 3]);
    assert_eq!(hearing.try_iter().collect::<Vec<_>>(), [3]);
    drop(listener);
}

/// A remote call made after the pump began waits for the next pump, even
/// one a slot's listener makes before the pump reaches the calls.
#[test]
fn a_remote_call_made_after_the_pump_began_waits_for_the_next_pump() {
    static TICKS: InputSlot<u32> = InputSlot::keep_latest();
    let (mut graph, edge) = Runtime::build(|b| {
        let (ticks, ticks_in) = b.input::<u32>();
        b.connect(ticks_in, &TICKS);
        let (echoes, echoes_in) = b.input::<u32>();
        (ticks, echoes_in, echoes.hold(b, 0u32))
    });
    let (ticks, echoes_in, echoes) = edge.keep();
    let remote = graph.remote_io();
    graph
        .listen(ticks, move |t| remote.send(echoes_in, t).unwrap())
        .keep();
    TICKS.send(7);
    graph.pump();
    assert_eq!(*graph.sample(echoes), 0, "made after the pump began");
    graph.pump();
    assert_eq!(*graph.sample(echoes), 7);
}

/// A double send inside a unit is an error in both builds: `try_pump`
/// reports it and drops the unit whole, none of its sends run, and the
/// units behind it stay queued; the panicking pump panics and leaves the
/// graph usable.
#[test]
fn a_double_send_inside_a_unit_drops_the_unit_whole() {
    let (mut graph, (left_in, right_in, merged)) = pair();
    let seen = log(&mut graph, merged);
    let remote = graph.remote_io();
    remote
        .transaction(move |tx| {
            tx.send(right_in, 1);
            tx.send(left_in, 2);
            tx.send(left_in, 3);
        })
        .unwrap();
    remote.send(left_in, 4).unwrap();
    assert_eq!(graph.try_pump(), Err(PumpError::DoubleSend));
    assert!(seen.borrow().is_empty(), "no send of the unit ran");
    assert_eq!(graph.try_pump(), Ok(()));
    assert_eq!(*seen.borrow(), [4], "the rest stayed queued");
    remote
        .transaction(move |tx| {
            tx.send(right_in, 5);
            tx.send(right_in, 6);
        })
        .unwrap();
    remote.send(right_in, 7).unwrap();
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
    let remote = graph.remote_io();
    assert_eq!(remote.send(foreign_in, 1), Err(IoError::ForeignGraph));
    remote
        .transaction(move |tx| {
            tx.send(numbers_in, 1);
            tx.send(foreign_in, 2);
        })
        .unwrap();
    remote.send(numbers_in, 3).unwrap();
    assert_eq!(graph.try_pump(), Err(PumpError::ForeignGraph));
    assert!(seen.borrow().is_empty());
    remote
        .transaction(move |tx| tx.send(foreign_in, 4))
        .unwrap();
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
    let remote = graph.remote_io();
    // Nothing roots the lost input, so the build's collection freed it.
    remote.send(lost_in, 1).unwrap();
    remote
        .transaction(move |tx| {
            tx.send(kept_in, 2);
            tx.send(lost_in, 3);
        })
        .unwrap();
    remote.send(kept_in, 4).unwrap();
    assert_eq!(graph.try_pump(), Err(PumpError::Stale));
    assert_eq!(graph.try_pump(), Err(PumpError::Stale));
    assert!(seen.borrow().is_empty(), "the whole unit was dropped");
    assert_eq!(graph.try_pump(), Ok(()));
    assert_eq!(*seen.borrow(), [4]);
    remote
        .transaction(move |tx| {
            tx.send(kept_in, 5);
            tx.send(lost_in, 6);
        })
        .unwrap();
    remote.send(kept_in, 7).unwrap();
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
    let remote = graph.remote_io();
    remote.send(things_in, Counted(drops.clone())).unwrap();
    drop(graph);
    assert_eq!(
        drops.load(Ordering::SeqCst),
        1,
        "the queued unit was dropped"
    );
    assert_eq!(
        remote.send(things_in, Counted(drops.clone())).err(),
        Some(IoError::Gone)
    );
    assert_eq!(remote.transaction(|_| ()), Err(IoError::Gone));
    assert_eq!(
        drops.load(Ordering::SeqCst),
        2,
        "a refused value is dropped"
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
    let remote = graph.remote_io();
    remote
        .transaction(move |tx| {
            tx.send(numbers_in, 1);
            panic!("a remote transaction's closure panicked");
        })
        .unwrap();
    remote.send(numbers_in, 2).unwrap();
    let message = panic_message(|| graph.pump());
    assert!(message.contains("closure panicked"), "{message}");
    assert_eq!(graph.try_pump(), Err(PumpError::Poisoned));
}

/// A listener registered from another thread runs on the driver's thread,
/// from the pump that registers it. One whose guard was dropped before
/// that pump is never registered.
#[test]
fn a_listener_registered_from_another_thread_runs_on_the_driver() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.share(b))
    });
    let (numbers_in, numbers) = edge.keep();
    let remote = graph.remote_io();
    let (heard, hearing) = mpsc::channel();
    let listener = thread::spawn(move || {
        let kept = remote
            .listen(numbers, move |n| {
                heard.send((n, thread::current().id())).unwrap()
            })
            .unwrap();
        drop(
            remote
                .listen(numbers, |_| panic!("a cancelled listener ran"))
                .unwrap(),
        );
        remote.send(numbers_in, 1).unwrap();
        kept
    })
    .join()
    .unwrap();
    graph.send(numbers_in, 0);
    assert!(
        hearing.try_recv().is_err(),
        "nothing registered before the pump"
    );
    graph.pump();
    assert_eq!(hearing.try_recv(), Ok((1, thread::current().id())));
    drop(listener);
    graph.send(numbers_in, 2);
    assert!(hearing.try_recv().is_err(), "dropping the guard unlistens");
}

/// A remote `listen_cell` makes its first call at the pump, with the value
/// then; a remote `listen_steps` makes none.
#[test]
fn a_remote_listen_cell_fires_at_the_pump_with_the_value_then() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.hold(b, 0u32))
    });
    let (numbers_in, latest) = edge.keep();
    let remote = graph.remote_io();
    let (cells, cells_heard) = mpsc::channel();
    let (steps, steps_heard) = mpsc::channel();
    thread::spawn(move || {
        remote
            .listen_cell(latest, move |n| cells.send(*n).unwrap())
            .unwrap()
            .keep();
        remote
            .listen_steps(latest, move |n| steps.send(*n).unwrap())
            .unwrap()
            .keep();
    })
    .join()
    .unwrap();
    graph.send(numbers_in, 5);
    graph.pump();
    graph.send(numbers_in, 6);
    assert_eq!(cells_heard.try_iter().collect::<Vec<_>>(), [5, 6]);
    assert_eq!(steps_heard.try_iter().collect::<Vec<_>>(), [6]);
}

/// A remote anchor keeps its value alive until the `Anchored` drops, and
/// the `Anchored` can come back from the thread that asked for it.
#[test]
fn a_remote_anchor_keeps_its_value_alive_until_it_drops() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.hold(b, 0u32))
    });
    let (numbers_in, latest) = *edge;
    let remote = graph.remote_io();
    let kept = thread::spawn(move || remote.anchor((numbers_in, latest)).unwrap())
        .join()
        .unwrap();
    graph.pump();
    drop(edge);
    graph.collect_garbage();
    graph.send(kept.0, 3);
    assert_eq!(*graph.sample(kept.1), 3, "the remote anchor kept both");
    drop(kept);
    graph.collect_garbage();
    assert_eq!(graph.try_sample(latest).err(), Some(TokenError::Stale));
}

/// A registration's token from another graph is refused when it's queued;
/// a stale one is found at the pump.
#[test]
fn a_remote_registrations_foreign_token_is_refused_and_a_stale_one_found_at_the_pump() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.hold(b, 0u32))
    });
    let (numbers_in, latest) = *edge;
    let _input = graph.anchor(numbers_in);
    drop(edge);
    graph.collect_garbage();
    let (_other, other_edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.hold(b, 0u32))
    });
    let (_foreign_in, foreign) = other_edge.keep();
    let remote = graph.remote_io();
    assert_eq!(
        remote.listen_steps(foreign, |_| ()).err(),
        Some(IoError::ForeignGraph)
    );
    assert_eq!(remote.anchor(foreign).err(), Some(IoError::ForeignGraph));
    let _listener = remote.listen_steps(latest, |_| ()).unwrap();
    assert_eq!(graph.try_pump(), Err(PumpError::Stale));
}

/// A `Threaded` runtime has only the `RemoteIo`. It registers through one,
/// moves to another thread, and its listener runs there at the pump.
#[test]
fn a_threaded_runtime_registers_through_remote_io_and_pumps_elsewhere() {
    let (mut graph, edge) = Runtime::build_threaded(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.accumulate(b, 0u32, |n, t| t + n))
    });
    let (numbers_in, total) = edge.keep();
    let remote = graph.remote_io();
    let (report, reports) = mpsc::channel();
    remote
        .listen_steps(total, move |t| {
            report.send((*t, thread::current().id())).unwrap()
        })
        .unwrap()
        .keep();
    remote.send(numbers_in, 1).unwrap();
    let driver = thread::spawn(move || {
        graph.pump();
        thread::current().id()
    });
    let driver = driver.join().unwrap();
    assert_eq!(reports.try_recv(), Ok((1, driver)));
}

/// A row a construct sends out plain: its input, and the count of what
/// was sent to it, starting at the event that opened it.
type Row = (Input<u32>, Cell<u32>);

/// Opens a row in a `Threaded` runtime, hands it to `on_row` in a listener,
/// with a `RemoteIo`, and returns it once its unit has run. The runtime
/// collects after every transaction, so a row nothing keeps is gone by then.
fn open_a_row(
    mut on_row: impl FnMut(&RemoteIo, Row) + Send + 'static,
) -> (Runtime<bough::Threaded>, Row) {
    let (mut graph, edge) = Runtime::build_threaded(|b| {
        let (open, open_in) = b.input::<u32>();
        let rows = open.construct(b, |b, start| {
            let (bumps, bumps_in) = b.input::<u32>();
            (bumps_in, bumps.accumulate(b, start, |n, c| c + n))
        });
        (open_in, rows)
    });
    let (open_in, rows) = edge.keep();
    graph.set_collect_after_every_transaction(true);
    let remote = graph.remote_io();
    let seen = Arc::new(Mutex::new(None));
    let sink = seen.clone();
    graph
        .listen(rows, move |row: Row| {
            on_row(&remote, row);
            *sink.lock().unwrap() = Some(row);
        })
        .keep();
    graph.send(open_in, 10);
    let row = seen.lock().unwrap().expect("the listener saw a row");
    (graph, row)
}

/// Waiting remote calls keep what they name alive, as an `Io`'s do: a
/// listener handed a plain row queues a `listen_cell` on its count, and
/// the row lasts through the collection after its unit until the pump,
/// where the listener keeps it.
#[test]
fn a_waiting_remote_listen_keeps_a_plain_row_alive_until_the_pump() {
    let counts = Arc::new(Mutex::new(Vec::new()));
    let sink = counts.clone();
    let (mut graph, (bumps_in, count)) = open_a_row(move |remote, (_, count)| {
        let sink = sink.clone();
        remote
            .listen_cell(count, move |c| sink.lock().unwrap().push(*c))
            .unwrap()
            .keep();
    });
    assert_eq!(
        *graph.sample(count),
        10,
        "alive after its unit's collection"
    );
    graph.pump();
    graph.send(bumps_in, 5);
    assert_eq!(
        *counts.lock().unwrap(),
        [10, 15],
        "the listener keeps it now"
    );
}

/// The same with a remote anchor of the whole row.
#[test]
fn a_waiting_remote_anchor_keeps_a_plain_row_alive_until_the_pump() {
    let (mut graph, (bumps_in, count)) = open_a_row(|remote, row| {
        remote.anchor(row).unwrap().keep();
    });
    assert_eq!(
        *graph.sample(count),
        10,
        "alive after its unit's collection"
    );
    graph.pump();
    graph.send(bumps_in, 5);
    assert_eq!(*graph.sample(count), 15, "the anchor keeps it now");
}

/// A waiting remote send keeps nothing alive, as an `Io`'s doesn't, and
/// neither does a registration whose guard has gone. The row's input and
/// count are gone after its unit, and the pump reports the send as stale.
#[test]
fn a_waiting_remote_send_or_cancelled_registration_keeps_nothing_alive() {
    let (mut graph, (bumps_in, count)) = open_a_row(|remote, (bumps_in, count)| {
        remote.send(bumps_in, 5).unwrap();
        drop(remote.listen_cell(count, |_| ()).unwrap());
    });
    assert_eq!(graph.try_sample(count).err(), Some(TokenError::Stale));
    assert_eq!(graph.try_send(bumps_in, 1), Err(SendError::Stale));
    assert_eq!(graph.try_pump(), Err(PumpError::Stale));
}
