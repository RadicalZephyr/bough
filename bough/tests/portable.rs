//! A core written once against `Portable` runs under both modes, and a
//! `Local` host mixes it with graph code of its own that is not `Send`.

use std::cell::RefCell;
use std::rc::Rc;

use bough::{Cell, Portable, Runtime, Source, Stream};

// ----------------------------------------------------------- the smallest core

/// Finding F23's shape: a core that writes its own closure and stores it.
fn plus_one(b: &mut Portable<'_>, numbers: Stream<u32>) -> Cell<u32> {
    numbers.map(|n| n + 1).hold(b, 0)
}

#[test]
fn a_core_that_stores_its_own_closure_builds_in_both_modes() {
    let (mut local, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, plus_one(&mut b.portable(), numbers))
    });
    let (local_in, local_out) = edge.keep();

    let (mut threaded, edge) = Runtime::build_threaded(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, plus_one(&mut b.portable(), numbers))
    });
    let (threaded_in, threaded_out) = edge.keep();

    for n in [1, 5, 41] {
        local.send(local_in, n);
        threaded.send(threaded_in, n);
        assert_eq!(*local.sample(local_out), n + 1);
        assert_eq!(*threaded.sample(threaded_out), n + 1);
    }
}

// ----------------------------------------------------------- a GTK-shaped host

/// A core the shell feeds: it counts clicks in steps the shell's cell
/// gives, and reports the total and each new total.
fn counter(b: &mut Portable<'_>, clicks: Stream<()>, step: Cell<u32>) -> (Cell<u32>, Stream<u32>) {
    let total = clicks
        .snapshot(step, |(), s| *s)
        .accumulate(b, 0u32, |s, t| t + s);
    (total, total.steps(b))
}

/// A second core the shell feeds with a cell it built from the first
/// core's output.
fn longest(b: &mut Portable<'_>, label: Cell<String>) -> Cell<usize> {
    label
        .steps(b)
        .map(|l| l.len())
        .accumulate(b, 0usize, |n, m| n.max(*m))
}

/// RFD 6's GTK frontend: a `Local` build holds graph code that is not
/// `Send`, here an `Rc` captured in a closure the graph stores, beside a
/// portable core's outputs. Tokens pass both ways: the shell's input and
/// cell into the core, the core's cell and stream into the shell, and the
/// shell's `Rc`-built cell into a second core.
#[test]
fn a_local_host_mixes_graph_code_that_is_not_send_with_a_portable_core() {
    let (mut graph, edge) = Runtime::build(|b| {
        let prefix: Rc<str> = Rc::from("total ");
        let (clicks, clicks_in) = b.input::<()>();
        let (step, step_in) = b.input_cell(1u32);

        // Shell into core.
        let (total, totals) = counter(&mut b.portable(), clicks, step);

        // Core into shell: a closure capturing an `Rc`, stored in the graph.
        let label = totals
            .map(move |t| format!("{prefix}{t}"))
            .hold(b, String::from("none"));
        let seen = Rc::new(RefCell::new(0usize));
        let reads = seen.clone();
        let logged = label.map_cell(b, move |l| {
            *reads.borrow_mut() += 1;
            l.len()
        });

        // The shell's `Rc`-built cell into a second core.
        let widest = longest(&mut b.portable(), label);
        (clicks_in, step_in, total, label, logged, widest)
    });
    let (clicks_in, step_in, total, label, logged, widest) = edge.keep();

    graph.send(clicks_in, ());
    graph.send(step_in, 10);
    graph.send(clicks_in, ());
    graph.send(clicks_in, ());
    assert_eq!(*graph.sample(total), 21);
    assert_eq!(graph.sample(label), "total 21");
    assert_eq!(*graph.sample(logged), "total 21".len());
    assert_eq!(*graph.sample(widest), "total 21".len());
}
