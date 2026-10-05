//! Mechanism B, staging, under `--features stage`: with rollback on,
//! every check that can refuse an edit runs before anything commits, and
//! a construct closure returns a `Result`, so a refusal throws away what
//! the instant made, before it joined the dependents of anything older,
//! and has nothing committed to undo. No panic is caught. The REPL's
//! constructs are `try_construct`s here, and its wiring returns a
//! mismatch rather than reaching an `unreachable!`.
//!
//! F1, F2 and F5 are covered, with the handoff's criteria as in mechanism
//! A's tests. F3 and F4 aren't: `boom` panics, and a panic in a value is
//! out of B's reach, so they still poison, unless `undo` is on too.

#![cfg(feature = "stage")]

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use bough::{CollectionPolicy, Runtime, Source};
use bough_repl::graph::{Arg, Command, Def, Made};
use bough_repl::ty::{Literal, Node};
use common::{
    Log, apply, assert_as_before, before, define, int_input, refusal, rollback_graph, sample_int,
    watch,
};

fn int(n: i64) -> Arg {
    Arg::Literal(Literal::Int(n))
}

/// F1's bindings: `x = add a 1` and `y = add x 1`, with `a` at 1.
fn chain(graph: &mut bough_repl::graph::Graph) -> (Node, Node, bough::Input<Def>, Node) {
    let (a, _) = int_input(graph, 1);
    let (x, x_redefine) = define(graph, apply("add", vec![Arg::Binding(a), int(1)]));
    let (y, _) = define(graph, apply("add", vec![Arg::Binding(x), int(1)]));
    (a, x, x_redefine, y)
}

/// F1 with `y` watched or not: the switches move before commit, after the
/// last new node has run and before force, and the cycle check refuses,
/// either way the same.
fn f1(watched: bool) {
    let mut graph = rollback_graph();
    let (a, x, x_redefine, y) = chain(&mut graph);
    let log = Log::default();
    watch(&mut graph, &log, "a", a);
    if watched {
        watch(&mut graph, &log, "y", y);
    }
    let nodes = [a, x, y];
    let before = before(&mut graph, &nodes);
    log.borrow_mut().clear();
    let refusal =
        refusal(graph.try_redefine(x_redefine, apply("add", vec![Arg::Binding(y), int(1)])));
    assert!(
        refusal
            .message
            .contains("switching closes a same-instant cycle"),
        "{refusal}"
    );
    assert_eq!(refusal.node, None);
    assert_as_before(&mut graph, &nodes, &before);
    assert!(log.borrow().is_empty(), "{:?}", log.borrow());
    graph
        .try_redefine(x_redefine, apply("add", vec![Arg::Binding(a), int(5)]))
        .unwrap();
    assert_eq!(sample_int(&mut graph, y), 7);
}

#[test]
fn f1_a_cycle_is_refused_before_commit() {
    f1(false);
}

#[test]
fn f1_watched_a_cycle_is_refused_before_commit_the_same_way() {
    f1(true);
}

/// F2 at a binding: its construct's closure returns the mismatch, and the
/// instant is refused before anything commits.
#[test]
fn f2_a_construct_closure_s_error_is_refused() {
    let mut graph = rollback_graph();
    let (a, _) = int_input(&mut graph, 1);
    let (b, b_redefine) = define(&mut graph, apply("add", vec![Arg::Binding(a), int(1)]));
    let log = Log::default();
    watch(&mut graph, &log, "b", b);
    let nodes = [a, b];
    let before = before(&mut graph, &nodes);
    log.borrow_mut().clear();
    // `not a` is a Bool, built over `a`: the new node depends on an older
    // one, so its link waits for commit, which never comes.
    let not = Def::Apply(
        |b, args| {
            let Node::IntCell(a) = args[0] else {
                unreachable!()
            };
            Ok(Node::BoolCell(a.map_cell(b, |n| *n > 0)))
        },
        vec![Arg::Binding(a)],
    );
    let refusal = refusal(graph.try_redefine(b_redefine, not));
    assert!(
        refusal.message.contains("the checks passed a BoolCell"),
        "{refusal}"
    );
    assert!(refusal.node.is_some(), "the binding's construct: {refusal}");
    assert_as_before(&mut graph, &nodes, &before);
    assert!(log.borrow().is_empty(), "{:?}", log.borrow());
    graph
        .try_redefine(b_redefine, apply("mul", vec![Arg::Binding(a), int(10)]))
        .unwrap();
    assert_eq!(*log.borrow(), ["b = 10"]);
}

/// F2 at the root: the wiring's mismatch is the root construct's error.
#[test]
fn f2_the_root_construct_s_error_is_refused_and_it_makes_the_next_binding() {
    let mut graph = rollback_graph();
    let Made::Input(made) = graph.try_make(Command::Input(Literal::Bool(true))).unwrap() else {
        unreachable!()
    };
    let (p, _) = made.keep();
    let before = before(&mut graph, &[]);
    let refusal = refusal(
        graph
            .try_make(Command::Define(apply("add", vec![Arg::Binding(p), int(1)])))
            .map(drop),
    );
    assert!(
        refusal.message.contains("the checks passed a BoolCell"),
        "{refusal}"
    );
    assert_as_before(&mut graph, &[], &before);
    let (k, _) = define(&mut graph, apply("add", vec![int(2), int(3)]));
    assert_eq!(sample_int(&mut graph, k), 5);
}

/// F5 with an inner `try_construct`: its error, at the instant that built
/// it, refuses the instant, and the outer construct's nodes go with it.
#[test]
fn f5_a_nested_construct_s_error_takes_the_outer_construct_s_nodes_with_it() {
    let (mut runtime, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<i64>();
        let numbers = numbers.share(b);
        let made = numbers.construct(b, move |b, n| {
            let first = b.constant(n);
            let inner = numbers.try_construct(b, move |b, m: i64| {
                if m == n && n % 7 == 0 {
                    return Err(format!("the inner construct built on {n}"));
                }
                Ok(b.constant(m * 10))
            });
            let latest = inner.hold(b, first).switch_cell(b);
            b.anchor(latest)
        });
        (numbers_in, made)
    });
    runtime.set_rollback(true);
    runtime.set_collection_policy(CollectionPolicy::Manual);
    let (numbers_in, made) = edge.keep();
    let latest = Rc::new(RefCell::new(None));
    let slot = latest.clone();
    runtime
        .listen(made, move |cell| *slot.borrow_mut() = Some(cell))
        .keep();
    runtime.send(numbers_in, 2);
    let first = latest
        .borrow_mut()
        .take()
        .expect("the outer construct ran")
        .keep();
    runtime.collect_garbage();
    let (topology, live) = (runtime.topology(), runtime.live_nodes());
    let refusal = refusal(runtime.try_send(numbers_in, 7));
    assert_eq!(refusal.message, "the inner construct built on 7");
    assert!(refusal.node.is_some(), "the inner construct: {refusal}");
    assert_eq!(runtime.live_nodes(), live, "freed at once");
    assert_eq!(runtime.topology(), topology);
    assert!(latest.borrow().is_none());
    assert_eq!(*runtime.sample(first), 20);
    runtime.try_send(numbers_in, 3).unwrap();
    let cell = latest
        .borrow_mut()
        .take()
        .expect("the outer construct ran")
        .keep();
    assert_eq!(*runtime.sample(cell), 30);
    assert_eq!(*runtime.sample(first), 30);
}

/// With rollback off, a closure's error poisons, as a panic in it would.
#[test]
fn with_rollback_off_a_closure_s_error_poisons() {
    let mut graph = bough_repl::graph::Graph::new();
    let (a, a_in) = int_input(&mut graph, 1);
    let (_, b_redefine) = define(&mut graph, apply("add", vec![Arg::Binding(a), int(1)]));
    let message = common::panic_message(|| {
        let _ = graph.try_redefine(b_redefine, Def::Alias(Arg::Literal(Literal::Bool(true))));
    });
    assert!(message.contains("returned an error"), "{message}");
    assert_eq!(
        graph.runtime().try_send(a_in, 2),
        Err(bough::SendError::Poisoned)
    );
}

/// What B can't reach: `boom` panics in a value, after every check, and
/// B catches no panic, so F3 still poisons. With `undo` on too, A catches
/// it, and `rollback_undo.rs` covers it.
#[cfg(not(feature = "undo"))]
#[test]
fn f3_a_panic_in_a_value_still_poisons() {
    let mut graph = rollback_graph();
    let (a, a_in) = int_input(&mut graph, 7);
    let (b, b_redefine) = define(&mut graph, apply("add", vec![Arg::Binding(a), int(1)]));
    let log = Log::default();
    watch(&mut graph, &log, "b", b);
    let message = common::panic_message(|| {
        let _ = graph.try_redefine(b_redefine, apply("boom", vec![Arg::Binding(a)]));
    });
    assert!(message.contains("boom on 7"), "{message}");
    assert_eq!(
        graph.runtime().try_send(a_in, 2),
        Err(bough::SendError::Poisoned)
    );
}
