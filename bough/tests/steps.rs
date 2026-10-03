//! The stream views of a cell (RFD 2, RFD 4): `steps`, Sodium's `updates`,
//! and `steps_with_current`, Sodium's `value`. Each event carries the
//! cell's value after the instant, and a read-through cell's value after
//! the instant is promoted into its memo at commit.

use std::cell::{Cell as StdCell, RefCell};
use std::rc::Rc;

use bough::{Lift, Runtime, Source};

/// A shared log and a closure that appends to it.
fn recorder<T: 'static>() -> (Rc<RefCell<Vec<T>>>, impl FnMut(T) + 'static) {
    let log = Rc::new(RefCell::new(Vec::new()));
    let writer = log.clone();
    (log, move |v| writer.borrow_mut().push(v))
}

/// A counter a closure can share with the test.
fn counter() -> (Rc<StdCell<u32>>, Rc<StdCell<u32>>) {
    let c = Rc::new(StdCell::new(0));
    (c.clone(), c)
}

#[test]
fn steps_fires_on_every_step_including_a_step_to_an_equal_value() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (level, level_in) = b.input_cell(5u32);
        (level_in, level.steps(b))
    });
    let (level_in, steps) = edge.keep();
    let (seen, on) = recorder();
    graph.listen(steps, on).keep();
    graph.send(level_in, 5);
    graph.send(level_in, 6);
    graph.send(level_in, 6);
    assert_eq!(*seen.borrow(), [5, 6, 6]);
}

#[test]
fn steps_of_a_hold_behind_a_filter_fires_only_when_the_hold_steps() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let big = numbers.filter(|n| *n > 5).hold(b, 0u32);
        (numbers_in, big.steps(b))
    });
    let (numbers_in, steps) = edge.keep();
    let (seen, on) = recorder();
    graph.listen(steps, on).keep();
    for n in [3, 9, 4, 7] {
        graph.send(numbers_in, n);
    }
    assert_eq!(*seen.borrow(), [9, 7]);
}

#[test]
fn steps_of_a_map_cell_carries_the_value_after_the_instant() {
    // A snapshot in the same instant reads the value before it; the steps
    // view carries the value after it.
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let numbers = numbers.share(b);
        let tripled = numbers.hold(b, 1u32).map_cell(b, |n| n * 3);
        let before = numbers.snapshot(tripled, |_, t| *t);
        let pairs = tripled
            .steps(b)
            .merge(b, before.map(|t| t * 1000), |after, before| before + after);
        (numbers_in, pairs)
    });
    let (numbers_in, pairs) = edge.keep();
    let (seen, on) = recorder();
    graph.listen(pairs, on).keep();
    graph.send(numbers_in, 5);
    graph.send(numbers_in, 6);
    assert_eq!(*seen.borrow(), [3_015, 15_018]);
}

#[test]
fn steps_of_an_accumulator_carries_its_new_value() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let total = numbers.accumulate(b, 0u32, |n, t| t + n);
        (numbers_in, total.steps(b))
    });
    let (numbers_in, totals) = edge.keep();
    let (seen, on) = recorder();
    graph.listen(totals, on).keep();
    for n in [1, 2, 3] {
        graph.send(numbers_in, n);
    }
    assert_eq!(*seen.borrow(), [1, 3, 6]);
}

#[test]
fn steps_of_a_lift_whose_two_inputs_step_together_fires_once() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (x, x_in) = b.input_cell(1u32);
        let (y, y_in) = b.input_cell(2u32);
        let sums = (x, y).lift(b, |x, y| x + y).steps(b);
        (x_in, y_in, sums)
    });
    let (x_in, y_in, sums) = edge.keep();
    let (seen, on) = recorder();
    graph.listen(sums, on).keep();
    graph.transaction(|tx| {
        tx.send(y_in, 20);
        tx.send(x_in, 10);
    });
    graph.send(y_in, 5);
    assert_eq!(*seen.borrow(), [30, 15]);
}

#[test]
fn steps_of_a_constant_never_fires() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (_numbers, numbers_in) = b.input::<u32>();
        let seven = b.constant(7u32);
        let count = seven.steps(b).accumulate(b, 0u32, |_, n| n + 1);
        (numbers_in, count)
    });
    let (numbers_in, count) = edge.keep();
    graph.send(numbers_in, 1);
    assert_eq!(*graph.sample(count), 0);
}

#[test]
fn steps_with_current_fires_at_its_creation_instant_and_on_every_step() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (level, level_in) = b.input_cell(5u32);
        let current = level.steps_with_current(b).share(b);
        // Built in the build, it fires in transaction zero, and a hold
        // built there takes that event.
        let held = current.hold(b, 0u32);
        let seven = b.constant(7u32).steps_with_current(b).hold(b, 0u32);
        (level_in, current, held, seven)
    });
    let (level_in, current, held, seven) = edge.keep();
    assert_eq!(*graph.sample(held), 5);
    assert_eq!(*graph.sample(seven), 7, "a constant's current value");
    let (seen, on) = recorder();
    graph.listen(current, on).keep();
    graph.send(level_in, 6);
    graph.send(level_in, 6);
    assert_eq!(*seen.borrow(), [6, 6]);
    assert_eq!(*graph.sample(held), 6);
}

#[test]
fn a_creation_and_a_step_at_one_instant_are_one_event() {
    // The semantics' `Value`: `coalesce (flip const) ((t0, a) : sts)`. The
    // hold steps in transaction zero, the instant the view is created, so
    // the view fires once there, with the value after the step.
    let (graph, edge) = Runtime::build(|b| {
        let (level, _level_in) = b.input_cell(5u32);
        let stepped = level.steps_with_current(b).hold(b, 0u32);
        let current = stepped.steps_with_current(b).share(b);
        let held = current.hold(b, 1u32);
        let events = current.accumulate(b, 0u32, |_, n| n + 1);
        (held, events)
    });
    let (held, events) = edge.keep();
    assert_eq!((*graph.sample(held), *graph.sample(events)), (5, 1));
}

#[test]
fn a_sample_during_the_build_reads_the_value_before_transaction_zero() {
    // The build is transaction zero, and `at c [0]` keeps the steps before
    // it. The hold and the map_cell over it step in transaction zero, after
    // the closure returns; the memo the sample filled is cleared then.
    let (graph, edge) = Runtime::build(|b| {
        let (level, _level_in) = b.input_cell(5u32);
        let held = level.steps_with_current(b).hold(b, 0u32);
        let doubled = held.map_cell(b, |n| n * 2);
        let sampled = (*held.sample(b), *doubled.sample(b));
        (held, doubled, sampled)
    });
    let (held, doubled, sampled) = edge.keep();
    assert_eq!(sampled, (0, 0));
    assert_eq!((*graph.sample(held), *graph.sample(doubled)), (5, 10));
}

#[test]
fn a_steps_view_value_is_promoted_into_the_memo_at_commit() {
    // The value after instant t is the value before t + 1. The steps view
    // computes it during evaluation, commit promotes it into the memo, and
    // the cell listener and a later sample read the memo: one call per
    // step, not two.
    let (calls, count) = counter();
    let (mut graph, edge) = Runtime::build(move |b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let tripled = numbers.hold(b, 1u32).map_cell(b, move |n| {
            count.set(count.get() + 1);
            n * 3
        });
        (numbers_in, tripled, tripled.steps(b))
    });
    let (numbers_in, tripled, steps) = edge.keep();
    let (stream_seen, on) = recorder();
    graph.listen(steps, on).keep();
    let (cell_seen, mut on_cell) = recorder();
    graph.listen_cell(tripled, move |v| on_cell(*v)).keep();
    assert_eq!(calls.get(), 1, "listen_cell read the cell at registration");
    graph.send(numbers_in, 5);
    graph.send(numbers_in, 6);
    assert_eq!(*stream_seen.borrow(), [15, 18]);
    assert_eq!(*cell_seen.borrow(), [3, 15, 18]);
    assert_eq!(*graph.sample(tripled), 18);
    assert_eq!(calls.get(), 3, "one call per step");
}

#[test]
fn promotion_runs_each_function_of_a_read_through_chain_once_per_step() {
    // A steps view on a lift over a map_cell: preparing the lift computes
    // the map_cell's value after the instant too, and both are promoted,
    // so two steps views and three cell listeners share one call of each
    // function per step.
    let (map_calls, count_map) = counter();
    let (lift_calls, count_lift) = counter();
    let (mut graph, edge) = Runtime::build(move |b| {
        let (x, x_in) = b.input_cell(1u32);
        let (y, y_in) = b.input_cell(10u32);
        let doubled = x.map_cell(b, move |x| {
            count_map.set(count_map.get() + 1);
            x * 2
        });
        let sum = (doubled, y).lift(b, move |d, y| {
            count_lift.set(count_lift.get() + 1);
            d + y
        });
        (x_in, y_in, (doubled, sum), (sum.steps(b), sum.steps(b)))
    });
    let (x_in, y_in, (doubled, sum), (first, second)) = edge.keep();
    let (seen, mut on) = recorder();
    graph.listen(first, move |s| on(("first", s))).keep();
    let (seen_second, on_second) = recorder();
    graph.listen(second, on_second).keep();
    let (cells, mut on_cell) = recorder();
    graph.listen_steps(doubled, move |d| on_cell(*d)).keep();
    graph.listen_steps(sum, |_| ()).keep();
    graph.listen_steps(sum, |_| ()).keep();
    graph.send(x_in, 2);
    graph.send(y_in, 20);
    graph.transaction(|tx| {
        tx.send(x_in, 3);
        tx.send(y_in, 30);
    });
    assert_eq!(
        *seen.borrow(),
        [("first", 14), ("first", 24), ("first", 36)]
    );
    assert_eq!(*seen_second.borrow(), [14, 24, 36]);
    assert_eq!(*cells.borrow(), [4, 6]);
    assert_eq!((*graph.sample(doubled), *graph.sample(sum)), (6, 36));
    // The map_cell stepped twice. At y's step the lift read the map_cell's
    // memo, which the step before had promoted, so nothing recomputed it.
    assert_eq!(map_calls.get(), 2, "once per step of the map_cell");
    assert_eq!(lift_calls.get(), 3, "once per step of the lift");
}

#[test]
fn a_lift_whose_inputs_step_in_transaction_zero_is_promoted_there() {
    // A sample in the build fills the memo with the value before
    // transaction zero; both inputs step there, the steps view computes
    // the value after it, and commit replaces the memo with that value.
    let (calls, count) = counter();
    let (graph, edge) = Runtime::build(move |b| {
        let (x, _x_in) = b.input_cell(2u32);
        let (y, _y_in) = b.input_cell(3u32);
        let x = x.steps_with_current(b).hold(b, 0u32);
        let y = y.steps_with_current(b).hold(b, 0u32);
        let lifted = (x, y).lift(b, move |x, y| {
            count.set(count.get() + 1);
            x * 10 + y
        });
        assert_eq!(*lifted.sample(b), 0);
        let current = lifted.steps_with_current(b).hold(b, 99u32);
        (lifted, current)
    });
    let (lifted, current) = edge.keep();
    assert_eq!((*graph.sample(current), *graph.sample(lifted)), (23, 23));
    assert_eq!(calls.get(), 2, "the sample in the build, then the step");
}
