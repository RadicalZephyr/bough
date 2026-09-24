//! Read-through cells (RFD 4, RFD 5): computed on read into a memo, stepped
//! in order when an input steps, and read like any cell during a
//! transaction, as the value before the instant.

use std::cell::{Cell as StdCell, RefCell};
use std::rc::Rc;

use bough::{Cell, Graph, Leaf, Lift, Source, State};

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
fn map_cell_computes_on_read_and_memoizes_until_its_input_steps() {
    let (calls, count) = counter();
    let (mut graph, (numbers_in, others_in, doubled)) = Graph::build(move |b| {
        let (numbers, numbers_in) = b.input_cell(3u32);
        let (_others, others_in) = b.input_cell(0u32);
        let doubled = numbers.map_cell(b, move |n| {
            count.set(count.get() + 1);
            format!("{}", n * 2)
        });
        (numbers_in, others_in, doubled)
    });
    assert_eq!(calls.get(), 0, "nothing has read the cell");
    let first: &String = graph.sample(doubled);
    let second: &String = graph.sample(doubled);
    assert!(std::ptr::eq(first, second), "the same memoized value");
    assert_eq!(first, "6");
    assert_eq!(calls.get(), 1);

    graph.send(numbers_in, 5);
    assert_eq!(
        calls.get(),
        1,
        "a step clears the memo and computes nothing"
    );
    assert_eq!(graph.sample(doubled), "10");
    assert_eq!(graph.sample(doubled), "10");
    assert_eq!(calls.get(), 2);

    graph.send(others_in, 1);
    assert_eq!(graph.sample(doubled), "10");
    assert_eq!(calls.get(), 2, "a step elsewhere leaves the memo");
}

#[test]
fn a_read_through_cell_that_nothing_reads_never_runs_its_function() {
    let (calls, count) = counter();
    let (mut graph, numbers_in) = Graph::build(move |b| {
        let (numbers, numbers_in) = b.input_cell(1u32);
        let _unread = numbers.map_cell(b, move |n| {
            count.set(count.get() + 1);
            n + 1
        });
        numbers_in
    });
    for n in 0..10 {
        graph.send(numbers_in, n);
    }
    assert_eq!(calls.get(), 0);
}

#[test]
fn a_marked_cell_that_did_not_step_keeps_its_memo_and_stays_quiet() {
    // F4, the semantics-first sketch's probe 7:
    // Updates (MapC (*2) (Hold 0 (Filter (> 5) s))) = [([2], 18)]. Marking
    // reaches the map_cell at both instants; only the second is a step.
    let (calls, count) = counter();
    let (mut graph, (numbers_in, doubled, view)) = Graph::build(move |b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let big = numbers.filter(|n| *n > 5).hold(b, 0u32);
        let doubled = big.map_cell(b, move |n| {
            count.set(count.get() + 1);
            n * 2
        });
        (numbers_in, doubled, doubled.steps(b))
    });
    let (view_seen, on_view) = recorder();
    graph.listen(view, on_view).keep();
    let (steps_seen, mut on_steps) = recorder();
    graph.listen_steps(doubled, move |v| on_steps(*v)).keep();
    let (cell_seen, mut on_cell) = recorder();
    graph.listen_cell(doubled, move |v| on_cell(*v)).keep();
    assert_eq!(calls.get(), 1, "listen_cell read the cell at registration");

    #[cfg(feature = "statistics")]
    let before = graph.statistics();
    graph.send(numbers_in, 3);
    #[cfg(feature = "statistics")]
    {
        let after = graph.statistics();
        assert_eq!(
            after.ordered - before.ordered,
            3,
            "the hold, the map_cell and the steps view"
        );
        assert_eq!(after.evaluations - before.evaluations, 3);
        assert_eq!(after.commits - before.commits, 0, "nothing stepped");
        assert_eq!(after.listener_calls - before.listener_calls, 0);
    }
    assert!(view_seen.borrow().is_empty());
    assert!(steps_seen.borrow().is_empty());
    assert_eq!(*cell_seen.borrow(), [0]);
    assert_eq!(*graph.sample(doubled), 0);
    assert_eq!(calls.get(), 1, "the memo survived a marking without a step");

    graph.send(numbers_in, 9);
    assert_eq!(*view_seen.borrow(), [18]);
    assert_eq!(*steps_seen.borrow(), [18]);
    assert_eq!(*cell_seen.borrow(), [0, 18]);
    assert_eq!(
        calls.get(),
        2,
        "one call for the step, by the steps view, promoted for the listeners"
    );
}

#[test]
fn a_read_through_cell_read_during_a_transaction_gives_the_value_before_it() {
    // `at c t` keeps the steps before t: a snapshot in the instant the
    // input steps reads the old value, and after commit a sample reads the
    // new one, not the memo the snapshot filled.
    for numbers_first in [true, false] {
        let (calls, count) = counter();
        let (mut graph, (numbers_in, probe_in, tripled, seen)) = Graph::build(move |b| {
            let (numbers, numbers_in) = b.input::<u32>();
            let numbers = numbers.share(b);
            let latest = numbers.hold(b, 1u32);
            let tripled = latest.map_cell(b, move |n| {
                count.set(count.get() + 1);
                n * 3
            });
            let (probe, probe_in) = b.input::<()>();
            let seen = numbers
                .snapshot(tripled, |n, t| (n, *t))
                .merge(b, probe.snapshot(tripled, |_, t| (0, *t)), |l, _| l)
                .hold(b, (0u32, 0u32));
            (numbers_in, probe_in, tripled, seen)
        });
        graph.transaction(|tx| {
            if numbers_first {
                tx.send(numbers_in, 5);
                tx.send(probe_in, ());
            } else {
                tx.send(probe_in, ());
                tx.send(numbers_in, 5);
            }
        });
        assert_eq!(
            *graph.sample(seen),
            (5, 3),
            "numbers first: {numbers_first}"
        );
        assert_eq!(*graph.sample(tripled), 15);
        assert_eq!(calls.get(), 2, "once before the instant, once after");
        graph.send(probe_in, ());
        assert_eq!(*graph.sample(seen), (0, 15));
        assert_eq!(calls.get(), 2);
    }
}

#[test]
fn listen_cell_on_a_map_cell_fires_at_registration_and_on_every_step() {
    let (mut graph, (level_in, label)) = Graph::build(|b| {
        let (level, level_in) = b.input_cell(1u32);
        (level_in, level.map_cell(b, |l| format!("level {l}")))
    });
    let (seen, mut on) = recorder();
    graph
        .listen_cell(label, move |t: &String| on(t.clone()))
        .keep();
    graph.send(level_in, 2);
    graph.send(level_in, 2);
    assert_eq!(*seen.borrow(), ["level 1", "level 2", "level 2"]);
}

#[test]
fn a_read_through_cell_steps_whenever_its_input_does_even_to_an_equal_value() {
    let (mut graph, (numbers_in, tens)) = Graph::build(|b| {
        let (numbers, numbers_in) = b.input_cell(5u32);
        (numbers_in, numbers.map_cell(b, |n| n / 10))
    });
    let (seen, mut on) = recorder();
    graph.listen_steps(tens, move |t| on(*t)).keep();
    for n in [6, 7, 15] {
        graph.send(numbers_in, n);
    }
    assert_eq!(*seen.borrow(), [0, 0, 1]);
}

#[test]
fn a_map_cell_over_a_map_cell_steps_with_it() {
    let (calls, count) = counter();
    let (mut graph, (numbers_in, text)) = Graph::build(move |b| {
        let (numbers, numbers_in) = b.input_cell(2u32);
        let squared = numbers.map_cell(b, move |n| {
            count.set(count.get() + 1);
            n * n
        });
        (numbers_in, squared.map_cell(b, |n| format!("{n}")))
    });
    let (seen, mut on) = recorder();
    graph
        .listen_cell(text, move |t: &String| on(t.clone()))
        .keep();
    graph.send(numbers_in, 3);
    graph.send(numbers_in, 4);
    assert_eq!(*seen.borrow(), ["4", "9", "16"]);
    assert_eq!(calls.get(), 3, "the inner function ran once per value");
}

#[test]
fn a_map_cell_over_a_state_is_a_state_read_like_a_cell() {
    let (mut graph, (names_in, count, labels)) = Graph::build(|b| {
        let (names, names_in) = b.input::<String>();
        let names = names.share(b);
        let members = names.accumulate_mut(b, Vec::new(), |n, m: &mut Vec<String>| m.push(n));
        let count: State<usize> = members.map_cell(b, |m| m.len());
        let labels = names
            .snapshot(count, |n, c| format!("{n} after {c}"))
            .hold(b, String::new());
        (names_in, count, labels)
    });
    let (seen, mut on) = recorder();
    graph.listen_cell(count, move |c| on(*c)).keep();
    graph.send(names_in, "ada".to_string());
    graph.send(names_in, "grace".to_string());
    assert_eq!(*seen.borrow(), [0, 1, 2]);
    assert_eq!(*graph.sample(count), 2);
    assert_eq!(graph.sample(labels), "grace after 1");
}

#[test]
fn lift_computes_on_read_and_memoizes_until_an_input_steps() {
    let (calls, count) = counter();
    let (mut graph, (x_in, y_in, product)) = Graph::build(move |b| {
        let (x, x_in) = b.input_cell(3u32);
        let (y, y_in) = b.input_cell(4u32);
        let product: Cell<u32> = (x, y).lift(b, move |x, y| {
            count.set(count.get() + 1);
            x * y
        });
        // Two samples compose in graph code, and read the initial values.
        assert_eq!(format!("{} {}", product.sample(b), x.sample(b)), "12 3");
        (x_in, y_in, product)
    });
    assert_eq!(calls.get(), 1);
    assert!(std::ptr::eq(graph.sample(product), graph.sample(product)));
    graph.send(x_in, 5);
    graph.send(y_in, 6);
    assert_eq!(calls.get(), 1, "nothing read the cell since the build");
    assert_eq!(*graph.sample(product), 30);
    assert_eq!(calls.get(), 2);
}

#[test]
fn lift_steps_once_when_several_inputs_step_in_one_transaction_at_every_arity() {
    let (calls, count) = counter();
    let (mut graph, ((a_in, c_in, d_in), (e_in, g_in, h_in), (two, three, six))) =
        Graph::build(move |b| {
            let (a, a_in) = b.input_cell(1u32);
            let (c, c_in) = b.input_cell(2u64);
            let (d, d_in) = b.input_cell("x".to_string());
            let (e, e_in) = b.input_cell(true);
            let (g, g_in) = b.input_cell(-3i8);
            let (h, h_in) = b.input_cell('h');
            let two = (a, c).lift(b, |a, c| u64::from(*a) + c);
            let three = (a, c, d).lift(b, |a, c, d| format!("{a}{c}{d}"));
            let six = (a, c, d, e, g, h).lift(b, move |a, c, d, e, g, h| {
                count.set(count.get() + 1);
                format!("{a} {c} {d} {e} {g} {h}")
            });
            ((a_in, c_in, d_in), (e_in, g_in, h_in), (two, three, six))
        });
    let (twos, mut on_two) = recorder();
    graph.listen_steps(two, move |v| on_two(*v)).keep();
    let (threes, mut on_three) = recorder();
    graph
        .listen_steps(three, move |v: &String| on_three(v.clone()))
        .keep();
    let (sixes, mut on_six) = recorder();
    graph
        .listen_cell(six, move |v: &String| on_six(v.clone()))
        .keep();

    graph.transaction(|tx| {
        tx.send(c_in, 20);
        tx.send(a_in, 10);
    });
    graph.send(d_in, "y".to_string());
    graph.transaction(|tx| {
        tx.send(h_in, 'i');
        tx.send(g_in, 4);
        tx.send(e_in, false);
        tx.send(d_in, "z".to_string());
        tx.send(c_in, 30);
        tx.send(a_in, 11);
    });
    assert_eq!(*twos.borrow(), [30, 41]);
    assert_eq!(*threes.borrow(), ["1020x", "1020y", "1130z"]);
    assert_eq!(
        *sixes.borrow(),
        [
            "1 2 x true -3 h",
            "10 20 x true -3 h",
            "10 20 y true -3 h",
            "11 30 z false 4 i"
        ]
    );
    assert_eq!(calls.get(), 4, "once at registration and once per step");
}

#[test]
fn lift_of_one_cell_twice_steps_once() {
    let (mut graph, (x_in, square)) = Graph::build(|b| {
        let (x, x_in) = b.input_cell(3u32);
        (x_in, (x, x).lift(b, |a, c| a * c))
    });
    let (seen, mut on) = recorder();
    graph.listen_steps(square, move |v| on(*v)).keep();
    graph.send(x_in, 4);
    assert_eq!(*seen.borrow(), [16]);
}

/// A cell value holding a function: `Box<dyn Fn>` holds no tokens, and
/// `Leaf` says so.
type Op = Leaf<Box<dyn Fn(&u32) -> u32>>;

#[test]
fn sodiums_apply_is_a_lift_of_a_cell_of_functions() {
    let (mut graph, (ops_in, numbers_in, applied)) = Graph::build(|b| {
        let (ops, ops_in) = b.input_cell::<Op>(Leaf(Box::new(|n| n + 1)));
        let (numbers, numbers_in) = b.input_cell(10u32);
        let applied = (ops, numbers).lift(b, |f, a| f(a));
        (ops_in, numbers_in, applied)
    });
    assert_eq!(*graph.sample(applied), 11);
    graph.send(ops_in, Leaf(Box::new(|n| n * 2)));
    assert_eq!(*graph.sample(applied), 20);
    graph.send(numbers_in, 7);
    assert_eq!(*graph.sample(applied), 14);
    graph.transaction(|tx| {
        tx.send(numbers_in, 1);
        tx.send(ops_in, Leaf(Box::new(|n| n + 100)));
    });
    assert_eq!(*graph.sample(applied), 101);
}

#[test]
fn a_lift_with_a_state_among_its_inputs_is_a_state() {
    let (mut graph, (joins_in, extra_in, (first, last, both))) = Graph::build(|b| {
        let (joins, joins_in) = b.input::<String>();
        let members = joins.accumulate_mut(b, Vec::new(), |n, m: &mut Vec<String>| m.push(n));
        let (extra, extra_in) = b.input_cell(10usize);
        let first: State<usize> = (members, extra).lift(b, |m, e| m.len() + e);
        let last: State<usize> = (extra, members).lift(b, |e, m| m.len() * e);
        let both: State<usize> = (first, last).lift(b, |f, l| f * 1000 + l);
        (joins_in, extra_in, (first, last, both))
    });
    let (seen, mut on) = recorder();
    graph.listen_cell(both, move |v| on(*v)).keep();
    graph.send(joins_in, "ada".to_string());
    graph.transaction(|tx| {
        tx.send(extra_in, 20);
        tx.send(joins_in, "grace".to_string());
    });
    assert_eq!(*seen.borrow(), [10_000, 11_010, 22_040]);
    assert_eq!((*graph.sample(first), *graph.sample(last)), (22, 40));
}
