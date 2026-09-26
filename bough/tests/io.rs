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

use bough::{Cell, Input, Io, IoError, PumpError, Runtime, SendError, Source, Stream, TokenError};

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
    graph.remote_io().send(first_in, 7).unwrap();
    graph.pump();
    assert_eq!(*graph.sample(second), 0, "made after the pump began");
    graph.pump();
    assert_eq!(*graph.sample(second), 7);
}

/// Test 2: a send, a listen, then a send. The listener hears only the
/// second send.
#[test]
fn calls_run_in_the_order_they_were_made() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.share(b))
    });
    let (numbers_in, numbers) = edge.keep();
    let io = graph.io();
    let heard = Rc::new(RefCell::new(Vec::new()));
    let sink = heard.clone();
    io.send(numbers_in, 1).unwrap();
    io.listen(numbers, move |n| sink.borrow_mut().push(n))
        .unwrap()
        .keep();
    io.send(numbers_in, 2).unwrap();
    graph.send(numbers_in, 3);
    assert!(
        heard.borrow().is_empty(),
        "nothing registered before the pump"
    );
    graph.pump();
    assert_eq!(*heard.borrow(), [2]);
    graph.send(numbers_in, 4);
    assert_eq!(*heard.borrow(), [2, 4], "kept, the listener stays");
}

/// Test 4: dropping a guard before the pump cancels its registration. The
/// call's closure is dropped at the pump without running, a cancelled
/// anchor roots nothing, and a cancelled call's stale token is never
/// looked up.
#[test]
fn dropping_a_guard_before_the_pump_cancels_its_registration() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let numbers = numbers.share(b);
        (numbers_in, numbers, numbers.hold(b, 0u32))
    });
    let (numbers_in, numbers, latest) = *edge;
    let io = graph.io();
    let heard = Rc::new(RefCell::new(Vec::new()));
    let sink = heard.clone();
    let listener = io
        .listen(numbers, move |n| sink.borrow_mut().push(n))
        .unwrap();
    let anchored = io.anchor(latest).unwrap();
    drop(listener);
    drop(anchored);
    graph.pump();
    assert_eq!(
        Rc::strong_count(&heard),
        1,
        "the listener's closure is gone"
    );
    graph.send(numbers_in, 1);
    assert!(heard.borrow().is_empty(), "the registration never ran");
    let _kept = graph.anchor(numbers_in);
    drop(edge);
    graph.collect_garbage();
    assert_eq!(graph.try_sample(latest).err(), Some(TokenError::Stale));

    drop(io.anchor(latest).unwrap());
    drop(io.listen_steps(latest, |_| ()).unwrap());
    assert_eq!(graph.try_pump(), Ok(()), "a cancelled call never fails");
}

/// `listen_cell`'s first call runs at the pump, with the value then;
/// `listen_steps` makes none.
#[test]
fn a_queued_listen_cell_fires_at_the_pump_with_the_value_then() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.hold(b, 0u32))
    });
    let (numbers_in, latest) = edge.keep();
    let io = graph.io();
    let cells = Rc::new(RefCell::new(Vec::new()));
    let steps = Rc::new(RefCell::new(Vec::new()));
    let (cell_sink, step_sink) = (cells.clone(), steps.clone());
    io.listen_cell(latest, move |n| cell_sink.borrow_mut().push(*n))
        .unwrap()
        .keep();
    io.listen_steps(latest, move |n| step_sink.borrow_mut().push(*n))
        .unwrap()
        .keep();
    graph.send(numbers_in, 5);
    assert!(cells.borrow().is_empty(), "nothing fires before the pump");
    graph.pump();
    assert_eq!(*cells.borrow(), [5], "the value at the pump");
    assert!(steps.borrow().is_empty());
    graph.send(numbers_in, 6);
    assert_eq!(*cells.borrow(), [5, 6]);
    assert_eq!(*steps.borrow(), [6]);
}

/// An anchor an `Io` asked for keeps its value alive until the `Anchored`
/// drops, and the `Anchored` carries the value from the start.
#[test]
fn a_queued_anchor_keeps_its_value_alive_until_it_drops() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.hold(b, 0u32))
    });
    let (numbers_in, latest) = *edge;
    let io = graph.io();
    let kept = io.anchor((numbers_in, latest)).unwrap();
    assert_eq!(*kept, (numbers_in, latest));
    graph.pump();
    drop(edge);
    graph.collect_garbage();
    graph.send(kept.0, 3);
    assert_eq!(*graph.sample(kept.1), 3, "the Io's anchor kept both");
    drop(kept);
    graph.collect_garbage();
    assert_eq!(graph.try_sample(latest).err(), Some(TokenError::Stale));
}

/// A registration that names a collected node is found at the pump, as a
/// send to a collected input is: `try_pump` returns it and anchors
/// nothing, and `pump` panics in a debug build, naming what was asked
/// for, where a release build counts it.
#[test]
fn a_queued_registration_with_a_stale_token_is_found_at_the_pump() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let numbers = numbers.share(b);
        (numbers_in, numbers, numbers.hold(b, 0u32))
    });
    let (numbers_in, numbers, latest) = *edge;
    let _input = graph.anchor(numbers_in);
    let shared = graph.anchor(numbers);
    drop(edge);
    graph.collect_garbage();
    let io = graph.io();

    let _anchored = io.anchor((numbers, latest)).unwrap();
    drop(shared);
    assert_eq!(graph.try_pump(), Err(PumpError::Stale));
    graph.collect_garbage();
    assert_eq!(
        graph.try_listen(numbers, |_| ()).err(),
        Some(TokenError::Stale),
        "the anchor that failed rooted nothing"
    );
    let _listener = io.listen_steps(latest, |_| ()).unwrap();
    if cfg!(debug_assertions) {
        let text = panic_text(catch_unwind(AssertUnwindSafe(|| graph.pump())));
        assert!(text.contains("a listener on a collected node"), "{text}");
    } else {
        graph.pump();
        assert_eq!(graph.stale_operations(), 1);
    }
}

/// A row a construct sends out plain: its input, and the count of what
/// was sent to it, starting at the event that opened it.
type Row = (Input<u32>, Cell<u32>);

/// Opens a row, hands it to `on_row` in a listener, with an `Io`, and
/// returns it once its unit has run. The runtime collects after every
/// transaction, so a row nothing keeps is gone by then.
fn open_a_row(mut on_row: impl FnMut(&Io, Row) + 'static) -> (Runtime, Row) {
    let (mut graph, edge) = Runtime::build(|b| {
        let (open, open_in) = b.input::<u32>();
        let rows = open.construct(b, |b, start| {
            let (bumps, bumps_in) = b.input::<u32>();
            (bumps_in, bumps.accumulate(b, start, |n, c| c + n))
        });
        (open_in, rows)
    });
    let (open_in, rows) = edge.keep();
    graph.set_collect_after_every_transaction(true);
    let io = graph.io();
    let seen = Rc::new(RefCell::new(None));
    let sink = seen.clone();
    graph
        .listen(rows, move |row: Row| {
            on_row(&io, row);
            *sink.borrow_mut() = Some(row);
        })
        .keep();
    graph.send(open_in, 10);
    let row = seen.borrow().expect("the listener saw a row");
    (graph, row)
}

/// Test 8: a listener handed a plain row queues a `listen_cell` on its
/// count. The waiting call keeps the row alive through the collection
/// after its unit, and from the pump on the listener keeps it.
#[test]
fn a_waiting_listen_keeps_a_plain_row_alive_until_the_pump() {
    let counts = Rc::new(RefCell::new(Vec::new()));
    let sink = counts.clone();
    let (mut graph, (bumps_in, count)) = open_a_row(move |io, (_, count)| {
        let sink = sink.clone();
        io.listen_cell(count, move |c| sink.borrow_mut().push(*c))
            .unwrap()
            .keep();
    });
    assert_eq!(
        *graph.sample(count),
        10,
        "alive after its unit's collection"
    );
    graph.pump();
    assert_eq!(*counts.borrow(), [10]);
    graph.send(bumps_in, 5);
    assert_eq!(*counts.borrow(), [10, 15], "the listener keeps it now");
}

/// Test 8: the same with an anchor of the whole row.
#[test]
fn a_waiting_anchor_keeps_a_plain_row_alive_until_the_pump() {
    let (mut graph, (bumps_in, count)) = open_a_row(|io, row| {
        io.anchor(row).unwrap().keep();
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

/// Test 8: a waiting registration whose guard has gone keeps nothing
/// alive, and the pump skips it.
#[test]
fn a_waiting_call_whose_guard_has_gone_keeps_nothing_alive() {
    let (mut graph, (bumps_in, count)) = open_a_row(|io, (bumps_in, count)| {
        drop(io.listen_cell(count, |_| ()).unwrap());
        drop(io.anchor(bumps_in).unwrap());
    });
    assert_eq!(graph.try_sample(count).err(), Some(TokenError::Stale));
    assert_eq!(graph.try_send(bumps_in, 1), Err(SendError::Stale));
    assert_eq!(graph.try_pump(), Ok(()));
}

/// A waiting transaction keeps nothing alive, since its closure hides its
/// tokens: its send to a plain row's input finds the input gone.
#[test]
fn a_waiting_transaction_keeps_nothing_alive() {
    let (mut graph, _row) = open_a_row(|io, (bumps_in, _)| {
        io.transaction(move |tx| tx.send(bumps_in, 5)).unwrap();
    });
    assert_eq!(graph.try_pump(), Err(PumpError::Stale));
}

/// Test 8: a waiting send keeps its input alive, but not the value it
/// carries. Once the send has run, nothing keeps the input.
#[test]
fn a_waiting_send_keeps_its_input_alive_but_not_the_value_it_carries() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (open, open_in) = b.input::<u32>();
        let (_carried, carried_in) = b.input::<Cell<u32>>();
        let rows = open.construct(b, |b, start| {
            let (bumps, bumps_in) = b.input::<u32>();
            (bumps_in, bumps.accumulate(b, start, |n, c| c + n))
        });
        (open_in, carried_in, rows)
    });
    let (open_in, carried_in, rows) = edge.keep();
    graph.set_collect_after_every_transaction(true);
    let io = graph.io();
    let row = Rc::new(RefCell::new(None));
    let row_sink = row.clone();
    graph
        .listen(rows, move |(bumps_in, count): Row| {
            io.send(bumps_in, 5).unwrap();
            io.send(carried_in, count).unwrap();
            *row_sink.borrow_mut() = Some((bumps_in, count));
        })
        .keep();
    graph.send(open_in, 10);
    let (bumps_in, count) = row.borrow().expect("the listener saw a row");
    assert_eq!(
        graph.try_sample(count).err(),
        Some(TokenError::Stale),
        "only a send's value carried the count"
    );
    assert_eq!(graph.try_pump(), Ok(()), "the send found its input alive");
    assert_eq!(graph.try_send(bumps_in, 1), Err(SendError::Stale));
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

/// A stale token is graph knowledge, found at the pump: `try_pump`
/// returns it, and `pump` panics on it in a debug build, where a release
/// build counts it.
#[test]
fn a_stale_token_is_found_at_the_pump() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (kept, kept_in) = b.input::<u32>();
        let (_lost, lost_in) = b.input::<u32>();
        (kept_in, lost_in, kept.hold(b, 0u32))
    });
    let (kept_in, lost_in, latest) = *edge;
    let _kept = graph.anchor((kept_in, latest));
    drop(edge);
    graph.collect_garbage();
    let io = graph.io();

    io.send(lost_in, 1).unwrap();
    io.send(kept_in, 2).unwrap();
    assert_eq!(graph.try_pump(), Err(PumpError::Stale));
    assert_eq!(graph.try_pump(), Ok(()));
    assert_eq!(*graph.sample(latest), 2, "the call behind it ran");

    io.send(lost_in, 3).unwrap();
    if cfg!(debug_assertions) {
        let text = panic_text(catch_unwind(AssertUnwindSafe(|| graph.pump())));
        assert!(text.contains("a send to a collected input"), "{text}");
    } else {
        graph.pump();
        assert_eq!(graph.stale_operations(), 1);
    }
}

/// A token from another graph is refused when the call that names it is
/// queued. A transaction's closure hides its tokens, so a foreign one there
/// is found at the pump: `try_pump` returns it, and `pump` panics on it in
/// both builds.
#[test]
fn a_foreign_token_is_refused_when_queued_or_found_at_the_pump() {
    let (mut graph, edge) = Runtime::build(|b| b.input::<u32>().1);
    let _numbers_in = edge.keep();
    let (_other, other_edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.hold(b, 0u32))
    });
    let (foreign_in, foreign) = other_edge.keep();
    let io = graph.io();

    assert_eq!(io.send(foreign_in, 1), Err(IoError::ForeignGraph));
    assert_eq!(
        io.listen_steps(foreign, |_| ()).err(),
        Some(IoError::ForeignGraph)
    );
    assert_eq!(io.anchor(foreign).err(), Some(IoError::ForeignGraph));
    assert_eq!(graph.try_pump(), Ok(()), "nothing was queued");

    io.transaction(move |tx| tx.send(foreign_in, 2)).unwrap();
    assert_eq!(graph.try_pump(), Err(PumpError::ForeignGraph));
    io.transaction(move |tx| tx.send(foreign_in, 3)).unwrap();
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| graph.pump())));
    assert!(text.contains("another graph"), "{text}");
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

/// A call checks in a fixed order: the runtime has dropped, is poisoned,
/// runs graph code, or a token the call names is another graph's. So
/// graph code naming a foreign token gets `FromGraphCode`, and a poisoned
/// runtime that has dropped gets `Gone`.
#[test]
fn a_call_reports_the_first_failure_in_a_fixed_order() {
    let (_other, other_edge) = Runtime::build(|b| b.input::<u32>().1);
    let foreign_in = other_edge.keep();
    let io_cell: Rc<RefCell<Option<Io>>> = Rc::new(RefCell::new(None));
    let found = Rc::new(RefCell::new(Vec::new()));
    let (mut graph, edge) = Runtime::build({
        let io_cell = io_cell.clone();
        let found = found.clone();
        move |b| {
            let (numbers, numbers_in) = b.input::<u32>();
            let checked = numbers.map(move |n: u32| {
                assert_ne!(n, 13, "unlucky");
                if let Some(io) = &*io_cell.borrow() {
                    found.borrow_mut().push(io.send(foreign_in, n));
                }
                n
            });
            (numbers_in, checked.hold(b, 0u32))
        }
    });
    let (numbers_in, _checked) = edge.keep();
    let io = graph.io();
    *io_cell.borrow_mut() = Some(io.clone());
    graph.send(numbers_in, 1);
    assert_eq!(*found.borrow(), [Err(IoError::FromGraphCode)]);
    let _ = catch_unwind(AssertUnwindSafe(|| graph.send(numbers_in, 13)));
    assert_eq!(graph.try_send(numbers_in, 2), Err(SendError::Poisoned));
    assert_eq!(io.send(numbers_in, 3), Err(IoError::Poisoned));
    drop(graph);
    assert_eq!(io.send(numbers_in, 4), Err(IoError::Gone));
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
