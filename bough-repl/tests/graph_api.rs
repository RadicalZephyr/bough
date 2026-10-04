//! What the graph API can do that the REPL's one-command-per-line surface
//! doesn't use: several redefinitions in one transaction.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use bough_repl::graph::{Arg, Graph};
use bough_repl::ty::{Literal, Node};
use common::{apply, define, int_input};

fn int(n: i64) -> Arg {
    Arg::Literal(Literal::Int(n))
}

#[test]
fn redefinitions_sent_in_one_transaction_land_in_one_instant() {
    let mut graph = Graph::new();
    let (a, a_in) = int_input(&mut graph, 1);
    let (x, x_redefine) = define(&mut graph, apply("add", vec![Arg::Binding(a), int(1)]));
    let (y, y_redefine) = define(&mut graph, apply("mul", vec![Arg::Binding(x), int(2)]));
    let Node::IntCell(y_cell) = y else {
        unreachable!()
    };
    let seen = Rc::new(RefCell::new(Vec::new()));
    let log = seen.clone();
    graph
        .runtime()
        .listen_steps(y_cell, move |n| log.borrow_mut().push(*n))
        .keep();
    // Each binding has an input of its own, so the two sends are
    // simultaneous: `y` steps once, to the value both make together, and
    // never shows one redefinition without the other.
    graph.runtime().transaction(|tx| {
        tx.send(x_redefine, apply("add", vec![Arg::Binding(a), int(2)]));
        tx.send(y_redefine, apply("mul", vec![Arg::Binding(x), int(3)]));
    });
    assert_eq!(*seen.borrow(), [9]);
    graph.runtime().send(a_in, 10);
    assert_eq!(*seen.borrow(), [9, 36]);
}
