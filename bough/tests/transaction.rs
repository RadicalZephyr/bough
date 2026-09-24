//! The transaction's entry points: transaction zero, sends, double sends,
//! foreign tokens and poisoning (RFD 5).

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};

use bough::{Graph, PoisonedError, SendError, TransactionSendError};

/// The message of a caught panic.
fn panic_text(result: Result<impl Sized, Box<dyn Any + Send>>) -> String {
    let payload = match result {
        Ok(_) => panic!("expected a panic"),
        Err(payload) => payload,
    };
    if let Some(text) = payload.downcast_ref::<&str>() {
        text.to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        String::new()
    }
}

#[test]
fn the_build_closure_runs_as_transaction_zero_and_returns_its_edge() {
    let (graph, (_numbers_in, _limit, _never)) = Graph::build(|b| {
        let (_numbers, numbers_in) = b.input::<u32>();
        let limit = b.constant(10u32);
        let never = b.never::<u32>();
        (numbers_in, limit, never)
    });
    assert_eq!(graph.live_nodes(), 3, "one node per input, constant, never");
}

#[test]
fn a_second_send_to_a_non_coalescing_input_is_a_double_send() {
    let (mut graph, (a_in, b_in)) = Graph::build(|b| {
        let (_a, a_in) = b.input::<u32>();
        let (_b, b_in) = b.input::<u32>();
        (a_in, b_in)
    });
    let results = graph.transaction(|tx| {
        (
            tx.try_send(a_in, 1),
            tx.try_send(b_in, 2),
            tx.try_send(a_in, 3),
        )
    });
    assert_eq!(
        results,
        (Ok(()), Ok(()), Err(TransactionSendError::DoubleSend))
    );
    // The transaction went on without the refused value and finished.
    assert_eq!(graph.try_send(a_in, 4), Ok(()));
}

#[test]
fn a_double_send_through_send_panics_and_poisons_the_graph() {
    let (mut graph, a_in) = Graph::build(|b| b.input::<u32>().1);
    let result = catch_unwind(AssertUnwindSafe(|| {
        graph.transaction(|tx| {
            tx.send(a_in, 1);
            tx.send(a_in, 2);
        })
    }));
    assert!(panic_text(result).contains("a second send to a non-coalescing input"));
    assert_eq!(graph.try_send(a_in, 3), Err(SendError::Poisoned));
    assert_eq!(graph.try_transaction(|_| ()), Err(PoisonedError));
    let again = catch_unwind(AssertUnwindSafe(|| graph.send(a_in, 4)));
    assert!(panic_text(again).contains("poisoned"));
}

#[test]
fn a_coalescing_input_accepts_several_sends_in_one_transaction() {
    let (mut graph, a_in) = Graph::build(|b| b.input_coalescing(|x: u32, y| x + y).1);
    let results = graph.transaction(|tx| (tx.try_send(a_in, 1), tx.try_send(a_in, 2)));
    assert_eq!(results, (Ok(()), Ok(())));
    graph.transaction(|tx| {
        tx.send(a_in, 1);
        tx.send(a_in, 2);
        tx.send(a_in, 3);
    });
}

#[test]
fn a_panic_in_a_coalescing_function_poisons_the_graph() {
    let (mut graph, a_in) = Graph::build(|b| {
        b.input_coalescing(|_: u32, _: u32| -> u32 { panic!("user code") })
            .1
    });
    let result = catch_unwind(AssertUnwindSafe(|| {
        graph.transaction(|tx| {
            tx.send(a_in, 1);
            tx.send(a_in, 2);
        })
    }));
    assert_eq!(panic_text(result), "user code");
    assert_eq!(graph.try_send(a_in, 3), Err(SendError::Poisoned));
}

#[test]
fn a_token_from_another_graph_is_an_error_and_leaves_the_graph_usable() {
    let (mut first, first_in) = Graph::build(|b| b.input::<u32>().1);
    let (mut second, second_in) = Graph::build(|b| b.input::<u32>().1);
    assert_eq!(first.try_send(second_in, 1), Err(SendError::ForeignGraph));
    assert_eq!(
        first.transaction(|tx| tx.try_send(second_in, 1)),
        Err(TransactionSendError::ForeignGraph)
    );
    // The panicking send checks the token before the transaction opens, so
    // the graph is not poisoned.
    let result = catch_unwind(AssertUnwindSafe(|| first.send(second_in, 1)));
    assert!(panic_text(result).contains("a token from another graph"));
    assert_eq!(first.try_send(first_in, 1), Ok(()));
    assert_eq!(second.try_send(second_in, 1), Ok(()));
}

#[test]
fn a_foreign_token_in_graph_code_panics() {
    let (_other, stranger) = Graph::build(|b| b.constant(1u32));
    let result = catch_unwind(|| {
        Graph::build(|b| {
            let _ = stranger.sample(b);
        })
    });
    assert!(panic_text(result).contains("a token from another graph"));
}

#[test]
fn swapping_the_build_context_for_another_graphs_is_caught() {
    let result = catch_unwind(|| {
        Graph::build(|outer| {
            Graph::build(|inner| core::mem::swap(outer, inner));
        })
    });
    assert!(panic_text(result).contains("swapped for another graph's"));
}

#[cfg(feature = "statistics")]
#[test]
fn statistics_count_instants_and_nothing_else_on_an_unconsumed_input() {
    let (mut graph, a_in) = Graph::build(|b| b.input::<u32>().1);
    let built: bough::Statistics = graph.statistics();
    assert_eq!(built.transactions, 1, "transaction zero");
    assert_eq!(built.new_nodes, 1, "the input ran once in transaction zero");
    graph.send(a_in, 1);
    graph.transaction(|_| ());
    let after = graph.statistics();
    assert_eq!(after.transactions, 3);
    assert_eq!(after.ordered, 0, "nothing depends on the input");
    assert_eq!(after.evaluations, 0);
}
