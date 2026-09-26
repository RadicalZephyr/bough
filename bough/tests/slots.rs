//! Input slots (RFD 7): one pending event folded in place, drained by
//! `pump` as one transaction per pending slot, higher priority first and
//! connection order among equals, each at most once per pump and never
//! simultaneous with another slot; a write wakes the driver's waker; a
//! slot feeds one input of one graph and is let go with the graph.
//!
//! A slot is a `static`, and the tests of this binary run at once on
//! several threads, so every test has slots of its own.
#![cfg(feature = "std")]

use std::any::Any;
use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Wake, Waker};
use std::thread;
use std::time::Duration;

use bough::{InputSlot, PumpError, Runtime, Source, Stream};

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

fn concat(a: String, b: String) -> String {
    a + &b
}

#[test]
fn a_burst_between_pumps_is_one_event_folded_left_to_right() {
    static WORDS: InputSlot<String> = InputSlot::new(concat);
    let (mut graph, edge) = Runtime::build(|b| {
        let (words, words_in) = b.input::<String>();
        b.connect(words_in, &WORDS, 0);
        words.node(b)
    });
    let words = edge.keep();
    let seen = log(&mut graph, words);
    for word in ["a", "b", "c"] {
        WORDS.send(word.to_string());
    }
    assert!(
        seen.borrow().is_empty(),
        "nothing runs until the driver pumps"
    );
    graph.pump();
    assert_eq!(*seen.borrow(), ["abc"], "one event, pending on the left");
    graph.pump();
    assert_eq!(seen.borrow().len(), 1, "a drained slot is empty");
    WORDS.send("d".to_string());
    graph.pump();
    assert_eq!(*seen.borrow(), ["abc", "d"]);
}

#[test]
fn keep_latest_drops_the_older_event() {
    static LEVEL: InputSlot<u32> = InputSlot::keep_latest();
    let (mut graph, edge) = Runtime::build(|b| {
        let (level, level_in) = b.input::<u32>();
        b.connect(level_in, &LEVEL, 0);
        level.hold(b, 0u32)
    });
    let level = edge.keep();
    for v in [3, 1, 4] {
        LEVEL.send(v);
    }
    graph.pump();
    assert_eq!(*graph.sample(level), 4);
}

/// Two slots written before one pump are two transactions in connection
/// order: a merge never sees them together, so its function never runs,
/// and a slot connected first drains first whatever the write order.
#[test]
fn pending_slots_run_one_transaction_each_in_connection_order() {
    static FIRST: InputSlot<u32> = InputSlot::new(|a, b| a + b);
    static SECOND: InputSlot<u32> = InputSlot::new(|a, b| a + b);
    let (mut graph, edge) = Runtime::build(|b| {
        let (left, left_in) = b.input::<u32>();
        let (right, right_in) = b.input::<u32>();
        b.connect(right_in, &FIRST, 0);
        b.connect(left_in, &SECOND, 0);
        left.merge(b, right, |l, r| l * 1000 + r)
    });
    let merged = edge.keep();
    let seen = log(&mut graph, merged);
    SECOND.send(2);
    FIRST.send(1);
    graph.pump();
    assert_eq!(*seen.borrow(), [1, 2], "never combined; connection order");
}

/// Slots drain by priority, higher first, and equal priorities in
/// connection order, whatever order they were connected or written in.
#[test]
fn slots_drain_by_priority_then_connection_order() {
    static LOW: InputSlot<u32> = InputSlot::keep_latest();
    static HIGH: InputSlot<u32> = InputSlot::keep_latest();
    static ALSO_HIGH: InputSlot<u32> = InputSlot::keep_latest();
    static MIDDLE: InputSlot<u32> = InputSlot::keep_latest();
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        b.connect(numbers_in, &LOW, 1);
        b.connect(numbers_in, &HIGH, 3);
        b.connect(numbers_in, &ALSO_HIGH, 3);
        b.connect(numbers_in, &MIDDLE, 2);
        numbers.node(b)
    });
    let numbers = edge.keep();
    let seen = log(&mut graph, numbers);
    MIDDLE.send(2);
    LOW.send(1);
    ALSO_HIGH.send(4);
    HIGH.send(3);
    graph.pump();
    assert_eq!(*seen.borrow(), [3, 4, 2, 1]);
}

/// A slot drains once per pump, even when its own transaction connects a
/// slot of higher priority ahead of it. Here ONCE's first event runs a
/// construct that connects AHEAD, and ONCE's listener writes ONCE again:
/// that write waits for the next pump.
#[test]
fn a_slot_drains_once_per_pump_even_when_a_slot_is_connected_ahead_of_it() {
    static ONCE: InputSlot<u32> = InputSlot::keep_latest();
    static AHEAD: InputSlot<u32> = InputSlot::keep_latest();
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        b.connect(numbers_in, &ONCE, 1);
        let numbers = numbers.share(b);
        let connected = numbers.once().construct(b, |b, _| {
            let (_ahead, ahead_in) = b.input::<u32>();
            b.connect(ahead_in, &AHEAD, 9);
        });
        (numbers, connected)
    });
    let (numbers, _connected) = edge.keep();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    graph
        .listen(numbers, move |n| {
            sink.borrow_mut().push(n);
            if n == 1 {
                ONCE.send(2);
            }
        })
        .keep();
    ONCE.send(1);
    graph.pump();
    assert_eq!(*seen.borrow(), [1], "the second write waits");
    graph.pump();
    assert_eq!(*seen.borrow(), [1, 2]);
}

/// Several slots feed one input, one per producer; each is a transaction
/// of its own, so a non-coalescing input sees no double send.
#[test]
fn two_slots_on_one_input_are_two_transactions() {
    static ONE: InputSlot<u32> = InputSlot::keep_latest();
    static TWO: InputSlot<u32> = InputSlot::keep_latest();
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        b.connect(numbers_in, &ONE, 0);
        b.connect(numbers_in, &TWO, 0);
        numbers.node(b)
    });
    let numbers = edge.keep();
    let seen = log(&mut graph, numbers);
    TWO.send(20);
    ONE.send(10);
    graph.pump();
    assert_eq!(*seen.borrow(), [10, 20]);
}

/// A slot's input may coalesce; the fold and the coalescing function are
/// independent, and a slot never makes the coalescing function run.
#[test]
fn a_slot_never_makes_a_coalescing_input_coalesce() {
    static PARTS: InputSlot<u32> = InputSlot::new(|a, b| a * 10 + b);
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input_coalescing(|a: u32, b| a + b);
        b.connect(numbers_in, &PARTS, 0);
        (numbers.node(b), numbers_in)
    });
    let (numbers, numbers_in) = edge.keep();
    let seen = log(&mut graph, numbers);
    PARTS.send(1);
    PARTS.send(2);
    graph.pump();
    graph.transaction(|tx| {
        tx.send(numbers_in, 5);
        tx.send(numbers_in, 6);
    });
    assert_eq!(*seen.borrow(), [12, 11], "the fold, then the coalescing");
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

/// A write wakes the waker the graph registered, which reaches every slot
/// connected before or after, one a construct connects included; a slot's
/// own waker holds until the graph registers one.
#[test]
fn a_write_wakes_the_waker_the_driver_registered() {
    static EARLY: InputSlot<u32> = InputSlot::keep_latest();
    static LATE: InputSlot<u32> = InputSlot::keep_latest();
    let own = Arc::new(Counter::default());
    EARLY.set_waker(Waker::from(own.clone()));
    EARLY.send(1);
    assert_eq!(
        own.0.load(Ordering::SeqCst),
        1,
        "unconnected, its own waker"
    );
    let (mut graph, edge) = Runtime::build(|b| {
        let (early, early_in) = b.input::<u32>();
        b.connect(early_in, &EARLY, 0);
        let (open, open_in) = b.input::<()>();
        let opened = open.construct(b, move |b, ()| {
            let (late, late_in) = b.input::<u32>();
            b.connect(late_in, &LATE, 0);
            late.hold(b, 0u32)
        });
        let none = b.constant(0u32);
        let shown = opened.hold(b, none).switch_cell(b);
        ((open_in, early.hold(b, 0u32)), shown)
    });
    let ((open_in, latest), shown) = edge.keep();
    EARLY.send(2);
    assert_eq!(own.0.load(Ordering::SeqCst), 2, "the graph registered none");
    let driver = Arc::new(Counter::default());
    graph.set_waker(Waker::from(driver.clone()));
    EARLY.send(3);
    assert_eq!(own.0.load(Ordering::SeqCst), 2);
    assert_eq!(driver.0.load(Ordering::SeqCst), 1);
    graph.pump();
    assert_eq!(
        *graph.sample(latest),
        3,
        "the burst 1, 2, 3 kept its latest"
    );
    graph.send(open_in, ());
    LATE.send(7);
    assert_eq!(driver.0.load(Ordering::SeqCst), 2);
    graph.pump();
    assert_eq!(*graph.sample(shown), 7);
}

/// `set_waker` with a waker that wakes the same task changes nothing, so a
/// future driver may call it on every poll.
#[test]
fn registering_the_same_waker_again_changes_nothing() {
    static TICKS: InputSlot<u32> = InputSlot::new(|a, b| a + b);
    let (mut graph, edge) = Runtime::build(|b| {
        let (ticks, ticks_in) = b.input::<u32>();
        b.connect(ticks_in, &TICKS, 0);
        ticks.accumulate(b, 0u32, |n, t| t + n)
    });
    let ticks = edge.keep();
    let counter = Arc::new(Counter::default());
    let waker = Waker::from(counter.clone());
    for _ in 0..3 {
        graph.set_waker(waker.clone());
        TICKS.send(1);
        graph.pump();
    }
    assert_eq!(counter.0.load(Ordering::SeqCst), 3);
    assert_eq!(*graph.sample(ticks), 3);
}

#[test]
fn a_slot_feeds_one_input_and_a_dropped_graph_lets_it_go() {
    static SHARED: InputSlot<u32> = InputSlot::new(|a, b| a + b);
    static TWICE: InputSlot<u32> = InputSlot::new(|a, b| a + b);
    let (mut first, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        b.connect(numbers_in, &SHARED, 0);
        (numbers_in, numbers.accumulate(b, 0u32, |n, t| t + n))
    });
    let (first_in, first_total) = edge.keep();
    let message = panic_message(|| {
        Runtime::build(|b| {
            let (numbers, numbers_in) = b.input::<u32>();
            b.connect(numbers_in, &SHARED, 0);
            numbers.hold(b, 0u32)
        })
    });
    assert!(
        message.contains("an input slot connected twice"),
        "{message}"
    );
    let message = panic_message(|| {
        Runtime::build(|b| {
            let (numbers, numbers_in) = b.input::<u32>();
            let (other, other_in) = b.input::<u32>();
            b.connect(numbers_in, &TWICE, 0);
            b.connect(other_in, &TWICE, 0);
            numbers.or_else(b, other).hold(b, 0u32)
        })
    });
    assert!(
        message.contains("an input slot connected twice"),
        "{message}"
    );
    SHARED.send(1);
    first.pump();
    assert_eq!(*first.sample(first_total), 1);
    first.send(first_in, 10);
    SHARED.send(5);
    drop(first);
    // The pending 5 was the dropped graph's; the next graph starts empty.
    let (mut second, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        b.connect(numbers_in, &SHARED, 0);
        b.connect(numbers_in, &TWICE, 0);
        numbers.accumulate(b, 0u32, |n, t| t + n)
    });
    let second_total = edge.keep();
    second.pump();
    assert_eq!(*second.sample(second_total), 0);
    SHARED.send(2);
    TWICE.send(3);
    second.pump();
    assert_eq!(*second.sample(second_total), 5);
}

/// A build that panics after connecting unwinds its graph, which lets the
/// slot go.
#[test]
fn a_panicking_build_lets_its_slots_go() {
    static AFTER_PANIC: InputSlot<u32> = InputSlot::keep_latest();
    let message = panic_message(|| {
        Runtime::build(|b| {
            let (numbers, numbers_in) = b.input::<u32>();
            b.connect(numbers_in, &AFTER_PANIC, 0);
            let _ = numbers.hold(b, 0u32);
            if b.constant(true).sample(b) == &true {
                panic!("the build closure failed");
            }
        })
    });
    assert!(message.contains("the build closure failed"));
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        b.connect(numbers_in, &AFTER_PANIC, 0);
        numbers.hold(b, 0u32)
    });
    let latest = edge.keep();
    AFTER_PANIC.send(9);
    graph.pump();
    assert_eq!(*graph.sample(latest), 9);
}

/// A connection is not a root. An event for an input collected since is a
/// stale send, found at `pump`: `try_pump` reports it, and the panicking
/// pump follows RFD 5, a debug-build panic and a counted release no-op.
/// The slot is dropped with its event either way, and the next slot
/// drains at the next pump.
#[test]
fn a_slot_whose_input_was_collected_is_stale_at_pump() {
    static ORPHAN: InputSlot<u32> = InputSlot::keep_latest();
    static ORPHAN_TOO: InputSlot<u32> = InputSlot::keep_latest();
    static KEPT: InputSlot<u32> = InputSlot::keep_latest();
    let (mut graph, edge) = Runtime::build(|b| {
        let (lost, lost_in) = b.input::<u32>();
        let _unrooted = lost.hold(b, 0u32);
        b.connect(lost_in, &ORPHAN, 0);
        b.connect(lost_in, &ORPHAN_TOO, 0);
        let (kept, kept_in) = b.input::<u32>();
        b.connect(kept_in, &KEPT, 0);
        kept.hold(b, 0u32)
    });
    let kept = edge.keep();
    ORPHAN.send(1);
    KEPT.send(2);
    // The first transaction after the build collects what it did not root.
    assert_eq!(graph.try_pump(), Err(PumpError::Stale));
    assert_eq!(*graph.sample(kept), 0, "the rest waits for the next pump");
    assert_eq!(graph.try_pump(), Ok(()));
    assert_eq!(*graph.sample(kept), 2);
    // A stale slot with nothing pending is no send, and is no error.
    assert_eq!(graph.try_pump(), Ok(()));
    ORPHAN_TOO.send(3);
    KEPT.send(4);
    if cfg!(debug_assertions) {
        let message = panic_message(|| graph.pump());
        assert!(message.contains("a send to a collected input"), "{message}");
        assert_eq!(graph.stale_operations(), 0);
        graph.pump();
    } else {
        graph.pump();
        assert_eq!(graph.stale_operations(), 1);
    }
    assert_eq!(*graph.sample(kept), 4, "the graph stays usable");
    // Both stale slots were let go.
    let (_other, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        b.connect(numbers_in, &ORPHAN, 0);
        b.connect(numbers_in, &ORPHAN_TOO, 0);
        numbers.hold(b, 0u32)
    });
    edge.keep();
}

/// A slot written by a listener while the pump runs is drained in this
/// pump when its turn has not come, and at the next pump when it has
/// passed.
#[test]
fn a_slot_written_during_a_pump_drains_when_its_turn_comes() {
    static BEFORE: InputSlot<u32> = InputSlot::keep_latest();
    static SOURCE: InputSlot<u32> = InputSlot::keep_latest();
    static AFTER: InputSlot<u32> = InputSlot::keep_latest();
    let (mut graph, edge) = Runtime::build(|b| {
        let (before, before_in) = b.input::<u32>();
        b.connect(before_in, &BEFORE, 0);
        let (source, source_in) = b.input::<u32>();
        b.connect(source_in, &SOURCE, 0);
        let (after, after_in) = b.input::<u32>();
        b.connect(after_in, &AFTER, 0);
        (source.node(b), before.node(b), after.node(b))
    });
    let (source, before, after) = edge.keep();
    graph
        .listen(source, |v| {
            BEFORE.send(v + 1);
            AFTER.send(v + 2);
        })
        .keep();
    let befores = log(&mut graph, before);
    let afters = log(&mut graph, after);
    SOURCE.send(1);
    graph.pump();
    assert!(befores.borrow().is_empty(), "BEFORE's turn had passed");
    assert_eq!(*afters.borrow(), [3], "AFTER's turn had not come");
    graph.pump();
    assert_eq!(*befores.borrow(), [2]);
}

/// A panic in a slot's transaction poisons the graph, as a panic escaping
/// any transaction does.
#[test]
fn a_panic_in_a_slot_s_transaction_poisons_the_graph() {
    static FRAGILE: InputSlot<u32> = InputSlot::keep_latest();
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        b.connect(numbers_in, &FRAGILE, 0);
        numbers.node(b)
    });
    let numbers = edge.keep();
    graph
        .listen(numbers, |_| panic!("a listener panicked"))
        .keep();
    FRAGILE.send(1);
    assert!(panic_message(|| graph.pump()).contains("a listener panicked"));
    assert_eq!(graph.try_pump(), Err(PumpError::Poisoned));
    assert!(panic_message(|| graph.pump()).contains("poisoned"));
}

/// The thread driver's waker, from RFD 6: an `Arc` made a waker through
/// `Wake`, which a condition variable blocks on.
#[derive(Default)]
struct Signal {
    woken: Mutex<bool>,
    ready: Condvar,
}

impl Wake for Signal {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        *self.woken.lock().unwrap() = true;
        self.ready.notify_one();
    }
}

impl Signal {
    /// Blocks until woken, then clears the flag.
    fn wait(&self) {
        let mut woken = self.woken.lock().unwrap();
        while !*woken {
            woken = self.ready.wait(woken).unwrap();
        }
        *woken = false;
    }
}

/// A slot written on another thread wakes a driver blocked on a condition
/// variable, which pumps. The driver owns a `Local` graph, built on its own
/// thread.
#[test]
fn a_write_from_another_thread_wakes_a_driver_blocked_on_a_condition_variable() {
    static SENSOR: InputSlot<u32> = InputSlot::new(|a, b| a.max(b));
    let (report, reports) = mpsc::channel();
    let signal = Arc::new(Signal::default());
    let waker = Waker::from(signal.clone());
    let driver = thread::spawn(move || {
        let (mut graph, edge) = Runtime::build(|b| {
            let (readings, readings_in) = b.input::<u32>();
            b.connect(readings_in, &SENSOR, 0);
            readings.node(b)
        });
        let readings = edge.keep();
        let last = Rc::new(RefCell::new(0));
        let sink = last.clone();
        let heard = report.clone();
        graph
            .listen(readings, move |v| {
                *sink.borrow_mut() = v;
                heard.send(Some(v)).unwrap();
            })
            .keep();
        graph.set_waker(waker);
        report.send(None).unwrap();
        while *last.borrow() != 3 {
            signal.wait();
            graph.pump();
        }
    });
    assert_eq!(reports.recv().unwrap(), None, "connected and waiting");
    let writer = thread::spawn(|| {
        for v in 1..=3 {
            SENSOR.send(v);
            thread::sleep(Duration::from_millis(5));
        }
    });
    writer.join().unwrap();
    driver.join().unwrap();
    let seen: Vec<u32> = reports.try_iter().flatten().collect();
    assert_eq!(seen.last(), Some(&3), "{seen:?}");
    assert!(seen.windows(2).all(|w| w[0] < w[1]), "{seen:?}");
}
