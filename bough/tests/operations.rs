//! The first-order operations, each against the clause of the semantics it
//! implements, observed through `sample`. Listeners have their own file.

use std::cell::Cell as StdCell;
use std::rc::Rc;

use bough::{Leaf, Runtime, Source, Trace, Tracer};

/// A counter a closure can share with the test.
fn counter() -> (Rc<StdCell<u32>>, Rc<StdCell<u32>>) {
    let c = Rc::new(StdCell::new(0));
    (c.clone(), c)
}

#[test]
fn a_hold_starts_at_its_initial_value_and_keeps_the_latest_event() {
    let (mut graph, (numbers_in, latest)) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let latest = numbers.hold(b, 7u32);
        // Sampling in graph code reads the value before the instant.
        assert_eq!(*latest.sample(b), 7);
        (numbers_in, latest)
    });
    assert_eq!(*graph.sample(latest), 7);
    graph.send(numbers_in, 1);
    assert_eq!(*graph.sample(latest), 1);
    graph.send(numbers_in, 2);
    graph.send(numbers_in, 3);
    assert_eq!(*graph.sample(latest), 3);
}

#[test]
fn map_filter_and_hold_fuse_into_one_node() {
    let (mut graph, (numbers_in, out)) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let out = numbers
            .map(|n| n * 2)
            .filter(|n| n % 3 != 0)
            .map(|n| n + 1)
            .hold(b, 0u32);
        (numbers_in, out)
    });
    assert_eq!(graph.live_nodes(), 2, "the input and the chain's hold");
    graph.send(numbers_in, 1);
    assert_eq!(*graph.sample(out), 3);
    graph.send(numbers_in, 3); // 6 is filtered out
    assert_eq!(*graph.sample(out), 3);
    graph.send(numbers_in, 4);
    assert_eq!(*graph.sample(out), 9);
}

#[test]
fn filter_map_maps_and_filters_in_one_step() {
    let (mut graph, (text_in, parsed)) = Runtime::build(|b| {
        let (text, text_in) = b.input::<String>();
        let parsed = text.filter_map(|t| t.parse::<i64>().ok()).hold(b, -1i64);
        (text_in, parsed)
    });
    graph.send(text_in, "12".to_string());
    assert_eq!(*graph.sample(parsed), 12);
    graph.send(text_in, "twelve".to_string());
    assert_eq!(*graph.sample(parsed), 12);
    graph.send(text_in, "-3".to_string());
    assert_eq!(*graph.sample(parsed), -3);
}

#[test]
fn filter_map_with_the_identity_is_sodiums_filter_optional() {
    let (mut graph, (maybe_in, held)) = Runtime::build(|b| {
        let (maybe, maybe_in) = b.input::<Option<u32>>();
        (maybe_in, maybe.filter_map(|o| o).hold(b, 0u32))
    });
    graph.send(maybe_in, Some(4));
    graph.send(maybe_in, None);
    assert_eq!(*graph.sample(held), 4);
}

#[test]
fn map_to_replaces_each_event_with_a_clone_of_one_value() {
    let (mut graph, (clicks_in, label)) = Runtime::build(|b| {
        let (clicks, clicks_in) = b.input::<()>();
        (
            clicks_in,
            clicks.map_to("clicked".to_string()).hold(b, String::new()),
        )
    });
    assert_eq!(graph.sample(label), "");
    graph.send(clicks_in, ());
    assert_eq!(graph.sample(label), "clicked");
}

#[test]
fn snapshot_reads_the_cell_as_it_was_before_the_instant() {
    let (mut graph, (numbers_in, limit_in, out)) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let (limit, limit_in) = b.input_cell(10u32);
        let out = numbers.snapshot(limit, |n, l| n.min(*l)).hold(b, 0u32);
        (numbers_in, limit_in, out)
    });
    graph.send(numbers_in, 18);
    assert_eq!(*graph.sample(out), 10);
    // The limit steps in the same instant: the snapshot still reads 10.
    graph.transaction(|tx| {
        tx.send(limit_in, 100);
        tx.send(numbers_in, 18);
    });
    assert_eq!(*graph.sample(out), 10);
    // The other send order gives the same instant.
    graph.transaction(|tx| {
        tx.send(numbers_in, 50);
        tx.send(limit_in, 20);
    });
    assert_eq!(*graph.sample(out), 50);
    graph.send(numbers_in, 50);
    assert_eq!(*graph.sample(out), 20);
}

#[test]
fn gate_keeps_the_events_during_which_the_cell_was_true_before_the_instant() {
    let (mut graph, (numbers_in, open_in, out)) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let (open, open_in) = b.input_cell(true);
        (numbers_in, open_in, numbers.gate(open).hold(b, 0u32))
    });
    graph.send(numbers_in, 1);
    assert_eq!(*graph.sample(out), 1);
    graph.send(open_in, false);
    graph.send(numbers_in, 2);
    assert_eq!(*graph.sample(out), 1);
    // Opening in the same instant does not let this event through.
    graph.transaction(|tx| {
        tx.send(numbers_in, 3);
        tx.send(open_in, true);
    });
    assert_eq!(*graph.sample(out), 1);
    graph.send(numbers_in, 4);
    assert_eq!(*graph.sample(out), 4);
    // Closing in the same instant lets this event through.
    graph.transaction(|tx| {
        tx.send(open_in, false);
        tx.send(numbers_in, 5);
    });
    assert_eq!(*graph.sample(out), 5);
}

#[test]
fn once_keeps_only_the_first_event() {
    let (mut graph, (numbers_in, first)) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.once().hold(b, 0u32))
    });
    graph.send(numbers_in, 5);
    graph.send(numbers_in, 6);
    assert_eq!(*graph.sample(first), 5);
}

#[test]
fn once_before_a_gate_takes_its_first_event_even_when_the_gate_drops_it() {
    // `gate (once s) c` against `once (gate s c)`: the first loses the
    // first event to the closed gate, the second waits for the gate.
    let (mut graph, (numbers_in, open_in, gated_once, once_gated)) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let numbers = numbers.share(b);
        let (open, open_in) = b.input_cell(false);
        let gated_once = numbers.once().gate(open).hold(b, 0u32);
        let once_gated = numbers.gate(open).once().hold(b, 0u32);
        (numbers_in, open_in, gated_once, once_gated)
    });
    graph.send(numbers_in, 1);
    graph.send(open_in, true);
    graph.send(numbers_in, 2);
    graph.send(numbers_in, 3);
    assert_eq!(*graph.sample(gated_once), 0);
    assert_eq!(*graph.sample(once_gated), 2);
}

#[test]
fn merge_combines_simultaneous_events_once_with_the_left_event_first() {
    let (calls, count) = counter();
    let (mut graph, (left_in, right_in, merged)) = Runtime::build(move |b| {
        let (left, left_in) = b.input::<u32>();
        let (right, right_in) = b.input::<u32>();
        let merged = left
            .merge(b, right, move |l, r| {
                count.set(count.get() + 1);
                l * 10 + r
            })
            .hold(b, 0u32);
        (left_in, right_in, merged)
    });
    graph.transaction(|tx| {
        tx.send(left_in, 1);
        tx.send(right_in, 2);
    });
    assert_eq!(*graph.sample(merged), 12);
    assert_eq!(calls.get(), 1);
    // The send order inside a transaction does not matter.
    graph.transaction(|tx| {
        tx.send(right_in, 4);
        tx.send(left_in, 3);
    });
    assert_eq!(*graph.sample(merged), 34);
    assert_eq!(calls.get(), 2);
    // Either event alone passes through without the function.
    graph.send(left_in, 5);
    assert_eq!(*graph.sample(merged), 5);
    graph.send(right_in, 6);
    assert_eq!(*graph.sample(merged), 6);
    assert_eq!(calls.get(), 2);
}

#[test]
fn merge_of_two_chains_from_one_shared_stream_fires_once() {
    let (calls, count) = counter();
    let (mut graph, (numbers_in, merged)) = Runtime::build(move |b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let numbers = numbers.share(b);
        let merged = numbers
            .map(|n| n + 1)
            .merge(b, numbers.map(|n| n * 10), move |l, r| {
                count.set(count.get() + 1);
                l + r
            })
            .hold(b, 0u32);
        (numbers_in, merged)
    });
    graph.send(numbers_in, 2);
    assert_eq!(*graph.sample(merged), 23);
    assert_eq!(calls.get(), 1);
}

#[test]
fn or_else_keeps_the_left_event() {
    let (mut graph, (left_in, right_in, merged)) = Runtime::build(|b| {
        let (left, left_in) = b.input::<&'static str>();
        let (right, right_in) = b.input::<&'static str>();
        (left_in, right_in, left.or_else(b, right).hold(b, ""))
    });
    graph.transaction(|tx| {
        tx.send(right_in, "right");
        tx.send(left_in, "left");
    });
    assert_eq!(*graph.sample(merged), "left");
    graph.send(right_in, "right alone");
    assert_eq!(*graph.sample(merged), "right alone");
}

#[test]
fn coalescing_inputs_fold_in_send_order_with_the_first_send_on_the_left() {
    let (mut graph, (words_in, folded)) = Runtime::build(|b| {
        let (words, words_in) = b.input_coalescing(|a: String, b: String| format!("({a}{b})"));
        (words_in, words.hold(b, String::new()))
    });
    graph.transaction(|tx| {
        tx.send(words_in, "a".to_string());
        tx.send(words_in, "b".to_string());
        tx.send(words_in, "c".to_string());
    });
    assert_eq!(graph.sample(folded), "((ab)c)");
    graph.send(words_in, "d".to_string());
    assert_eq!(graph.sample(folded), "d", "one send is not folded");
}

#[test]
fn an_input_cell_starts_at_its_initial_value_and_steps_on_each_send() {
    let (mut graph, (level, level_in)) = Runtime::build(|b| b.input_cell(3u8));
    assert_eq!(graph.live_nodes(), 2, "an input and a hold over it");
    assert_eq!(*graph.sample(level), 3);
    graph.send(level_in, 4);
    assert_eq!(*graph.sample(level), 4);
}

#[test]
fn a_coalescing_input_cell_folds_the_sends_of_one_transaction() {
    let (mut graph, (digits, digits_in)) =
        Runtime::build(|b| b.input_cell_coalescing(0u32, |a, b| a * 10 + b));
    graph.transaction(|tx| {
        tx.send(digits_in, 1);
        tx.send(digits_in, 2);
        tx.send(digits_in, 3);
    });
    assert_eq!(*graph.sample(digits), 123);
}

#[test]
fn a_constant_never_changes_and_can_be_snapshotted() {
    let (mut graph, (numbers_in, scale, scaled)) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let scale = b.constant(3u32);
        (
            numbers_in,
            scale,
            numbers.snapshot(scale, |n, k| n * k).hold(b, 0u32),
        )
    });
    graph.send(numbers_in, 5);
    assert_eq!(*graph.sample(scale), 3);
    assert_eq!(*graph.sample(scaled), 15);
}

#[test]
fn never_never_fires() {
    let (calls, count) = counter();
    let (mut graph, (numbers_in, merged, held)) = Runtime::build(move |b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let nothing = b.never::<u32>();
        let merged = numbers
            .merge(b, nothing, move |l, _| {
                count.set(count.get() + 1);
                l
            })
            .hold(b, 0u32);
        let held = b.never::<u32>().map(|n| n + 1).hold(b, 9u32);
        (numbers_in, merged, held)
    });
    graph.send(numbers_in, 4);
    assert_eq!(*graph.sample(merged), 4);
    assert_eq!(*graph.sample(held), 9);
    assert_eq!(calls.get(), 0);
}

#[test]
fn a_shared_stream_gives_every_consumer_the_event() {
    let (mut graph, (words_in, lengths, shouts)) = Runtime::build(|b| {
        let (words, words_in) = b.input::<String>();
        let words = words.share(b);
        let lengths = words.map(|w| w.len()).hold(b, 0usize);
        let shouts = words.map(|w| w.to_uppercase()).hold(b, String::new());
        (words_in, lengths, shouts)
    });
    graph.send(words_in, "hey".to_string());
    assert_eq!(*graph.sample(lengths), 3);
    assert_eq!(graph.sample(shouts), "HEY");
}

#[test]
fn a_node_gives_a_chain_an_identity_that_a_later_chain_reads() {
    let (mut graph, (numbers_in, out)) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let doubled = numbers.map(|n| n * 2).node(b);
        (numbers_in, doubled.map(|n| n + 1).hold(b, 0u32))
    });
    assert_eq!(graph.live_nodes(), 3);
    graph.send(numbers_in, 4);
    assert_eq!(*graph.sample(out), 9);
}

/// An event with no `Clone`, which only a linear path can carry.
struct Ticket(u32);

impl Trace for Ticket {
    fn trace(&self, _tracer: &mut Tracer) {}
}

#[test]
fn an_event_with_no_clone_moves_through_a_linear_path_into_a_hold() {
    let (mut graph, (tickets_in, last)) = Runtime::build(|b| {
        let (tickets, tickets_in) = b.input::<Ticket>();
        let nothing = b.never();
        let last = tickets
            .filter(|t| t.0 != 0)
            .merge(b, nothing, |l, _| l)
            .hold(b, Ticket(0));
        (tickets_in, last)
    });
    graph.send(tickets_in, Ticket(5));
    graph.send(tickets_in, Ticket(0));
    assert_eq!(graph.sample(last).0, 5);
}

#[test]
fn cell_values_are_read_by_reference_without_clone() {
    let (mut graph, (names_in, names)) = Runtime::build(|b| {
        let (names, names_in) = b.input::<Leaf<Vec<String>>>();
        (names_in, names.hold(b, Leaf(Vec::new())))
    });
    graph.send(names_in, Leaf(vec!["ada".to_string()]));
    let first: &Leaf<Vec<String>> = graph.sample(names);
    let second: &Leaf<Vec<String>> = graph.sample(names);
    assert!(std::ptr::eq(first, second));
    assert_eq!(first.0, ["ada".to_string()]);
}

#[test]
fn a_threaded_graph_runs_every_first_order_operation() {
    let (mut graph, (numbers_in, limit_in, words_in, out, words)) = Runtime::build_threaded(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let numbers = numbers.share(b);
        let (limit, limit_in) = b.input_cell(10u32);
        let (open, _open_in) = b.input_cell(true);
        let clipped = numbers
            .map(|n| n * 2)
            .filter(|n| *n > 0)
            .filter_map(Some)
            .snapshot(limit, |n, l| n.min(*l))
            .gate(open)
            .node(b);
        let first = numbers.once().map_to(1000u32);
        let nothing = b.never::<u32>();
        let out = clipped
            .merge(b, first, |l, r| l + r)
            .or_else(b, nothing)
            .hold(b, 0u32);
        let (words, words_in) = b.input_coalescing(|a: String, b: String| a + &b);
        let words = words.hold(b, String::new());
        (numbers_in, limit_in, words_in, out, words)
    });
    graph.send(numbers_in, 3);
    assert_eq!(*graph.sample(out), 1006);
    graph.send(limit_in, 5);
    graph.send(numbers_in, 4);
    assert_eq!(*graph.sample(out), 5);
    graph.transaction(|tx| {
        tx.send(words_in, "x".to_string());
        tx.send(words_in, "y".to_string());
    });
    assert_eq!(graph.sample(words), "xy");
}
