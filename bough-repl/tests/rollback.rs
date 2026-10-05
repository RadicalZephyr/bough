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

/// A watch downstream changes nothing: commit, where the switch moves and
/// the cycle is found, comes before the listeners.
#[test]
fn f1_watched_the_cycle_is_still_found_at_the_switch_move() {
    let mut graph = Graph::new();
    let (a_in, x_redefine, y) = chain(&mut graph);
    let log = Log::default();
    watch(&mut graph, &log, "y", y);
    let message = panic_message(|| {
        graph.redefine(x_redefine, apply("add", vec![Arg::Binding(y), int(1)]));
    });
    assert!(
        message.contains("switching closes a same-instant cycle"),
        "{message}"
    );
    assert_eq!(*log.borrow(), ["y = 3"], "only the watch's first line");
    assert_eq!(graph.runtime().try_send(a_in, 2), Err(SendError::Poisoned));
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

/// Where F3 fails: `boom` is a `map_cell`, whose function runs when the
/// cell is first read after commit, which is a watch's listener. With
/// another binding redefined in the same transaction, that binding's
/// watcher has printed first in some listener orders: a side effect of a
/// transaction that then failed.
#[test]
fn f3_the_panic_comes_in_dispatch_after_another_watcher_printed_in_some_orders() {
    let printed: Vec<u64> = (0..32)
        .filter(|&seed| {
            let mut graph = Graph::new();
            graph.runtime().set_shuffle_seed(Some(seed));
            let (a, _) = int_input(&mut graph, 7);
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
            log.borrow().iter().any(|line| line == "x = 9")
        })
        .collect();
    assert!(
        !printed.is_empty() && printed.len() < 32,
        "x printed under seeds {printed:?}"
    );
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

/// As in F3, the panic comes in dispatch: a watch on a sibling of `b`
/// prints for the seventh tick in some listener orders, though the tick's
/// transaction fails.
#[test]
fn f4_a_sibling_watcher_prints_for_the_failing_tick_in_some_orders() {
    let printed: Vec<u64> = (0..32)
        .filter(|&seed| {
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
        })
        .collect();
    assert!(
        !printed.is_empty() && printed.len() < 32,
        "c printed under seeds {printed:?}"
    );
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
