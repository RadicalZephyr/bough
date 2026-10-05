//! Mechanism A, the undo log, under `--features undo`: with rollback on,
//! the engine catches a panic, undoes everything the instant did, commit
//! included, and refuses the transaction. For F1, F2, F3 and F5, each test
//! shows the handoff's criteria: the runtime isn't poisoned; the live-node
//! count, every live node's structure, switches included, and every
//! binding's value are as they were before, with no collection between;
//! no watcher printed for the refused transaction; and a later
//! redefinition of the same binding works.

#![cfg(feature = "undo")]

mod common;

use std::cell::{Cell as StdCell, RefCell};
use std::rc::Rc;

use bough::{CollectionPolicy, Runtime, SendError, Source};
use bough_repl::graph::{Arg, Command, Def, Made};
use bough_repl::ty::{Literal, Node};
use bough_repl::{Repl, registry};
use common::{
    Log, apply, assert_as_before, before, define, int_input, listing, panic_message, refusal,
    rollback_graph, sample_int, transcript, watch,
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

/// Unwatched, the cycle is found at the switch's move, after every hold
/// has committed, so this is the undo log's whole path: the parked values
/// back, the moves reversed, the memos forgotten, the new nodes freed.
#[test]
fn f1_a_cycle_found_at_the_switch_move_is_undone_after_commit() {
    let mut graph = rollback_graph();
    let (a, x, x_redefine, y) = chain(&mut graph);
    let log = Log::default();
    watch(&mut graph, &log, "a", a);
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
    assert_eq!(refusal.node, None, "no node's code runs at a switch's move");
    assert_as_before(&mut graph, &nodes, &before);
    assert!(log.borrow().is_empty(), "{:?}", log.borrow());
    graph
        .try_redefine(x_redefine, apply("add", vec![Arg::Binding(a), int(5)]))
        .unwrap();
    assert_eq!(sample_int(&mut graph, y), 7);
}

/// Watched, force finds the cycle before commit, so less is undone.
#[test]
fn f1_watched_a_cycle_found_before_commit_is_undone() {
    let mut graph = rollback_graph();
    let (a, x, x_redefine, y) = chain(&mut graph);
    let log = Log::default();
    watch(&mut graph, &log, "y", y);
    let nodes = [a, x, y];
    let before = before(&mut graph, &nodes);
    log.borrow_mut().clear();
    let refusal =
        refusal(graph.try_redefine(x_redefine, apply("add", vec![Arg::Binding(y), int(1)])));
    // With `stage` on too, the switch's move is checked before force runs,
    // and finds the cycle first.
    let found = if cfg!(feature = "stage") {
        "switching closes a same-instant cycle"
    } else {
        "a same-instant cycle through a read after the instant"
    };
    assert!(refusal.message.contains(found), "{refusal}");
    assert_as_before(&mut graph, &nodes, &before);
    assert!(log.borrow().is_empty(), "{:?}", log.borrow());
    graph
        .try_redefine(x_redefine, apply("add", vec![Arg::Binding(a), int(5)]))
        .unwrap();
    assert_eq!(*log.borrow(), ["y = 7"]);
}

/// The construct's program survives its closure's panic, so the binding
/// can be redefined again: without the catch around a node's evaluation,
/// the unwind would drop it.
#[test]
fn f2_a_construct_closure_that_panics_is_undone_and_its_program_kept() {
    let mut graph = rollback_graph();
    let (a, _) = int_input(&mut graph, 1);
    let (b, b_redefine) = define(&mut graph, apply("add", vec![Arg::Binding(a), int(1)]));
    let log = Log::default();
    watch(&mut graph, &log, "b", b);
    let nodes = [a, b];
    let before = before(&mut graph, &nodes);
    log.borrow_mut().clear();
    let refusal =
        refusal(graph.try_redefine(b_redefine, Def::Alias(Arg::Literal(Literal::Bool(true)))));
    assert!(
        refusal.message.contains("the checks passed a BoolCell"),
        "{refusal}"
    );
    assert!(refusal.node.is_some(), "the binding's construct: {refusal}");
    assert_eq!(
        refusal.inputs.len(),
        1,
        "the binding's input of definitions"
    );
    assert_as_before(&mut graph, &nodes, &before);
    assert!(log.borrow().is_empty(), "{:?}", log.borrow());
    graph
        .try_redefine(b_redefine, apply("mul", vec![Arg::Binding(a), int(10)]))
        .unwrap();
    assert_eq!(*log.borrow(), ["b = 10"]);
}

/// The root construct, which every new binding goes through, keeps its
/// program too, so the next binding is made.
#[test]
fn f2_the_root_construct_is_undone_and_makes_the_next_binding() {
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

/// F3 beside another redefinition, under every seed: with force, `boom`
/// fails before any listener runs, and the undo log takes back both
/// redefinitions, the one that would have worked too, since they were
/// one transaction. A refused `transaction` panics with the refusal and
/// leaves the runtime usable.
#[test]
fn f3_a_definition_that_fails_on_the_current_value_is_undone_before_any_watcher_prints() {
    for seed in 0..32 {
        let mut graph = rollback_graph();
        graph.runtime().set_shuffle_seed(Some(seed));
        let (a, _) = int_input(&mut graph, 7);
        let (x, x_redefine) = define(&mut graph, apply("add", vec![Arg::Binding(a), int(1)]));
        let (b, b_redefine) = define(&mut graph, apply("add", vec![Arg::Binding(a), int(1)]));
        let (c, _) = define(
            &mut graph,
            apply("mul", vec![Arg::Binding(a), Arg::Binding(b)]),
        );
        let log = Log::default();
        for (name, node) in [("x", x), ("b", b), ("c", c)] {
            watch(&mut graph, &log, name, node);
        }
        let nodes = [a, x, b, c];
        let before = before(&mut graph, &nodes);
        log.borrow_mut().clear();
        let message = panic_message(|| {
            graph.runtime().transaction(|tx| {
                tx.send(x_redefine, apply("add", vec![Arg::Binding(a), int(2)]));
                tx.send(b_redefine, apply("boom", vec![Arg::Binding(a)]));
            });
        });
        assert!(
            message.contains("refused") && message.contains("boom on 7"),
            "{message}"
        );
        assert_as_before(&mut graph, &nodes, &before);
        assert!(log.borrow().is_empty(), "seed {seed}: {:?}", log.borrow());
        graph
            .try_redefine(b_redefine, apply("add", vec![Arg::Binding(a), int(2)]))
            .unwrap();
        let mut printed = log.borrow().clone();
        printed.sort();
        assert_eq!(printed, ["b = 9", "c = 63"], "seed {seed}");
    }
}

/// Through the REPL, with rollback on, the refusal is an error line and
/// the old definition stays.
#[test]
fn f3_through_the_repl_the_refusal_is_an_error_and_the_old_definition_stays() {
    let mut repl = Repl::new();
    repl.set_rollback(true);
    transcript(
        &mut repl,
        &[
            ("input a 7", &[]),
            ("def b add a 1", &[]),
            ("def c mul a b", &[]),
            ("watch c", &["c = 56"]),
        ],
    );
    let printed = repl.run("def b boom a");
    assert!(
        printed.len() == 1
            && printed[0].starts_with("error: refused a transaction: node ")
            && printed[0].contains("boom on 7"),
        "{printed:?}"
    );
    transcript(
        &mut repl,
        &[("set a 1", &["c = 2"]), ("def b add a 2", &["c = 3"])],
    );
    assert_eq!(
        listing(&mut repl),
        [
            "a : Int = input 7",
            "b : Int = add a 2",
            "c : Int = mul a b"
        ]
    );
}

/// What A can't refuse is what never ran: unwatched, the bad definition
/// installs, and fails at the first read, outside any transaction.
#[test]
fn f3_unwatched_the_definition_still_installs_and_fails_at_the_next_watch() {
    let mut repl = Repl::new();
    repl.set_rollback(true);
    for line in ["input a 7", "def b add a 1", "def b boom a"] {
        assert_eq!(repl.run(line), Vec::<String>::new(), "`{line}`");
    }
    let message = panic_message(|| {
        repl.run("watch b");
    });
    assert!(message.contains("boom on 7"), "{message}");
    assert_eq!(repl.run("set a 8"), Vec::<String>::new());
    assert_eq!(repl.run("watch b"), ["b = 8"]);
}

/// F5: the inner construct's failure takes everything the outer one made
/// at the instant with it, and the outer construct's event never reaches
/// its listener. The inner construct the first event built runs on 7 too,
/// before the new one fails, and its step is undone with the rest. The inner construct the first event built runs on 7 too,
/// before the new one fails, and its step is undone with the rest.
#[test]
fn f5_a_nested_construct_that_fails_takes_the_outer_construct_s_nodes_with_it() {
    let (mut runtime, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<i64>();
        let numbers = numbers.share(b);
        let made = numbers.construct(b, move |b, n| {
            let first = b.constant(n);
            // It fails only at the instant that built it, on 7, so one
            // built by an earlier event runs on.
            let inner = numbers.construct(b, move |b, m: i64| {
                assert!(m != n || n % 7 != 0, "the inner construct built on {n}");
                b.constant(m * 10)
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
    assert!(
        refusal.message.contains("the inner construct built on 7"),
        "{refusal}"
    );
    assert!(refusal.node.is_some(), "the inner construct: {refusal}");
    assert_eq!(
        runtime.live_nodes(),
        live,
        "freed at once, with no collection"
    );
    assert_eq!(runtime.topology(), topology);
    assert!(
        latest.borrow().is_none(),
        "the outer construct's listener heard nothing"
    );
    assert_eq!(
        *runtime.sample(first),
        20,
        "the first inner construct's step undone"
    );
    runtime.try_send(numbers_in, 3).unwrap();
    let cell = latest
        .borrow_mut()
        .take()
        .expect("the outer construct ran")
        .keep();
    assert_eq!(*runtime.sample(cell), 30);
}

/// A cell's value after a refused instant, computed before the failure
/// for a listener, isn't left beside its memo, where commit would promote
/// it the next time the cell steps with no listener to compute a new one.
/// Whether `c` is computed before `d` fails depends on the seed.
#[test]
fn a_value_computed_for_after_a_refused_instant_is_forgotten() {
    for seed in 0..16 {
        let (mut runtime, edge) = Runtime::build(|b| {
            let (a, a_in) = b.input_cell(1i64);
            let c = a.map_cell(b, |x| x + 1);
            let d = a.map_cell(b, |x| registry::boom(*x));
            (a_in, c, d)
        });
        runtime.set_rollback(true);
        runtime.set_shuffle_seed(Some(seed));
        let (a_in, c, d) = edge.keep();
        let listeners = [
            runtime.listen_cell(c, |_| {}),
            runtime.listen_cell(d, |_| {}),
        ];
        let refused = runtime.try_send(a_in, 7);
        assert!(matches!(refused, Err(SendError::Refused(_))), "{refused:?}");
        drop(listeners);
        runtime.try_send(a_in, 3).unwrap();
        assert_eq!(*runtime.sample(c), 4, "seed {seed}");
    }
}

/// The log is the engine's: what a closure keeps of its own, it keeps. A
/// construct closure counted its run before it panicked, and the count
/// stands after the roll back.
#[test]
fn a_closure_s_own_state_is_outside_the_log() {
    let runs = Rc::new(StdCell::new(0));
    let counted = runs.clone();
    let (mut runtime, edge) = Runtime::build(move |b| {
        let (numbers, numbers_in) = b.input::<i64>();
        let made = numbers.construct(b, move |b, n| {
            counted.set(counted.get() + 1);
            assert!(n != 7, "the closure on {n}");
            let k = b.constant(n);
            b.anchor(k)
        });
        (numbers_in, made)
    });
    runtime.set_rollback(true);
    let (numbers_in, made) = edge.keep();
    runtime.listen(made, drop).keep();
    let refused = runtime.try_send(numbers_in, 7);
    assert!(matches!(refused, Err(SendError::Refused(_))), "{refused:?}");
    assert_eq!(runs.get(), 1, "the closure's own count isn't rolled back");
    runtime.try_send(numbers_in, 1).unwrap();
    assert_eq!(runs.get(), 2);
}

/// An in-place accumulator's function mutates its state where it is, at
/// commit, so a panic there can't be undone, and poisons as before.
#[test]
fn a_panic_in_accumulate_mut_still_poisons() {
    let (mut runtime, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<i64>();
        let total = numbers.accumulate_mut(b, 0i64, |n, total: &mut i64| {
            assert!(n != 7, "the in-place function on {n}");
            *total += n;
        });
        (numbers_in, total)
    });
    runtime.set_rollback(true);
    let (numbers_in, _total) = edge.keep();
    runtime.try_send(numbers_in, 1).unwrap();
    let message = panic_message(|| {
        let _ = runtime.try_send(numbers_in, 7);
    });
    assert!(message.contains("the in-place function on 7"), "{message}");
    assert_eq!(runtime.try_send(numbers_in, 2), Err(SendError::Poisoned));
}
