//! Accumulators and `scan` (RFD 4): `accumulate` reads its own value from
//! before the instant, `accumulate_mut` runs its function at commit and
//! gives the same values, and `scan` emits an output per event and keeps
//! its state. An in-place accumulator's `State` is read by every cell
//! reader.

use std::cell::{Cell as StdCell, RefCell};
use std::rc::Rc;

use bough::{Runtime, Source};

/// A shared log and a closure that appends to it.
fn recorder<T: 'static>() -> (Rc<RefCell<Vec<T>>>, impl FnMut(T) + 'static) {
    let log = Rc::new(RefCell::new(Vec::new()));
    let writer = log.clone();
    (log, move |v| writer.borrow_mut().push(v))
}

#[test]
fn an_accumulator_reads_its_own_value_from_before_the_instant() {
    let (mut graph, (digits_in, number, seen)) = Runtime::build(|b| {
        let (digits, digits_in) = b.input::<u32>();
        let digits = digits.share(b);
        // The state `f` gets is the accumulator's own committed value.
        let number = digits.accumulate(b, 0u32, |d, n| n * 10 + d);
        // A snapshot in the instant of a step reads the value before it.
        let seen = digits.snapshot(number, |_, n| *n).hold(b, 0u32);
        (digits_in, number, seen)
    });
    assert_eq!(*graph.sample(number), 0);
    for d in [1, 2, 3] {
        graph.send(digits_in, d);
    }
    assert_eq!(*graph.sample(number), 123);
    assert_eq!(*graph.sample(seen), 12);
}

#[test]
fn an_accumulator_steps_only_when_its_chain_fires() {
    let (mut graph, (numbers_in, total)) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let total = numbers
            .filter(|n| n % 2 == 1)
            .accumulate(b, 100u32, |n, total| total + n);
        (numbers_in, total)
    });
    let (steps, mut on) = recorder();
    graph.listen_steps(total, move |t| on(*t)).keep();
    for n in [1, 2, 3, 4, 5] {
        graph.send(numbers_in, n);
    }
    assert_eq!(*steps.borrow(), [101, 104, 109]);
}

#[test]
fn accumulate_and_accumulate_mut_give_the_same_values_on_the_same_input() {
    let calls = Rc::new(StdCell::new(0u32));
    let count = calls.clone();
    let (mut graph, (words_in, copied, in_place, lengths)) = Runtime::build(move |b| {
        let (words, words_in) = b.input::<String>();
        let words = words.share(b);
        let copied =
            words
                .filter(|w| !w.is_empty())
                .accumulate(b, Vec::new(), |w, list: &Vec<String>| {
                    let mut next = list.clone();
                    next.push(w);
                    next
                });
        let in_place = words.filter(|w| !w.is_empty()).accumulate_mut(
            b,
            Vec::new(),
            move |w, list: &mut Vec<String>| {
                count.set(count.get() + 1);
                list.push(w)
            },
        );
        // Both read in the instant of each step: the values before it.
        let lengths = words
            .snapshot(copied, |_, c| c.len())
            .snapshot(in_place, |c, m| (c, m.len()))
            .node(b);
        (words_in, copied, in_place, lengths)
    });
    let (copied_seen, mut on_copied) = recorder();
    graph
        .listen_cell(copied, move |v| on_copied(v.join(" ")))
        .keep();
    let (in_place_seen, mut on_in_place) = recorder();
    graph
        .listen_cell(in_place, move |v| on_in_place(v.join(" ")))
        .keep();
    let (read, on_read) = recorder();
    graph.listen(lengths, on_read).keep();

    for word in ["a", "", "b", "c"] {
        graph.send(words_in, word.to_string());
        assert_eq!(graph.sample(copied), graph.sample(in_place));
    }
    assert_eq!(graph.sample(in_place).join(" "), "a b c");
    assert_eq!(*copied_seen.borrow(), ["", "a", "a b", "a b c"]);
    assert_eq!(*in_place_seen.borrow(), *copied_seen.borrow());
    assert_eq!(*read.borrow(), [(0, 0), (1, 1), (1, 1), (2, 2)]);
    assert_eq!(calls.get(), 3, "the in-place function ran once per event");
}

#[test]
fn scan_emits_an_output_per_event_and_keeps_its_state() {
    let (mut graph, (numbers_in, labels, last)) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        // Sodium's collect: the running count and total are the state, and
        // each event emits a label built from the state before it.
        let labels = numbers
            .filter(|n| *n != 0)
            .scan(b, (0u32, 0u32), |n, (count, total)| {
                (format!("#{count}: {total}+{n}"), (count + 1, total + n))
            })
            .share(b);
        let last = labels.hold(b, String::new());
        (numbers_in, labels, last)
    });
    let (seen, on) = recorder();
    graph.listen(labels, on).keep();
    for n in [5, 0, 7, 1] {
        graph.send(numbers_in, n);
    }
    assert_eq!(*seen.borrow(), ["#0: 0+5", "#1: 5+7", "#2: 12+1"]);
    assert_eq!(graph.sample(last), "#2: 12+1");
}

#[test]
fn two_scans_over_one_stream_keep_their_own_states() {
    // Both advance at every send, and each reads its own state from before
    // the instant.
    let (mut graph, (numbers_in, sums, products)) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u64>();
        let numbers = numbers.share(b);
        let sums = numbers.scan(b, 0u64, |n, s| (s + n, s + n)).hold(b, 0u64);
        let products = numbers.scan(b, 1u64, |n, p| (p * n, p * n)).hold(b, 0u64);
        (numbers_in, sums, products)
    });
    for n in [2, 3, 4] {
        graph.send(numbers_in, n);
    }
    assert_eq!((*graph.sample(sums), *graph.sample(products)), (9, 24));
}

#[test]
fn a_state_is_read_by_sample_snapshot_gate_and_the_cell_listeners() {
    let (mut graph, (joins_in, lines_in, members, open, said)) = Runtime::build(|b| {
        let (joins, joins_in) = b.input::<String>();
        let (lines, lines_in) = b.input::<String>();
        let joins = joins.share(b);
        let members = joins.accumulate_mut(b, Vec::new(), |name, m: &mut Vec<String>| m.push(name));
        let open = joins.accumulate_mut(b, false, |_, open: &mut bool| *open = true);
        assert!(members.sample(b).is_empty());
        assert!(!*open.sample(b));
        let said = lines
            .gate(open)
            .snapshot(members, |line, m| format!("{line} to {}", m.len()))
            .node(b);
        (joins_in, lines_in, members, open, said)
    });
    let (heard, on_said) = recorder();
    graph.listen(said, on_said).keep();
    let (sizes, mut on_size) = recorder();
    graph.listen_cell(members, move |m| on_size(m.len())).keep();
    let (opened, mut on_open) = recorder();
    graph.listen_steps(open, move |o| on_open(*o)).keep();

    graph.send(lines_in, "nobody".to_string());
    // The gate and the snapshot read the states before the instant: closed.
    graph.transaction(|tx| {
        tx.send(joins_in, "ada".to_string());
        tx.send(lines_in, "hello".to_string());
    });
    graph.send(lines_in, "again".to_string());
    graph.transaction(|tx| {
        tx.send(lines_in, "both".to_string());
        tx.send(joins_in, "grace".to_string());
    });
    assert_eq!(*heard.borrow(), ["again to 1", "both to 1"]);
    assert_eq!(*sizes.borrow(), [0, 1, 2]);
    assert_eq!(*opened.borrow(), [true, true], "a step to an equal value");
    assert_eq!(graph.sample(members).join(" "), "ada grace");
    assert_eq!(graph.try_sample(members).map(Vec::len), Ok(2));
    assert!(*graph.sample(open));
}

#[test]
fn an_in_place_accumulator_runs_its_function_at_commit() {
    // The listener sees the mutated state; a snapshot in the same instant
    // saw the old one.
    let (mut graph, (numbers_in, log, before)) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let numbers = numbers.share(b);
        let log = numbers.accumulate_mut(b, Vec::new(), |n, log: &mut Vec<u32>| log.push(n));
        let before = numbers
            .snapshot(log, |_, log| log.iter().sum::<u32>())
            .hold(b, 0u32);
        (numbers_in, log, before)
    });
    let (seen, mut on) = recorder();
    graph.listen_steps(log, move |l| on(l.clone())).keep();
    graph.send(numbers_in, 4);
    graph.send(numbers_in, 5);
    assert_eq!(*seen.borrow(), [vec![4], vec![4, 5]]);
    assert_eq!(*graph.sample(before), 4);
}
