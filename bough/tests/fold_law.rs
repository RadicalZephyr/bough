//! RFD 7's fold law: a slot's sequence of writes is cut into runs by when
//! the driver pumps, and each run folds left to right, in the order of the
//! writes, into one event. For random writes and random pumps between
//! them, the stream the engine delivers is exactly the fold of each
//! non-empty run, computed here directly; a pump after an empty run
//! delivers nothing. A later commit checks the same against GHC.
//!
//! The folds: concatenation, associative and not commutative, so a fold
//! out of order or across a cut shows; `keep_latest`; and composition of
//! affine maps, associative and not commutative, on plain integers.
//! Each property has a static slot of its own, reconnected in every case:
//! a dropped graph lets its slots go.
#![cfg(feature = "std")]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use proptest::prelude::*;

use bough::{Graph, InputSlot, Source};

/// A driver step: a write to the slot, or a pump.
#[derive(Clone, Debug)]
enum Step<A> {
    Write(A),
    Pump,
}

fn steps<A: std::fmt::Debug + Clone>(
    value: impl Strategy<Value = A>,
) -> impl Strategy<Value = Vec<Step<A>>> {
    prop::collection::vec(
        prop_oneof![3 => value.prop_map(Step::Write), 1 => Just(Step::Pump)],
        0..48,
    )
}

/// The law, directly: each non-empty run folded left to right.
fn folded_runs<A: Clone>(steps: &[Step<A>], fold: fn(A, A) -> A) -> Vec<A> {
    let mut out = Vec::new();
    let mut run: Option<A> = None;
    for step in steps.iter().chain([&Step::Pump]) {
        match step {
            Step::Write(v) => {
                run = Some(match run.take() {
                    Some(pending) => fold(pending, v.clone()),
                    None => v.clone(),
                })
            }
            Step::Pump => out.extend(run.take()),
        }
    }
    out
}

/// What the engine delivers for the same steps, through a slot connected
/// to an input a listener observes, with a pump at the end.
fn delivered<A: Clone + Send + 'static>(slot: &'static InputSlot<A>, steps: &[Step<A>]) -> Vec<A> {
    let (mut graph, events) = Graph::build(|b| {
        let (events, events_in) = b.input::<A>();
        b.connect(events_in, slot);
        events.node(b)
    });
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    graph
        .listen(events, move |v| sink.borrow_mut().push(v))
        .keep();
    for step in steps.iter().cloned().chain([Step::Pump]) {
        match step {
            Step::Write(v) => slot.send(v),
            Step::Pump => graph.pump(),
        }
    }
    seen.take()
}

fn concat(mut a: Vec<u8>, b: Vec<u8>) -> Vec<u8> {
    a.extend(b);
    a
}

fn latest(_: u16, b: u16) -> u16 {
    b
}

/// `x -> a * x + b`, composed: first `f`, then `g`.
fn affine(f: (u64, u64), g: (u64, u64)) -> (u64, u64) {
    (
        g.0.wrapping_mul(f.0),
        g.0.wrapping_mul(f.1).wrapping_add(g.1),
    )
}

proptest! {
    #[test]
    fn concatenation_folds_each_run(steps in steps(prop::collection::vec(any::<u8>(), 1..4))) {
        static SLOT: InputSlot<Vec<u8>> = InputSlot::new(concat);
        prop_assert_eq!(delivered(&SLOT, &steps), folded_runs(&steps, concat));
    }

    #[test]
    fn keep_latest_keeps_each_run_s_last(steps in steps(any::<u16>())) {
        static SLOT: InputSlot<u16> = InputSlot::keep_latest();
        prop_assert_eq!(delivered(&SLOT, &steps), folded_runs(&steps, latest));
    }

    #[test]
    fn affine_composition_folds_each_run(steps in steps(any::<(u64, u64)>())) {
        static SLOT: InputSlot<(u64, u64)> = InputSlot::new(affine);
        prop_assert_eq!(delivered(&SLOT, &steps), folded_runs(&steps, affine));
    }
}

/// With the writes on another thread, where they fall between pumps is
/// timing: the runs are whatever the pumps cut. Under concatenation the
/// law still says what is delivered: every event is a non-empty run, and
/// the runs, joined, are the writes in order, none lost and none twice.
///
/// The writer pauses now and then. Without the pauses the standard mutex,
/// which is not fair, lets a writer that relocks at once hold off the
/// driver's drain until the burst ends: one run. The slot never grows, so
/// that costs latency only.
#[test]
fn runs_cut_by_timing_join_into_the_writes() {
    static SLOT: InputSlot<Vec<u32>> = InputSlot::new(|mut a, b| {
        a.extend(b);
        a
    });
    const WRITES: u32 = 20_000;
    let (mut graph, events) = Graph::build(|b| {
        let (events, events_in) = b.input::<Vec<u32>>();
        b.connect(events_in, &SLOT);
        events.node(b)
    });
    let runs = Rc::new(RefCell::new(Vec::new()));
    let sink = runs.clone();
    graph
        .listen(events, move |run| sink.borrow_mut().push(run))
        .keep();
    let (done, finished) = mpsc::channel();
    let writer = thread::spawn(move || {
        for k in 0..WRITES {
            SLOT.send(vec![k]);
            if k % 1000 == 999 {
                thread::sleep(Duration::from_micros(200));
            }
        }
        done.send(()).unwrap();
    });
    while finished.try_recv().is_err() {
        graph.pump();
    }
    graph.pump();
    writer.join().unwrap();
    let runs = runs.take();
    assert!(runs.iter().all(|run| !run.is_empty()));
    assert!(
        runs.len() > 1,
        "the pumps cut the writes: {} run",
        runs.len()
    );
    let joined: Vec<u32> = runs.into_iter().flatten().collect();
    assert_eq!(joined, (0..WRITES).collect::<Vec<_>>());
}
