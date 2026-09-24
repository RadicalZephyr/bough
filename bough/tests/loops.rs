//! Loops (RFD 2): a forward token, declared before its definition exists and
//! closed with it later. The rule `close` checks is that the dependency
//! graph stays acyclic, where reading a cell's value from before the
//! instant (snapshot, gate, sample) is not a dependency (finding F3).
//!
//! Expected values come from GHC running the unchanged `Denotational.hs`
//! with every loop solved by explicit fixed-point iteration, the research's
//! method (research-loop-shapes/loop-shapes.md): the program behind each
//! test is in the spike's scratchpad as `stage3-ghc/Stage3.hs`, and its
//! output is quoted at each test. Instant `[0]` is the build, and `[k]` is
//! the k-th transaction after it.

use std::cell::{Cell as StdCell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use bough::{Graph, Lift, Source};

/// A shared log and a closure that appends to it.
fn recorder<T: 'static>() -> (Rc<RefCell<Vec<T>>>, impl FnMut(T) + 'static) {
    let log = Rc::new(RefCell::new(Vec::new()));
    let writer = log.clone();
    (log, move |v| writer.borrow_mut().push(v))
}

/// The message of the panic `f` raises.
fn panic_message<R>(f: impl FnOnce() -> R) -> String {
    let payload = match catch_unwind(AssertUnwindSafe(f)) {
        Ok(_) => panic!("expected a panic"),
        Err(payload) => payload,
    };
    if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else {
        String::new()
    }
}

// ----------------------------------------------------------- legal loops

/// GHC: `counter: (0,[([1],1),([2],2),([3],3),([4],4),([5],5)])`.
#[test]
fn a_counter_reads_its_own_forward_token_through_a_snapshot() {
    let (mut graph, (ticks_in, count, next, seen)) = Graph::build(|b| {
        let (count, count_loop) = b.cell_loop::<u32>();
        let (ticks, ticks_in) = b.input::<()>();
        let ticks = ticks.share(b);
        let next = ticks.snapshot(count, |_, n| n + 1).hold(b, 0u32);
        count_loop.close(b, next);
        assert_eq!(*count.sample(b), 0, "closed: the forward is its definition");
        // Another reader of the forward, in the same instants: it sees the
        // count from before each instant.
        let seen = ticks.snapshot(count, |_, n| *n).hold(b, 99u32);
        (ticks_in, count, next, seen)
    });
    let (values, on_value) = recorder();
    let mut on_value = on_value;
    graph.listen_cell(count, move |n| on_value(*n)).keep();
    for _ in 0..5 {
        graph.send(ticks_in, ());
    }
    assert_eq!(*values.borrow(), [0, 1, 2, 3, 4, 5]);
    assert_eq!(*graph.sample(count), 5);
    assert_eq!(*graph.sample(next), 5);
    assert_eq!(*graph.sample(seen), 4);
}

/// Fact 1's capped counter, on which the lazy executable semantics do not
/// terminate: `c = hold 0 (filter (<= 10) (snapshot ticks c (+1)))`. The
/// fixed point steps 1 to 10 at `[1]` to `[10]` and never again (GHC,
/// research-loop-shapes/loop-shapes.md, "calibration: fact 1's capped
/// counter", 12 evaluations; also `capped` in Stage3.hs).
#[test]
fn the_capped_counter_the_lazy_semantics_cannot_run_steps_ten_times() {
    let (mut graph, (ticks_in, count)) = Graph::build(|b| {
        let (count, count_loop) = b.cell_loop::<u32>();
        let (ticks, ticks_in) = b.input::<()>();
        let next = ticks
            .snapshot(count, |_, n| n + 1)
            .filter(|n| *n <= 10)
            .hold(b, 0u32);
        count_loop.close(b, next);
        (ticks_in, count)
    });
    let now = Rc::new(StdCell::new(0u32));
    let (steps, mut on_step) = recorder();
    let clock = now.clone();
    graph
        .listen_steps(count, move |n| on_step((clock.get(), *n)))
        .keep();
    for k in 1..=12 {
        now.set(k);
        graph.send(ticks_in, ());
    }
    let expected: Vec<(u32, u32)> = (1..=10).map(|k| (k, k)).collect();
    assert_eq!(*steps.borrow(), expected);
    assert_eq!(*graph.sample(count), 10);
}

/// Two accumulators read each other, one through a forward token; the loop
/// closes with an accumulate. GHC: `accumulate pair: a
/// (1,[([1],1),([2],5),([3],20),([4],72)]) b
/// (0,[([1],2),([2],5),([3],13),([4],37)])`.
#[test]
fn a_loop_whose_definition_is_an_accumulate() {
    let (mut graph, (ticks_in, a, b_acc)) = Graph::build(|b| {
        let (a, a_loop) = b.cell_loop::<u64>();
        let (ticks, ticks_in) = b.input::<u64>();
        let ticks = ticks.share(b);
        let b_acc = ticks
            .snapshot(a, |t, a| t + a)
            .accumulate(b, 0u64, |x, s| s + x);
        let a_acc = ticks
            .snapshot(b_acc, |t, b| t * b)
            .accumulate(b, 1u64, |x, s| s + x);
        a_loop.close(b, a_acc);
        (ticks_in, a, b_acc)
    });
    let (a_steps, mut on_a) = recorder();
    graph.listen_steps(a, move |v| on_a(*v)).keep();
    let (b_steps, mut on_b) = recorder();
    graph.listen_steps(b_acc, move |v| on_b(*v)).keep();
    for t in 1..=4 {
        graph.send(ticks_in, t);
    }
    assert_eq!(*a_steps.borrow(), [1, 5, 20, 72]);
    assert_eq!(*b_steps.borrow(), [2, 5, 13, 37]);
}

/// The accumulator's chain snapshots a map_cell of the accumulator's own
/// forward. Its data stays in the arena while its program runs, so the
/// read goes through the loop node and the read-through cell to the
/// accumulator's committed value; taking the whole node out of the arena,
/// or borrowing it whole, fails here. GHC: `acc through read:
/// (1,[([1],8),([2],25),([3],77)])`.
#[test]
fn an_accumulator_reads_itself_through_a_read_through_loop() {
    let (mut graph, (ticks_in, acc, doubled)) = Graph::build(|b| {
        let (forward, forward_loop) = b.cell_loop::<u32>();
        let doubled = forward.map_cell(b, |n| n * 2);
        let (ticks, ticks_in) = b.input::<u32>();
        let acc = ticks
            .snapshot(doubled, |t, d| (t, *d))
            .accumulate(b, 1u32, |(t, d), s| s + t + d);
        forward_loop.close(b, acc);
        (ticks_in, acc, doubled)
    });
    let (steps, mut on_step) = recorder();
    graph.listen_steps(acc, move |v| on_step(*v)).keep();
    for t in [5, 1, 2] {
        graph.send(ticks_in, t);
    }
    assert_eq!(*steps.borrow(), [8, 25, 77]);
    assert_eq!(*graph.sample(doubled), 154);
}

/// The forward is created before its definition, so creation order is not
/// dependency order; the new-node phase of transaction zero pulls the
/// definition, which steps there, before the loop node settles. GHC: `tx
/// zero: (0,[([0],10),([1],11),([2],12)])`.
#[test]
fn a_loop_whose_definition_steps_in_transaction_zero() {
    let (mut graph, (ticks_in, count, views)) = Graph::build(|b| {
        let (count, count_loop) = b.cell_loop::<u32>();
        // Created before the definition, over the forward.
        let views = count.steps_with_current(b).hold(b, 99u32);
        let start = b.constant(10u32).steps_with_current(b);
        let (ticks, ticks_in) = b.input::<()>();
        let next = start
            .or_else(b, ticks.snapshot(count, |_, n| n + 1))
            .hold(b, 0u32);
        count_loop.close(b, next);
        (ticks_in, count, views)
    });
    assert_eq!(*graph.sample(count), 10);
    assert_eq!(*graph.sample(views), 10, "one event at [0], after the step");
    graph.send(ticks_in, ());
    graph.send(ticks_in, ());
    assert_eq!(*graph.sample(count), 12);
    assert_eq!(*graph.sample(views), 12);
}

/// R9: stream views taken on the forward itself, before close. The loop
/// node steps when its definition does, and its value after the instant is
/// the definition's. GHC: `r9 steps fwd: [([1],5),([2],7),([3],7)]`,
/// `r9 steps (map_cell fwd (*3)): [([1],15),([2],21),([3],21)]`, `r9 value
/// fwd at [0]: [([0],0),([1],5),([2],7),([3],7)]`.
#[test]
fn stream_views_of_the_forward_carry_the_definitions_steps() {
    let (mut graph, (s_in, forward, forward_steps, tripled_steps, current)) = Graph::build(|b| {
        let (forward, closer) = b.cell_loop::<u32>();
        let forward_steps = forward.steps(b);
        let tripled_steps = forward.map_cell(b, |n| n * 3).steps(b);
        let current = forward.steps_with_current(b).hold(b, 99u32);
        let (s, s_in) = b.input::<u32>();
        let definition = s.hold(b, 0u32);
        closer.close(b, definition);
        (s_in, forward, forward_steps, tripled_steps, current)
    });
    assert_eq!(
        *graph.sample(current),
        0,
        "steps_with_current of the forward fired at [0]"
    );
    let (steps, on_steps) = recorder();
    graph.listen(forward_steps, on_steps).keep();
    let (tripled, on_tripled) = recorder();
    graph.listen(tripled_steps, on_tripled).keep();
    let (cell, mut on_cell) = recorder();
    graph.listen_cell(forward, move |v| on_cell(*v)).keep();
    let (cell_steps, mut on_cell_step) = recorder();
    graph
        .listen_steps(forward, move |v| on_cell_step(*v))
        .keep();
    for v in [5, 7, 7] {
        graph.send(s_in, v);
    }
    assert_eq!(*steps.borrow(), [5, 7, 7]);
    assert_eq!(*tripled.borrow(), [15, 21, 21]);
    assert_eq!(*cell.borrow(), [0, 5, 7, 7]);
    assert_eq!(*cell_steps.borrow(), [5, 7, 7]);
    assert_eq!(*graph.sample(current), 7);
}

/// A steps view of a forward closed with a lift prepares through the loop
/// node into the lift, whose value after the instant is promoted into its
/// memo at commit: one call of the function per step, however it is read.
#[test]
fn a_steps_view_of_a_forward_closed_with_a_lift_runs_the_function_once_per_step() {
    let calls = Rc::new(StdCell::new(0u32));
    let count = calls.clone();
    let (mut graph, (x_in, y_in, forward, view)) = Graph::build(move |b| {
        let (forward, closer) = b.cell_loop::<u32>();
        let view = forward.steps(b);
        let (x, x_in) = b.input_cell(1u32);
        let (y, y_in) = b.input_cell(10u32);
        let sum = (x, y).lift(b, move |x, y| {
            count.set(count.get() + 1);
            x + y
        });
        closer.close(b, sum);
        (x_in, y_in, forward, view)
    });
    let (seen, on) = recorder();
    graph.listen(view, on).keep();
    assert_eq!(calls.get(), 0);
    graph.send(x_in, 2);
    assert_eq!(*seen.borrow(), [12]);
    assert_eq!(calls.get(), 1, "the steps view computed the value after");
    assert_eq!(*graph.sample(forward), 12);
    assert_eq!(calls.get(), 1, "commit promoted it into the memo");
    graph.transaction(|tx| {
        tx.send(x_in, 3);
        tx.send(y_in, 20);
    });
    assert_eq!(*seen.borrow(), [12, 23], "two inputs, one step");
    assert_eq!(*graph.sample(forward), 23);
    assert_eq!(calls.get(), 2);
}

/// A forward closed with another loop's forward reads through both loop
/// nodes to the last definition; a chain of loops is not a cycle.
#[test]
fn a_loop_closed_with_another_loops_forward_reads_through_both() {
    let (mut graph, (s_in, outer, view)) = Graph::build(|b| {
        let (outer, outer_loop) = b.cell_loop::<u32>();
        let (inner, inner_loop) = b.cell_loop::<u32>();
        let view = outer.steps(b);
        outer_loop.close(b, inner);
        let (s, s_in) = b.input::<u32>();
        let held = s.hold(b, 4u32);
        inner_loop.close(b, held);
        assert_eq!(*outer.sample(b), 4);
        (s_in, outer, view)
    });
    let (seen, on) = recorder();
    graph.listen(view, on).keep();
    graph.send(s_in, 8);
    assert_eq!(*seen.borrow(), [8]);
    assert_eq!(*graph.sample(outer), 8);
}

#[test]
fn a_cell_loop_is_one_node_besides_its_definition() {
    let (graph, ()) = Graph::build(|b| {
        let (count, count_loop) = b.cell_loop::<u32>();
        let (ticks, _ticks_in) = b.input::<()>();
        let next = ticks.snapshot(count, |_, n| n + 1).hold(b, 0u32);
        count_loop.close(b, next);
    });
    assert_eq!(graph.live_nodes(), 3, "the forward, the input, the hold");
}

// ----------------------------------------------------------- refused loops

/// F3. `c = hold 0 (merge ticks (map (+1) (steps c)))`: the path from the
/// definition back to the forward passes through a hold, which RFD 2's
/// rule accepts, yet the hold's steps view is a dependency at the same
/// instant, the text diverges on it, and no order evaluates it. The
/// dependency graph would have a cycle, so close refuses it and names its
/// nodes in creation order: the forward 1, the input 2, the steps view 3,
/// the merge 4, the hold 5.
#[test]
#[should_panic(
    expected = "same-instant cycle: node 1 (Loop) -> node 3 (Stream) -> node 4 (Stream) -> node 5 (Hold) -> node 1"
)]
fn a_loop_through_a_holds_steps_view_is_refused_at_close() {
    let _ = Graph::build(|b| {
        let (c, c_loop) = b.cell_loop::<u32>();
        let (ticks, _ticks_in) = b.input::<u32>();
        let c_steps = c.steps(b);
        let h = ticks
            .merge(b, c_steps.map(|n| n + 1), |t, _| t)
            .hold(b, 0u32);
        c_loop.close(b, h);
    });
}

/// RFD 2's own examples of a refused loop, and the degenerate ones: each
/// defines the cell at an instant by itself at that instant.
#[test]
fn loops_that_define_a_cell_by_itself_at_the_same_instant_are_refused() {
    let lift = panic_message(|| {
        Graph::build(|b| {
            let (c, c_loop) = b.cell_loop::<u32>(); // node 1
            let (other, _other_in) = b.input_cell(1u32); // nodes 2 and 3
            let sum = (c, other).lift(b, |c, o| c + o); // node 4
            c_loop.close(b, sum);
        })
    });
    assert!(
        lift.contains("same-instant cycle: node 1 (Loop) -> node 4 (ReadThrough) -> node 1"),
        "{lift}"
    );

    let map = panic_message(|| {
        Graph::build(|b| {
            let (c, c_loop) = b.cell_loop::<u32>();
            let next = c.map_cell(b, |n| n + 1);
            c_loop.close(b, next);
        })
    });
    assert!(
        map.contains("same-instant cycle: node 1 (Loop) -> node 2 (ReadThrough) -> node 1"),
        "{map}"
    );

    // Sodium's `hold 0 (updates c)`, which has a fixed point at every
    // step list.
    let updates = panic_message(|| {
        Graph::build(|b| {
            let (c, c_loop) = b.cell_loop::<u32>();
            let held = c.steps(b).hold(b, 0u32);
            c_loop.close(b, held);
        })
    });
    assert!(
        updates.contains("node 1 (Loop) -> node 2 (Stream) -> node 3 (Hold) -> node 1"),
        "{updates}"
    );

    let itself = panic_message(|| {
        Graph::build(|b| {
            let (c, c_loop) = b.cell_loop::<u32>();
            c_loop.close(b, c);
        })
    });
    assert!(
        itself.contains("same-instant cycle: node 1 (Loop) -> node 1"),
        "{itself}"
    );

    let each_other = panic_message(|| {
        Graph::build(|b| {
            let (a, a_loop) = b.cell_loop::<u32>();
            let (c, c_loop) = b.cell_loop::<u32>();
            a_loop.close(b, c);
            c_loop.close(b, a);
        })
    });
    assert!(
        each_other.contains("same-instant cycle: node 2 (Loop) -> node 1 (Loop) -> node 2"),
        "{each_other}"
    );
}

/// A loop left open panics where its scope ends. For the build closure
/// that is inside `Graph::build`, before any graph exists, so the panic
/// leaves nothing poisoned: `Graph::build` panics, and the next build and
/// its transactions work.
#[test]
fn a_loop_left_open_panics_when_the_build_ends_and_poisons_nothing() {
    let message = panic_message(|| {
        Graph::build(|b| {
            let (_count, _count_loop) = b.cell_loop::<u32>();
        })
    });
    assert!(
        message.contains("a loop declared in this scope was never closed"),
        "{message}"
    );
    let (mut graph, (numbers_in, latest)) = Graph::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.hold(b, 0u32))
    });
    graph.send(numbers_in, 3);
    assert_eq!(*graph.sample(latest), 3);
}

#[test]
#[should_panic(expected = "a cell loop sampled before it is closed")]
fn sampling_a_forward_before_close_panics() {
    let _ = Graph::build(|b| {
        let (count, count_loop) = b.cell_loop::<u32>();
        let _ = *count.sample(b);
        let (ticks, _ticks_in) = b.input::<()>();
        let next = ticks.snapshot(count, |_, n| n + 1).hold(b, 0u32);
        count_loop.close(b, next);
    });
}

#[test]
#[should_panic(expected = "a cell loop sampled before it is closed")]
fn sampling_a_read_through_cell_over_an_open_forward_panics() {
    let _ = Graph::build(|b| {
        let (count, count_loop) = b.cell_loop::<u32>();
        let doubled = count.map_cell(b, |n| n * 2);
        let _ = *doubled.sample(b);
        let (ticks, _ticks_in) = b.input::<()>();
        let next = ticks.snapshot(count, |_, n| n + 1).hold(b, 0u32);
        count_loop.close(b, next);
    });
}
