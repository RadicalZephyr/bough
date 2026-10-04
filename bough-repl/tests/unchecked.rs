//! What the engine does with a redefinition the REPL's checks would have
//! refused: the graph API driven directly, past the checks. Both are panics
//! in graph code, so both poison the runtime, which is why checks 3 and 4
//! run on the REPL's side before anything touches the graph.

use std::panic::{AssertUnwindSafe, catch_unwind};

use bough::{Input, SendError};
use bough_repl::graph::{Arg, Command, Def, Graph, Made};
use bough_repl::registry;
use bough_repl::ty::{InputToken, Literal, Node};

fn apply(function: &str, args: Vec<Arg>) -> Def {
    Def::Apply(registry::lookup(function).unwrap().wire, args)
}

fn define(graph: &mut Graph, def: Def) -> (Node, Input<Def>) {
    match graph.make(Command::Define(def)) {
        Made::Defined(made) => made.keep(),
        Made::Input(_) => unreachable!(),
    }
}

fn input(graph: &mut Graph, literal: Literal) -> (Node, InputToken) {
    match graph.make(Command::Input(literal)) {
        Made::Input(made) => made.keep(),
        Made::Defined(_) => unreachable!(),
    }
}

fn panic_message(f: impl FnOnce()) -> String {
    let payload = catch_unwind(AssertUnwindSafe(f)).expect_err("a panic");
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => payload.downcast::<&str>().unwrap().to_string(),
    }
}

#[test]
fn a_cycle_through_two_switches_panics_at_the_move_and_poisons_the_runtime() {
    let mut graph = Graph::new();
    let (a, a_in) = input(&mut graph, Literal::Int(1));
    let one = || Arg::Literal(Literal::Int(1));
    let (x, x_redefine) = define(&mut graph, apply("add", vec![Arg::Binding(a), one()]));
    let (y, _) = define(&mut graph, apply("add", vec![Arg::Binding(x), one()]));
    let message = panic_message(|| {
        graph.redefine(x_redefine, apply("add", vec![Arg::Binding(y), one()]));
    });
    assert!(message.contains("cycle"), "{message}");
    let InputToken::Int(a_in) = a_in else {
        unreachable!()
    };
    assert_eq!(graph.runtime().try_send(a_in, 2), Err(SendError::Poisoned));
}

#[test]
fn a_definition_of_another_type_panics_in_the_construct_and_poisons_the_runtime() {
    let mut graph = Graph::new();
    let (a, a_in) = input(&mut graph, Literal::Int(1));
    let (_, b_redefine) = define(
        &mut graph,
        apply("add", vec![Arg::Binding(a), Arg::Literal(Literal::Int(1))]),
    );
    let message = panic_message(|| {
        graph.redefine(b_redefine, Def::Alias(Arg::Literal(Literal::Bool(true))));
    });
    assert!(
        message.contains("the checks passed a BoolCell"),
        "{message}"
    );
    let InputToken::Int(a_in) = a_in else {
        unreachable!()
    };
    assert_eq!(graph.runtime().try_send(a_in, 2), Err(SendError::Poisoned));
}
