//! What the engine does with a redefinition the REPL's checks would have
//! refused: the graph API driven directly, past the checks. Both are panics
//! in graph code, so both poison the runtime, which is why checks 3 and 4
//! run on the REPL's side before anything touches the graph.

mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};

use bough::SendError;
use bough_repl::graph::{Arg, Def, Graph};
use bough_repl::ty::Literal;
use common::{apply, define, int_input};

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
    let (a, a_in) = int_input(&mut graph, 1);
    let one = || Arg::Literal(Literal::Int(1));
    let (x, x_redefine) = define(&mut graph, apply("add", vec![Arg::Binding(a), one()]));
    let (y, _) = define(&mut graph, apply("add", vec![Arg::Binding(x), one()]));
    let message = panic_message(|| {
        graph.redefine(x_redefine, apply("add", vec![Arg::Binding(y), one()]));
    });
    assert!(message.contains("cycle"), "{message}");
    assert_eq!(graph.runtime().try_send(a_in, 2), Err(SendError::Poisoned));
}

#[test]
fn a_definition_of_another_type_panics_in_the_construct_and_poisons_the_runtime() {
    let mut graph = Graph::new();
    let (a, a_in) = int_input(&mut graph, 1);
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
    assert_eq!(graph.runtime().try_send(a_in, 2), Err(SendError::Poisoned));
}
