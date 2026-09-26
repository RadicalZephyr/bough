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
//!
//! With several slots of different priorities, the law holds per slot,
//! with each run cut where the pump drains that slot: between whole units,
//! the highest-priority pending slot that hasn't drained in this pump,
//! which a listener's write can make pending while the pump runs.
//! Each property has a static slot of its own, reconnected in every case:
//! a dropped graph lets its slots go.
#![cfg(feature = "std")]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use proptest::prelude::*;

use bough::{InputSlot, Runtime, Source};

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
    let (mut graph, edge) = Runtime::build(|b| {
        let (events, events_in) = b.input::<A>();
        b.connect(events_in, slot, 0);
        events.node(b)
    });
    let events = edge.keep();
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

/// A driver step with several slots: a write to one of them, or a pump.
#[derive(Clone, Debug)]
enum Many {
    Write(usize, u8),
    Pump,
}

/// Slots of the scheduling property.
const SLOTS: usize = 3;

/// What the pump should deliver, by a model of it: between whole units,
/// the highest-priority pending slot that hasn't drained in this pump,
/// connection order among equals; each drain delivers the fold of the
/// slot's run since its last drain, and its listener writes its
/// reactions, in order, as the engine's listeners do.
fn scheduled(
    priorities: &[u8],
    reactions: &[Vec<(usize, u8)>],
    steps: &[Many],
) -> Vec<(usize, Vec<u8>)> {
    let mut order: Vec<usize> = (0..SLOTS).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(priorities[i]));
    let mut pending: Vec<Option<Vec<u8>>> = vec![None; SLOTS];
    let write = |pending: &mut Vec<Option<Vec<u8>>>, slot: usize, value: u8| {
        pending[slot] = Some(match pending[slot].take() {
            Some(run) => concat(run, vec![value]),
            None => vec![value],
        });
    };
    let mut out = Vec::new();
    for step in steps.iter().chain([&Many::Pump]) {
        match *step {
            Many::Write(slot, value) => write(&mut pending, slot, value),
            Many::Pump => {
                let mut drained = [false; SLOTS];
                while let Some(&slot) = order.iter().find(|&&i| !drained[i] && pending[i].is_some())
                {
                    drained[slot] = true;
                    out.push((slot, pending[slot].take().unwrap()));
                    for &(target, value) in &reactions[slot] {
                        write(&mut pending, target, value);
                    }
                }
            }
        }
    }
    out
}

/// What the engine delivers for the same steps: one input per slot, each
/// connected at its priority, and a listener per input that logs what it
/// hears and writes its reactions. The steps can't deliver more than a
/// slot per pump, so the listeners stop reacting far past that: a pump
/// that drained a slot twice fails the comparison rather than runs on.
fn delivered_many(
    slots: &'static [InputSlot<Vec<u8>>; SLOTS],
    priorities: &[u8],
    reactions: &[Vec<(usize, u8)>],
    steps: &[Many],
) -> Vec<(usize, Vec<u8>)> {
    let (mut graph, edge) = Runtime::build(|b| {
        let mut streams = Vec::new();
        for (slot, &priority) in slots.iter().zip(priorities) {
            let (events, events_in) = b.input::<Vec<u8>>();
            b.connect(events_in, slot, priority);
            streams.push(events);
        }
        streams
    });
    let streams = edge.keep();
    let seen = Rc::new(RefCell::new(Vec::new()));
    for (i, events) in streams.into_iter().enumerate() {
        let sink = seen.clone();
        let reactions = reactions[i].clone();
        graph
            .listen(events, move |run| {
                sink.borrow_mut().push((i, run));
                if sink.borrow().len() > 1000 {
                    return;
                }
                for &(target, value) in &reactions {
                    slots[target].send(vec![value]);
                }
            })
            .keep();
    }
    for step in steps.iter().cloned().chain([Many::Pump]) {
        match step {
            Many::Write(slot, value) => slots[slot].send(vec![value]),
            Many::Pump => graph.pump(),
        }
    }
    seen.take()
}

proptest! {
    /// Priority and pre-emption keep the fold law: each slot delivers the
    /// fold of each run between its drains, and each drain is the one the
    /// model picks. Listeners write slots while the pump runs, their own
    /// included, so pre-emption and the once-per-pump rule both show.
    #[test]
    fn priority_and_pre_emption_keep_the_fold_law(
        priorities in prop::collection::vec(0u8..3, SLOTS),
        reactions in prop::collection::vec(
            prop::collection::vec((0..SLOTS, any::<u8>()), 0..3),
            SLOTS,
        ),
        steps in prop::collection::vec(
            prop_oneof![
                3 => (0..SLOTS, any::<u8>()).prop_map(|(slot, value)| Many::Write(slot, value)),
                1 => Just(Many::Pump),
            ],
            0..40,
        ),
    ) {
        static MANY: [InputSlot<Vec<u8>>; SLOTS] =
            [InputSlot::new(concat), InputSlot::new(concat), InputSlot::new(concat)];
        prop_assert_eq!(
            delivered_many(&MANY, &priorities, &reactions, &steps),
            scheduled(&priorities, &reactions, &steps)
        );
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
    let (mut graph, edge) = Runtime::build(|b| {
        let (events, events_in) = b.input::<Vec<u32>>();
        b.connect(events_in, &SLOT, 0);
        events.node(b)
    });
    let events = edge.keep();
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
