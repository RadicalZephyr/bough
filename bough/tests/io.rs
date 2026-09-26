//! The `Io` (RFD 6, RFD 7): a handle for I/O code that can't hold the
//! runtime. Every call queues for the driver's next pump, which runs the
//! calls after the slots and the remote units, in the order they were
//! made, taking only those made before it began.

use std::any::Any;
use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Wake, Waker};

use bough::{Input, Io, IoError, PumpError, Runtime, SendError, Source, Stream};

/// The message of a caught panic.
fn panic_text(result: Result<impl Sized, Box<dyn Any + Send>>) -> String {
    let payload = match result {
        Ok(_) => panic!("expected a panic"),
        Err(payload) => payload,
    };
    if let Some(text) = payload.downcast_ref::<&str>() {
        text.to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
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

/// Test 1.
#[test]
fn a_call_waits_for_the_next_pump() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.hold(b, 0u32))
    });
    let (numbers_in, latest) = edge.keep();
    let io = graph.io();
    io.send(numbers_in, 1).unwrap();
    assert_eq!(*graph.sample(latest), 0, "a call only queues");
    graph.send(numbers_in, 2);
    assert_eq!(*graph.sample(latest), 2, "the runtime's own calls run now");
    graph.pump();
    assert_eq!(*graph.sample(latest), 1, "the pump ran the queued send");
}

/// Test 3, with the pump's own units: each pump runs one step of a
/// listener that always sends, so the pump returns.
#[test]
fn a_call_a_listener_makes_during_a_pump_waits_for_the_next_one() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let numbers = numbers.share(b);
        (numbers_in, numbers, numbers.hold(b, 0u32))
    });
    let (numbers_in, numbers, latest) = edge.keep();
    let io = graph.io();
    let feedback = io.clone();
    graph
        .listen(numbers, move |n| feedback.send(numbers_in, n + 1).unwrap())
        .keep();
    io.send(numbers_in, 0).unwrap();
    for step in 0..3 {
        graph.pump();
        assert_eq!(*graph.sample(latest), step);
    }
}

/// Test 3, with a remote unit: a call a listener makes while the pump runs
/// the units, before it reaches the `Io`'s calls, still waits.
#[cfg(feature = "std")]
#[test]
fn a_call_made_while_the_pump_runs_the_units_waits_for_the_next_pump() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (first, first_in) = b.input::<u32>();
        let (second, second_in) = b.input::<u32>();
        let first = first.share(b);
        (first_in, second_in, first, second.hold(b, 0u32))
    });
    let (first_in, second_in, first, second) = edge.keep();
    let io = graph.io();
    graph
        .listen(first, move |n| io.send(second_in, n).unwrap())
        .keep();
    graph.remote().send(first_in, 7);
    graph.pump();
    assert_eq!(*graph.sample(second), 0, "made after the pump began");
    graph.pump();
    assert_eq!(*graph.sample(second), 7);
}

/// Test 5.
#[test]
fn a_queued_transactions_sends_are_simultaneous() {
    let (mut graph, (left_in, right_in, merged)) = pair();
    let seen = log(&mut graph, merged);
    let io = graph.io();
    io.transaction(move |tx| {
        tx.send(left_in, 1);
        tx.send(right_in, 2);
    })
    .unwrap();
    io.send(left_in, 3).unwrap();
    io.send(right_in, 4).unwrap();
    graph.pump();
    assert_eq!(
        *seen.borrow(),
        [1002, 3, 4],
        "one unit, then two, in the order they were made"
    );
}

/// A queued unit whose send fails is dropped whole, as a remote's is:
/// `try_pump` returns the error, none of the unit's sends run, and the
/// calls behind it stay queued. The panicking pump panics and leaves the
/// runtime usable.
#[test]
fn a_queued_unit_whose_send_fails_is_dropped_whole() {
    let (mut graph, (left_in, right_in, merged)) = pair();
    let seen = log(&mut graph, merged);
    let io = graph.io();
    let double = move |tx: &mut bough::IoTransaction<'_>| {
        tx.send(right_in, 1);
        tx.send(left_in, 2);
        tx.send(left_in, 3);
    };
    io.transaction(double).unwrap();
    io.send(left_in, 4).unwrap();
    assert_eq!(graph.try_pump(), Err(PumpError::DoubleSend));
    assert!(seen.borrow().is_empty(), "no send of the unit ran");
    assert_eq!(graph.try_pump(), Ok(()));
    assert_eq!(*seen.borrow(), [4], "the rest stayed queued");

    io.transaction(double).unwrap();
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| graph.pump())));
    assert!(text.contains("a second send"), "{text}");
    io.send(left_in, 5).unwrap();
    graph.pump();
    assert_eq!(*seen.borrow(), [4, 5], "the runtime stayed usable");
}

/// A stale or foreign token is graph knowledge, found at the pump:
/// `try_pump` returns it; `pump` panics on a foreign token in both builds,
/// and on a stale one in a debug build, where a release build counts it.
#[test]
fn a_stale_or_foreign_token_is_found_at_the_pump() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (kept, kept_in) = b.input::<u32>();
        let (_lost, lost_in) = b.input::<u32>();
        (kept_in, lost_in, kept.hold(b, 0u32))
    });
    let (kept_in, lost_in, latest) = *edge;
    let _kept = graph.anchor((kept_in, latest));
    drop(edge);
    graph.collect_garbage();
    let (_other, other_edge) = Runtime::build(|b| b.input::<u32>().1);
    let foreign_in = other_edge.keep();
    let io = graph.io();

    io.send(lost_in, 1).unwrap();
    io.send(kept_in, 2).unwrap();
    assert_eq!(graph.try_pump(), Err(PumpError::Stale));
    assert_eq!(graph.try_pump(), Ok(()));
    assert_eq!(*graph.sample(latest), 2, "the call behind it ran");

    io.send(foreign_in, 3).unwrap();
    assert_eq!(graph.try_pump(), Err(PumpError::ForeignGraph));
    io.send(foreign_in, 3).unwrap();
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| graph.pump())));
    assert!(text.contains("another graph"), "{text}");

    io.send(lost_in, 4).unwrap();
    if cfg!(debug_assertions) {
        let text = panic_text(catch_unwind(AssertUnwindSafe(|| graph.pump())));
        assert!(text.contains("a send to a collected input"), "{text}");
    } else {
        graph.pump();
        assert_eq!(graph.stale_operations(), 1);
    }
}

/// Test 6: a call from graph code is refused, and a listener's call
/// queues. The `Io` reaches the `map` function through a cell, since it
/// exists only once the build has returned.
#[test]
fn a_call_from_graph_code_is_refused() {
    let io_cell: Rc<RefCell<Option<Io>>> = Rc::new(RefCell::new(None));
    let refused = Rc::new(RefCell::new(Vec::new()));
    let (mut graph, edge) = Runtime::build({
        let io_cell = io_cell.clone();
        let refused = refused.clone();
        move |b| {
            let (numbers, numbers_in) = b.input::<u32>();
            let (echoes, echoes_in) = b.input::<u32>();
            let mapped = numbers
                .map(move |n| {
                    if let Some(io) = &*io_cell.borrow() {
                        refused.borrow_mut().push(io.send(echoes_in, n));
                    }
                    n
                })
                .hold(b, 0u32);
            (numbers_in, echoes_in, mapped, echoes.hold(b, 0u32))
        }
    });
    let (numbers_in, echoes_in, mapped, echoes) = edge.keep();
    *io_cell.borrow_mut() = Some(graph.io());
    let io = graph.io();
    graph
        .listen_steps(mapped, move |n| io.send(echoes_in, n * 10).unwrap())
        .keep();
    graph.send(numbers_in, 1);
    assert_eq!(*refused.borrow(), [Err(IoError::FromGraphCode)]);
    assert_eq!(*graph.sample(echoes), 0, "the listener's call waits");
    graph.pump();
    assert_eq!(*graph.sample(echoes), 10);
}

/// Test 6: a call after the runtime drops reports `Gone`, and the calls it
/// left waiting are dropped with it.
#[test]
fn a_call_after_the_runtime_drops_is_gone() {
    let (graph, edge) = Runtime::build(|b| b.input::<Rc<u32>>().1);
    let numbers_in = edge.keep();
    let io = graph.io();
    let value = Rc::new(1u32);
    io.send(numbers_in, value.clone()).unwrap();
    assert_eq!(Rc::strong_count(&value), 2);
    drop(graph);
    assert_eq!(Rc::strong_count(&value), 1, "the waiting call was dropped");
    assert_eq!(io.send(numbers_in, value), Err(IoError::Gone));
}

/// Test 6: a panic that escapes graph code leaves the graph-code flag set,
/// so a call reports `FromGraphCode` until an entry on the runtime finds
/// the poison, and `Poisoned` from then on.
#[test]
fn a_call_after_an_entry_finds_the_poison_is_poisoned() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let checked = numbers.map(|n: u32| {
            assert_ne!(n, 13, "unlucky");
            n
        });
        (numbers_in, checked.hold(b, 0u32))
    });
    let (numbers_in, _checked) = edge.keep();
    let io = graph.io();
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| {
        graph.send(numbers_in, 13)
    })));
    assert!(text.contains("unlucky"), "{text}");
    assert_eq!(io.send(numbers_in, 1), Err(IoError::FromGraphCode));
    assert_eq!(graph.try_send(numbers_in, 1), Err(SendError::Poisoned));
    assert_eq!(io.send(numbers_in, 1), Err(IoError::Poisoned));
    assert_eq!(graph.try_pump(), Err(PumpError::Poisoned));
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

/// Test 7: one wake covers every call the next pump will run, and a call
/// made while a pump runs wakes the driver for the pump after.
#[test]
fn queuing_wakes_the_driver_once_per_burst() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (first, first_in) = b.input::<u32>();
        let (second, second_in) = b.input::<u32>();
        let first = first.share(b);
        (first_in, second_in, first, second.hold(b, 0u32))
    });
    let (first_in, second_in, first, second) = edge.keep();
    let counter = Arc::new(Counter::default());
    let wakes = || counter.0.load(Ordering::SeqCst);
    let io = graph.io();
    io.send(second_in, 1).unwrap();
    assert_eq!(wakes(), 0, "no waker yet");
    graph.set_waker(Waker::from(counter.clone()));
    graph.pump();

    for n in 0..3 {
        io.send(second_in, n).unwrap();
    }
    assert_eq!(wakes(), 1, "one burst, one wake");
    graph.pump();
    assert_eq!(wakes(), 1, "a pump that queues nothing wakes nothing");
    io.send(second_in, 3).unwrap();
    io.send(second_in, 4).unwrap();
    assert_eq!(wakes(), 2);
    graph.pump();

    let feedback = io.clone();
    graph
        .listen(first, move |n| feedback.send(second_in, n).unwrap())
        .keep();
    io.send(first_in, 5).unwrap();
    assert_eq!(wakes(), 3);
    graph.pump();
    assert_eq!(wakes(), 4, "the listener's call woke the driver");
    assert_eq!(*graph.sample(second), 4, "and waits for the next pump");
    io.send(second_in, 6).unwrap();
    assert_eq!(wakes(), 4, "that wake covers this call too");
    graph.pump();
    assert_eq!(*graph.sample(second), 6);
}
