//! The rollback probe's failure cases, driven through the REPL where its
//! checks let them through and past it, through the graph API, where they
//! would refuse them:
//!
//! | Case | What fails | Kind |
//! |---|---|---|
//! | F1 | A redefinition closes a cycle, found when the switch moves | Edit |
//! | F2 | A construct closure panics: the `unreachable!` arm of a type mismatch | Edit |
//! | F3 | A redefinition's new definition panics on the current value | Edit |
//! | F4 | An installed definition, `boom`, panics on some values only, driven by a tick | Data |
//! | F5 | A construct built in another construct's run fails at the same instant | Edit |
//!
//! These pin down what the unchanged spike does with each. Every one
//! poisons the runtime, and two of them only after a listener has run.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use bough::{Runtime, SendError, Source};
use bough_repl::Repl;
use bough_repl::graph::{Arg, Command, Def, Graph, Made};
#[cfg(feature = "force")]
use bough_repl::registry;
use bough_repl::ty::{InputToken, Literal, Node};
use common::{
    Log, TICK, apply, define, int_input, panic_message, ticking, watch, with_manual_clock,
};

fn int(n: i64) -> Arg {
    Arg::Literal(Literal::Int(n))
}

/// `x = add a 1` and `y = add x 1`, with `a` at 1: the bindings F1
/// redefines `x` over.
fn chain(graph: &mut Graph) -> (bough::Input<i64>, bough::Input<Def>, Node) {
    let (a, a_in) = int_input(graph, 1);
    let (x, x_redefine) = define(graph, apply("add", vec![Arg::Binding(a), int(1)]));
    let (y, _) = define(graph, apply("add", vec![Arg::Binding(x), int(1)]));
    (a_in, x_redefine, y)
}

#[test]
fn f1_a_cycle_closed_by_a_redefinition_is_found_at_the_switch_move_and_poisons() {
    let mut graph = Graph::new();
    let (a_in, x_redefine, y) = chain(&mut graph);
    let message = panic_message(|| {
        graph.redefine(x_redefine, apply("add", vec![Arg::Binding(y), int(1)]));
    });
    assert!(
        message.contains("switching closes a same-instant cycle"),
        "{message}"
    );
    assert_eq!(graph.runtime().try_send(a_in, 2), Err(SendError::Poisoned));
}

/// F1 with a watch downstream of the cycle, which panics with `message`.
fn f1_watched(message: &str) {
    let mut graph = Graph::new();
    let (a_in, x_redefine, y) = chain(&mut graph);
    let log = Log::default();
    watch(&mut graph, &log, "y", y);
    let panicked = panic_message(|| {
        graph.redefine(x_redefine, apply("add", vec![Arg::Binding(y), int(1)]));
    });
    assert!(panicked.contains(message), "{panicked}");
    assert_eq!(*log.borrow(), ["y = 3"], "only the watch's first line");
    assert_eq!(graph.runtime().try_send(a_in, 2), Err(SendError::Poisoned));
}

/// A watch downstream changes nothing: commit, where the switch moves and
/// the cycle is found, comes before the listeners.
#[cfg(not(feature = "force"))]
#[test]
fn f1_watched_the_cycle_is_still_found_at_the_switch_move() {
    f1_watched("switching closes a same-instant cycle");
}

/// Computing the watched value before commit finds the cycle first, as a
/// read that comes back to a cell it's computing. The switches haven't
/// moved, so it's a read's check that sees the cycle, not the move's.
#[cfg(feature = "force")]
#[test]
fn f1_watched_with_force_the_cycle_is_found_before_commit() {
    f1_watched("a same-instant cycle through a read after the instant");
}

#[test]
fn f2_a_binding_s_construct_closure_that_panics_poisons() {
    let mut graph = Graph::new();
    let (a, a_in) = int_input(&mut graph, 1);
    let (_, b_redefine) = define(&mut graph, apply("add", vec![Arg::Binding(a), int(1)]));
    let message = panic_message(|| {
        graph.redefine(b_redefine, Def::Alias(Arg::Literal(Literal::Bool(true))));
    });
    assert!(
        message.contains("the checks passed a BoolCell"),
        "{message}"
    );
    assert_eq!(graph.runtime().try_send(a_in, 2), Err(SendError::Poisoned));
}

/// The root construct is the one every new binding goes through, so after
/// it panics no command reaches the graph.
#[test]
fn f2_the_root_construct_panicking_poisons_every_later_command() {
    let mut graph = Graph::new();
    let Made::Input(made) = graph.make(Command::Input(Literal::Bool(true))) else {
        unreachable!()
    };
    let (p, InputToken::Bool(_)) = made.keep() else {
        unreachable!()
    };
    let message = panic_message(|| {
        graph.make(Command::Define(apply("add", vec![Arg::Binding(p), int(1)])));
    });
    assert!(
        message.contains("the checks passed a BoolCell"),
        "{message}"
    );
    let message = panic_message(|| {
        graph.make(Command::Input(Literal::Int(0)));
    });
    assert!(message.contains("poisoned"), "{message}");
}

/// The REPL's checks can't refuse what fails only on a value: the types
/// agree and there's no cycle, so `def b boom a` reaches the graph.
#[test]
fn f3_a_definition_that_panics_on_the_current_value_poisons_through_the_repl() {
    let mut repl = Repl::new();
    for line in ["input a 7", "def b add a 1", "def c mul a b"] {
        assert_eq!(repl.run(line), Vec::<String>::new(), "`{line}`");
    }
    assert_eq!(repl.run("watch c"), ["c = 56"]);
    let message = panic_message(|| {
        repl.run("def b boom a");
    });
    assert!(message.contains("boom on 7"), "{message}");
    let message = panic_message(|| {
        repl.run("set a 1");
    });
    assert!(message.contains("poisoned"), "{message}");
}

/// F3 in a transaction that also redefines `x`, with both watched, under
/// shuffle seed `seed`: whether `x`'s watcher printed before the panic.
fn f3_sibling_printed(seed: u64) -> bool {
    let mut graph = Graph::new();
    graph.runtime().set_shuffle_seed(Some(seed));
    let (a, a_in) = int_input(&mut graph, 7);
    let (x, x_redefine) = define(&mut graph, apply("add", vec![Arg::Binding(a), int(1)]));
    let (b, b_redefine) = define(&mut graph, apply("add", vec![Arg::Binding(a), int(1)]));
    let log = Log::default();
    watch(&mut graph, &log, "x", x);
    watch(&mut graph, &log, "b", b);
    log.borrow_mut().clear();
    let message = panic_message(|| {
        graph.runtime().transaction(|tx| {
            tx.send(x_redefine, apply("add", vec![Arg::Binding(a), int(2)]));
            tx.send(b_redefine, apply("boom", vec![Arg::Binding(a)]));
        });
    });
    assert!(message.contains("boom on 7"), "{message}");
    assert_eq!(graph.runtime().try_send(a_in, 2), Err(SendError::Poisoned));
    log.borrow().iter().any(|line| line == "x = 9")
}

/// Where F3 fails: `boom` is a `map_cell`, whose function runs when the
/// cell is first read after commit, which is a watch's listener. With
/// another binding redefined in the same transaction, that binding's
/// watcher has printed first in some listener orders: a side effect of a
/// transaction that then failed.
#[cfg(not(feature = "force"))]
#[test]
fn f3_the_panic_comes_in_dispatch_after_another_watcher_printed_in_some_orders() {
    let printed: Vec<u64> = (0..32).filter(|&seed| f3_sibling_printed(seed)).collect();
    assert!(
        !printed.is_empty() && printed.len() < 32,
        "x printed under seeds {printed:?}"
    );
}

/// With every watched value computed before commit, `boom` fails before
/// any listener runs, under every seed.
#[cfg(feature = "force")]
#[test]
fn f3_with_force_the_panic_comes_before_any_watcher_prints() {
    let printed: Vec<u64> = (0..32).filter(|&seed| f3_sibling_printed(seed)).collect();
    assert!(printed.is_empty(), "x printed under seeds {printed:?}");
}

/// Nothing reads a binding nobody watches, so its new definition's
/// function doesn't run: the edit succeeds and installs a definition that
/// fails, and the failure comes at the next read. A watch's first call
/// runs outside any transaction, so it poisons nothing.
#[test]
fn f3_unwatched_the_definition_installs_and_fails_at_the_next_watch() {
    let mut repl = Repl::new();
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

/// The REPL's sentinel-free FizzBuzz, cut down to `boom` over a tick: the
/// seventh tick kills the program.
#[test]
fn f4_a_tick_boom_panics_on_poisons_the_runtime() {
    let (mut repl, ticks) = with_manual_clock();
    ticking(
        &mut repl,
        &ticks,
        &[
            ("tick t 1000", &[]),
            ("def b boom t", &[]),
            ("watch b", &["b = 0"]),
            (TICK, &["b = 1"]),
            (TICK, &["b = 2"]),
            (TICK, &["b = 3"]),
            (TICK, &["b = 4"]),
            (TICK, &["b = 5"]),
            (TICK, &["b = 6"]),
        ],
    );
    ticks.fire();
    let message = panic_message(|| {
        repl.pump();
    });
    assert!(message.contains("boom on 7"), "{message}");
    let message = panic_message(|| {
        repl.run("def z 1");
    });
    assert!(message.contains("poisoned"), "{message}");
}

/// F4 with a watched sibling of `b`, `c = add t 1`, under shuffle seed
/// `seed`: whether `c`'s watcher printed for the seventh tick, the one
/// that fails.
fn f4_sibling_printed(seed: u64) -> bool {
    let (mut repl, ticks) = with_manual_clock();
    repl.set_shuffle_seed(Some(seed));
    ticking(
        &mut repl,
        &ticks,
        &[
            ("tick t 1000", &[]),
            ("def c add t 1", &[]),
            ("def b boom t", &[]),
            ("watch c", &["c = 1"]),
            ("watch b", &["b = 0"]),
        ],
    );
    for _ in 1..7 {
        ticks.fire();
        repl.pump();
    }
    ticks.fire();
    let message = panic_message(|| {
        repl.pump();
    });
    assert!(message.contains("boom on 7"), "{message}");
    // What the failed pump printed before it panicked.
    repl.run("").iter().any(|line| line == "c = 8")
}

/// As in F3, the panic comes in dispatch: a watch on a sibling of `b`
/// prints for the seventh tick in some listener orders, though the tick's
/// transaction fails.
#[cfg(not(feature = "force"))]
#[test]
fn f4_a_sibling_watcher_prints_for_the_failing_tick_in_some_orders() {
    let printed: Vec<u64> = (0..32).filter(|&seed| f4_sibling_printed(seed)).collect();
    assert!(
        !printed.is_empty() && printed.len() < 32,
        "c printed under seeds {printed:?}"
    );
}

/// With force, the seventh tick fails before any listener runs.
#[cfg(feature = "force")]
#[test]
fn f4_with_force_no_watcher_prints_for_the_failing_tick() {
    let printed: Vec<u64> = (0..32).filter(|&seed| f4_sibling_printed(seed)).collect();
    assert!(printed.is_empty(), "c printed under seeds {printed:?}");
}

/// A `State`'s value after an instant exists only from commit, once its
/// in-place accumulator has run, so force leaves a function over one to
/// dispatch: `boom` over an `accumulate_mut` still fails after a sibling's
/// watcher has printed, in some orders.
#[cfg(feature = "force")]
#[test]
fn with_force_a_function_over_a_state_still_fails_in_dispatch() {
    let printed: Vec<u64> = (0..32)
        .filter(|&seed| {
            let (mut runtime, edge) = Runtime::build(|b| {
                let (numbers, numbers_in) = b.input::<i64>();
                let numbers = numbers.share(b);
                let total = numbers.accumulate_mut(b, 0i64, |n, total: &mut i64| *total += n);
                let boomed = total.map_cell(b, |t| registry::boom(*t));
                let plain = numbers.hold(b, 0i64).map_cell(b, |n| n + 1);
                (numbers_in, boomed, plain)
            });
            let (numbers_in, boomed, plain) = edge.keep();
            runtime.set_shuffle_seed(Some(seed));
            let log = Log::default();
            let l = log.clone();
            runtime
                .listen_cell(boomed, move |n| {
                    l.borrow_mut().push(format!("boomed = {n}"))
                })
                .keep();
            let l = log.clone();
            runtime
                .listen_cell(plain, move |n| l.borrow_mut().push(format!("plain = {n}")))
                .keep();
            runtime.send(numbers_in, 3);
            log.borrow_mut().clear();
            let message = panic_message(|| runtime.send(numbers_in, 4));
            assert!(message.contains("boom on 7"), "{message}");
            log.borrow().iter().any(|line| line == "plain = 5")
        })
        .collect();
    assert!(!printed.is_empty(), "plain printed under seeds {printed:?}");
}

/// The REPL never builds a construct that runs at the instant it was
/// built, so F5 is a graph of its own: an outer construct whose closure
/// builds an inner construct over the same stream, which runs at the same
/// instant, once the outer closure has returned, and panics on 7.
#[test]
fn f5_a_construct_built_in_another_s_run_that_panics_at_the_same_instant_poisons() {
    let (mut runtime, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<i64>();
        let numbers = numbers.share(b);
        let made = numbers.construct(b, move |b, n| {
            let first = b.constant(n);
            let inner = numbers.construct(b, |b, m: i64| {
                assert!(m % 7 != 0, "the inner construct on {m}");
                b.constant(m * 10)
            });
            let latest = inner.hold(b, first).switch_cell(b);
            b.anchor(latest)
        });
        (numbers_in, made)
    });
    let (numbers_in, made) = edge.keep();
    let latest = Rc::new(RefCell::new(None));
    let slot = latest.clone();
    runtime
        .listen(made, move |cell| *slot.borrow_mut() = Some(cell))
        .keep();
    runtime.send(numbers_in, 2);
    let cell = latest
        .borrow_mut()
        .take()
        .expect("the outer construct ran")
        .keep();
    assert_eq!(*runtime.sample(cell), 20, "the inner construct ran at once");
    let message = panic_message(|| runtime.send(numbers_in, 7));
    assert!(message.contains("the inner construct on 7"), "{message}");
    assert_eq!(runtime.try_send(numbers_in, 3), Err(SendError::Poisoned));
}
