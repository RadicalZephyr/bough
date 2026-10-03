//! Loops (RFD 2): a forward token, declared before its definition exists and
//! closed with it later. The rule `close` checks is that the dependency
//! graph stays acyclic, where reading a cell's value from before the
//! instant (snapshot, gate, sample) is not a dependency (finding F3).
//!
//! Where a test quotes GHC, the value comes from GHC running the unchanged
//! `Denotational.hs` with every loop solved by explicit fixed-point
//! iteration, the research's method (`probes/loop-shapes/loop-shapes.md`);
//! the programs are `bough-oracle/haskell/probes/stage3/Stage3.hs` and
//! `Boundary.hs` beside it, and each test quotes their output. The other
//! tests state the semantics they follow. Instant `[0]` is the build, and
//! `[k]` is the k-th transaction after it.

use std::cell::{Cell as StdCell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use bough::{Lift, Runtime, Source};

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
    let (mut graph, edge) = Runtime::build(|b| {
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
    let (ticks_in, count, next, seen) = edge.keep();
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
/// probes/loop-shapes/loop-shapes.md, "calibration: fact 1's capped
/// counter", 12 evaluations; also `capped` in Stage3.hs).
#[test]
fn the_capped_counter_the_lazy_semantics_cannot_run_steps_ten_times() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (count, count_loop) = b.cell_loop::<u32>();
        let (ticks, ticks_in) = b.input::<()>();
        let next = ticks
            .snapshot(count, |_, n| n + 1)
            .filter(|n| *n <= 10)
            .hold(b, 0u32);
        count_loop.close(b, next);
        (ticks_in, count)
    });
    let (ticks_in, count) = edge.keep();
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

/// A loop through a gate: the counter's ticks pass while a read-through
/// cell over its own forward says the count is below 3. The gate reads the
/// cell's value from before the instant, so the read-through cell depends
/// on the forward and nothing depends on it, and the loop is legal. In the
/// semantics a gate is a filter of a snapshot, so this is fact 1's capped
/// counter with the cap read through a map: 1, 2, 3 at `[1]` to `[3]`, then
/// no step.
#[test]
fn a_loop_through_a_gate_on_a_read_through_cell_of_its_forward() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (count, count_loop) = b.cell_loop::<u32>();
        let below = count.map_cell(b, |n| *n < 3);
        let (ticks, ticks_in) = b.input::<()>();
        let next = ticks
            .gate(below)
            .snapshot(count, |_, n| n + 1)
            .hold(b, 0u32);
        count_loop.close(b, next);
        (ticks_in, count, below)
    });
    let (ticks_in, count, below) = edge.keep();
    let (steps, mut on_step) = recorder();
    graph.listen_steps(count, move |n| on_step(*n)).keep();
    for _ in 0..5 {
        graph.send(ticks_in, ());
    }
    assert_eq!(*steps.borrow(), [1, 2, 3]);
    assert!(!*graph.sample(below));
}

/// Two accumulators read each other, one through a forward token; the loop
/// closes with an accumulate. GHC: `accumulate pair: a
/// (1,[([1],1),([2],5),([3],20),([4],72)]) b
/// (0,[([1],2),([2],5),([3],13),([4],37)])`.
#[test]
fn a_loop_whose_definition_is_an_accumulate() {
    let (mut graph, edge) = Runtime::build(|b| {
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
    let (ticks_in, a, b_acc) = edge.keep();
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
    let (mut graph, edge) = Runtime::build(|b| {
        let (forward, forward_loop) = b.cell_loop::<u32>();
        let doubled = forward.map_cell(b, |n| n * 2);
        let (ticks, ticks_in) = b.input::<u32>();
        let acc = ticks
            .snapshot(doubled, |t, d| (t, *d))
            .accumulate(b, 1u32, |(t, d), s| s + t + d);
        forward_loop.close(b, acc);
        (ticks_in, acc, doubled)
    });
    let (ticks_in, acc, doubled) = edge.keep();
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
    let (mut graph, edge) = Runtime::build(|b| {
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
    let (ticks_in, count, views) = edge.keep();
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
    let (mut graph, edge) = Runtime::build(|b| {
        let (forward, closer) = b.cell_loop::<u32>();
        let forward_steps = forward.steps(b);
        let tripled_steps = forward.map_cell(b, |n| n * 3).steps(b);
        let current = forward.steps_with_current(b).hold(b, 99u32);
        let (s, s_in) = b.input::<u32>();
        let definition = s.hold(b, 0u32);
        closer.close(b, definition);
        (s_in, forward, forward_steps, tripled_steps, current)
    });
    let (s_in, forward, forward_steps, tripled_steps, current) = edge.keep();
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
    let (mut graph, edge) = Runtime::build(move |b| {
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
    let (x_in, y_in, forward, view) = edge.keep();
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
    let (mut graph, edge) = Runtime::build(|b| {
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
    let (s_in, outer, view) = edge.keep();
    let (seen, on) = recorder();
    graph.listen(view, on).keep();
    graph.send(s_in, 8);
    assert_eq!(*seen.borrow(), [8]);
    assert_eq!(*graph.sample(outer), 8);
}

/// The rule's boundary: a steps view inside a loop is legal when the cycle
/// also passes through a snapshot, which delays it to the next instant. x
/// snapshots y, and y holds x's steps: `x = hold 0 (snapshot ticks y
/// (\t y -> y + t))`, `y = hold 100 (map (*2) (updates x))`. GHC
/// (Boundary.hs; the fixed point and the lazy knot agree): x `[1] 101 [2]
/// 204 [3] 411`, y `[1] 202 [2] 408 [3] 822`, y carrying x's value after
/// each instant. With x reading y's steps instead of a snapshot of y, the
/// cycle runs through both loops within one instant and the second close
/// refuses it, naming all seven nodes.
#[test]
fn a_steps_view_inside_a_loop_is_legal_when_a_snapshot_is_on_the_cycle() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (x_fwd, x_loop) = b.cell_loop::<u32>();
        let (y_fwd, y_loop) = b.cell_loop::<u32>();
        let (ticks, ticks_in) = b.input::<u32>();
        let x = ticks.snapshot(y_fwd, |t, y| y + t).hold(b, 0u32);
        let x_steps = x_fwd.steps(b);
        let y = x_steps.map(|v| v * 2).hold(b, 100u32);
        x_loop.close(b, x);
        y_loop.close(b, y);
        (ticks_in, x, y)
    });
    let (ticks_in, x, y) = edge.keep();
    let (x_seen, mut on_x) = recorder();
    graph.listen_steps(x, move |v| on_x(*v)).keep();
    let (y_seen, mut on_y) = recorder();
    graph.listen_steps(y, move |v| on_y(*v)).keep();
    for t in [1, 2, 3] {
        graph.send(ticks_in, t);
    }
    assert_eq!(*x_seen.borrow(), [101, 204, 411]);
    assert_eq!(*y_seen.borrow(), [202, 408, 822]);

    let both_steps = panic_message(|| {
        Runtime::build(|b| {
            let (x_fwd, x_loop) = b.cell_loop::<u32>(); // node 1
            let (y_fwd, y_loop) = b.cell_loop::<u32>(); // node 2
            let (ticks, _ticks_in) = b.input::<u32>(); // node 3
            let y_steps = y_fwd.steps(b); // node 4
            let x = ticks.or_else(b, y_steps).hold(b, 0u32); // nodes 5 and 6
            let x_steps = x_fwd.steps(b); // node 7
            let y = x_steps.map(|v| v * 2).hold(b, 100u32); // node 8
            x_loop.close(b, x);
            y_loop.close(b, y);
        })
    });
    assert!(
        both_steps.contains(
            "same-instant cycle: node 2 (Loop) -> node 4 (Stream) -> node 5 (Stream) -> \
             node 6 (Hold) -> node 1 (Loop) -> node 7 (Stream) -> node 8 (Hold) -> node 2"
        ),
        "{both_steps}"
    );
}

#[test]
fn a_cell_loop_is_one_node_besides_its_definition() {
    let (graph, edge) = Runtime::build(|b| {
        let (count, count_loop) = b.cell_loop::<u32>();
        let (ticks, _ticks_in) = b.input::<()>();
        let next = ticks.snapshot(count, |_, n| n + 1).hold(b, 0u32);
        count_loop.close(b, next);
        next
    });
    let _next = edge.keep();
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
    let _ = Runtime::build(|b| {
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
        Runtime::build(|b| {
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
        Runtime::build(|b| {
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
        Runtime::build(|b| {
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
        Runtime::build(|b| {
            let (c, c_loop) = b.cell_loop::<u32>();
            c_loop.close(b, c);
        })
    });
    assert!(
        itself.contains("same-instant cycle: node 1 (Loop) -> node 1"),
        "{itself}"
    );

    let each_other = panic_message(|| {
        Runtime::build(|b| {
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
/// that is inside `Runtime::build`, before any graph exists, so the panic
/// leaves nothing poisoned: `Runtime::build` panics, and the next build and
/// its transactions work.
#[test]
fn a_loop_left_open_panics_when_the_build_ends_and_poisons_nothing() {
    let message = panic_message(|| {
        Runtime::build(|b| {
            let (_count, _count_loop) = b.cell_loop::<u32>();
        })
    });
    assert!(
        message.contains("a loop declared in this scope was never closed"),
        "{message}"
    );
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.hold(b, 0u32))
    });
    let (numbers_in, latest) = edge.keep();
    graph.send(numbers_in, 3);
    assert_eq!(*graph.sample(latest), 3);
}

#[test]
#[should_panic(expected = "a cell loop sampled before it is closed")]
fn sampling_a_forward_before_close_panics() {
    let _ = Runtime::build(|b| {
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
    let _ = Runtime::build(|b| {
        let (count, count_loop) = b.cell_loop::<u32>();
        let doubled = count.map_cell(b, |n| n * 2);
        let _ = *doubled.sample(b);
        let (ticks, _ticks_in) = b.input::<()>();
        let next = ticks.snapshot(count, |_, n| n + 1).hold(b, 0u32);
        count_loop.close(b, next);
    });
}

// ----------------------------------------------------------- stream loops

/// A stream loop through a hold of its own forward, read by snapshot. The
/// snapshot reads the hold as it was before the instant, so the dependency
/// graph has no cycle and the loop needs no split or defer. GHC (`stream
/// through hold`, a stream fixed point): `[1] 1, [2] 3, [3] 6, [4] 16`, and
/// the hold steps with each.
#[test]
fn a_stream_loop_through_a_hold_read_by_snapshot_needs_no_split_or_defer() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (sums, sums_loop) = b.stream_loop::<u32>();
        let sums = sums.share(b);
        let last = sums.hold(b, 0u32);
        let (ticks, ticks_in) = b.input::<u32>();
        sums_loop.close(b, ticks.snapshot(last, |t, l| t + l));
        (ticks_in, sums, last)
    });
    let (ticks_in, sums, last) = edge.keep();
    let (events, on_event) = recorder();
    graph.listen(sums, on_event).keep();
    let (steps, mut on_step) = recorder();
    graph.listen_steps(last, move |v| on_step(*v)).keep();
    for t in [1, 2, 3, 10] {
        graph.send(ticks_in, t);
    }
    assert_eq!(*events.borrow(), [1, 3, 6, 16]);
    assert_eq!(*steps.borrow(), [1, 3, 6, 16]);
}

/// The forward is linear: its one consumer takes each event, so an event
/// type without `Clone` goes around the loop.
#[test]
fn a_stream_loop_moves_its_events_without_clone() {
    struct Coin(u32);
    let (mut graph, edge) = Runtime::build(|b| {
        let (coins, coins_loop) = b.stream_loop::<Coin>();
        let purse = coins.accumulate(b, 0u32, |coin, total| total + coin.0);
        let (minted, minted_in) = b.input::<u32>();
        coins_loop.close(b, minted.snapshot(purse, |n, p| Coin(n + p)));
        (minted_in, purse)
    });
    let (minted_in, purse) = edge.keep();
    for n in [1, 1, 5] {
        graph.send(minted_in, n);
    }
    // Coins of 1, 1 + 1 and 5 + 3.
    assert_eq!(*graph.sample(purse), 11);
}

/// The definition fires in transaction zero, and a hold of the forward,
/// created before the definition, takes the event there, as `Hold 0
/// (MapS (+100) (Value (Constant 5) [0])) [0]` steps to 105 at `[0]`.
#[test]
fn a_stream_loop_whose_definition_fires_in_transaction_zero() {
    let (graph, edge) = Runtime::build(|b| {
        let (forward, forward_loop) = b.stream_loop::<u32>();
        let held = forward.hold(b, 0u32);
        let start = b.constant(5u32).steps_with_current(b);
        forward_loop.close(b, start.map(|n| n + 100));
        held
    });
    let held = edge.keep();
    assert_eq!(*graph.sample(held), 105);
}

#[test]
fn a_stream_loop_is_one_node_its_definition_is_fused_into() {
    let (graph, edge) = Runtime::build(|b| {
        let (sums, sums_loop) = b.stream_loop::<u32>();
        let total = sums.hold(b, 0u32);
        let (numbers, _numbers_in) = b.input::<u32>();
        let chain = numbers
            .map(|n| n * 2)
            .filter(|n| *n > 0)
            .snapshot(total, |n, t| n + t);
        sums_loop.close(b, chain);
        total
    });
    let _total = edge.keep();
    assert_eq!(
        graph.live_nodes(),
        3,
        "the forward with the chain fused in, the hold, the input"
    );
}

/// A chain whose events come from the forward, directly or through
/// anything that depends on it, a hold's steps included, is a same-instant
/// cycle, refused at close with its nodes.
#[test]
fn stream_loops_whose_chain_depends_on_the_forward_are_refused() {
    let itself = panic_message(|| {
        Runtime::build(|b| {
            let (forward, forward_loop) = b.stream_loop::<u32>(); // node 1
            forward_loop.close(b, forward.map(|n| n + 1));
        })
    });
    assert!(
        itself.contains("same-instant cycle: node 1 (Stream) -> node 1"),
        "{itself}"
    );

    let merged = panic_message(|| {
        Runtime::build(|b| {
            let (forward, forward_loop) = b.stream_loop::<u32>(); // node 1
            let (ticks, _ticks_in) = b.input::<u32>(); // node 2
            let both = ticks.or_else(b, forward.map(|n| n + 1)); // node 3
            forward_loop.close(b, both.filter(|n| *n < 10));
        })
    });
    assert!(
        merged.contains("same-instant cycle: node 1 (Stream) -> node 3 (Stream) -> node 1"),
        "{merged}"
    );

    let steps = panic_message(|| {
        Runtime::build(|b| {
            let (forward, forward_loop) = b.stream_loop::<u32>(); // node 1
            let (ticks, _ticks_in) = b.input::<u32>(); // node 2
            let last = forward.hold(b, 0u32); // node 3
            let last_steps = last.steps(b); // node 4
            let both = ticks.or_else(b, last_steps); // node 5
            forward_loop.close(b, both);
        })
    });
    assert!(
        steps.contains(
            "same-instant cycle: node 1 (Stream) -> node 3 (Hold) -> node 4 (Stream) \
             -> node 5 (Stream) -> node 1"
        ),
        "{steps}"
    );
}

#[test]
#[should_panic(expected = "a loop declared in this scope was never closed")]
fn a_stream_loop_left_open_panics_when_the_build_ends() {
    let _ = Runtime::build(|b| {
        let (forward, _forward_loop) = b.stream_loop::<u32>();
        let _held = forward.hold(b, 0u32);
    });
}

// ----------------------------------------------------------- state loops

/// An in-place accumulator closes a state loop and reads its own forward
/// by snapshot, as another reader does. GHC (`state loop`, the in-place
/// accumulator as the accumulator it is observationally):
/// `([],[([1],[10]),([2],[10,21]),([3],[10,21,32])])`.
#[test]
fn an_in_place_accumulator_closes_a_state_loop_read_by_snapshot() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (log, log_loop) = b.state_loop::<Vec<u32>>();
        let (ticks, ticks_in) = b.input::<u32>();
        let ticks = ticks.share(b);
        let definition = ticks
            .snapshot(log, |t, l| t * 10 + l.len() as u32)
            .accumulate_mut(b, Vec::new(), |entry, l: &mut Vec<u32>| l.push(entry));
        log_loop.close(b, definition);
        let lengths = ticks.snapshot(log, |_, l| l.len() as u32).hold(b, 99u32);
        // A read-through cell over the forward is a State as well.
        let total = log.map_cell(b, |l| l.iter().sum::<u32>());
        (ticks_in, log, lengths, total)
    });
    let (ticks_in, log, lengths, total) = edge.keep();
    let (steps, mut on_step) = recorder();
    graph.listen_steps(log, move |l| on_step(l.clone())).keep();
    let (totals, mut on_total) = recorder();
    graph.listen_cell(total, move |t| on_total(*t)).keep();
    for t in [1, 2, 3] {
        graph.send(ticks_in, t);
    }
    assert_eq!(*steps.borrow(), [vec![10], vec![10, 21], vec![10, 21, 32]]);
    assert_eq!(*totals.borrow(), [0, 10, 31, 63]);
    assert_eq!(*graph.sample(log), [10, 21, 32]);
    assert_eq!(*graph.sample(lengths), 2, "read before the third entry");
}

/// A state loop closes with a Cell too; its forward still only reads, and
/// a lift with it is a State.
#[test]
fn a_state_loop_closes_with_a_cell_and_its_forward_only_reads() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (count, count_loop) = b.state_loop::<u32>();
        let (ticks, ticks_in) = b.input::<()>();
        let next = ticks.snapshot(count, |_, n| n + 1).hold(b, 0u32);
        count_loop.close(b, next);
        let (factor, _factor_in) = b.input_cell(3u32);
        let scaled: bough::State<u32> = (count, factor).lift(b, |c, f| c * f);
        (ticks_in, count, scaled)
    });
    let (ticks_in, count, scaled) = edge.keep();
    for _ in 0..4 {
        graph.send(ticks_in, ());
    }
    assert_eq!(*graph.sample(count), 4);
    assert_eq!(*graph.sample(scaled), 12);
}

#[test]
#[should_panic(expected = "same-instant cycle: node 1 (Loop) -> node 2 (ReadThrough) -> node 1")]
fn a_state_loop_closed_with_a_map_cell_of_its_forward_is_refused() {
    let _ = Runtime::build(|b| {
        let (log, log_loop) = b.state_loop::<Vec<u32>>(); // node 1
        let longer = log.map_cell(b, |l| {
            let mut l = l.clone();
            l.push(0);
            l
        }); // node 2
        log_loop.close(b, longer);
    });
}
