//! The transaction's entry points: transaction zero, sends, double sends,
//! foreign tokens and poisoning (RFD 5).

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};

use portia::prelude::*;
use portia::{PoisonedError, SendError, TransactionSendError};

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
    let mut graph = Runtime::local();
    let (_numbers_in, _limit, _never) = graph.build(|b| {
        let (_numbers, numbers_in) = b.input::<u32>();
        let limit = b.constant(10u32);
        let never = b.never::<u32>();
        (numbers_in, limit, never)
    });
    assert_eq!(graph.live_nodes(), 3, "one node per input, constant, never");
}

#[test]
fn a_second_send_to_a_non_coalescing_input_is_a_double_send() {
    let mut graph = Runtime::local();
    let (a_in, b_in) = graph.build(|b| {
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
    let mut graph = Runtime::local();
    let a_in = graph.build(|b| b.input::<u32>().1);
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
    let mut graph = Runtime::local();
    let a_in = graph.build(|b| b.input_coalescing(|x: u32, y| x + y).1);
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
    let mut graph = Runtime::local();
    let a_in = graph.build(|b| {
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
    let mut first = Runtime::local();
    let first_in = first.build(|b| b.input::<u32>().1);
    let mut second = Runtime::local();
    let second_in = second.build(|b| b.input::<u32>().1);
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
    let mut _other = Runtime::local();
    let stranger = _other.build(|b| b.constant(1u32));
    let result = catch_unwind(|| {
        Runtime::local().build(|b| {
            let _ = stranger.sample(b);
        })
    });
    assert!(panic_text(result).contains("a token from another graph"));
}

#[test]
fn swapping_the_build_context_for_another_graphs_is_caught() {
    let result = catch_unwind(|| {
        Runtime::local().build(|outer| {
            Runtime::local().build(|inner| core::mem::swap(outer, inner));
        })
    });
    assert!(panic_text(result).contains("swapped for another graph's"));
}

#[cfg(feature = "statistics")]
#[test]
fn statistics_count_instants_and_nothing_else_on_an_unconsumed_input() {
    let mut graph = Runtime::local();
    let a_in = graph.build(|b| b.input::<u32>().1);
    let built: portia::Statistics = graph.statistics();
    assert_eq!(built.transactions, 1, "transaction zero");
    assert_eq!(built.new_nodes, 1, "the input ran once in transaction zero");
    graph.send(a_in, 1);
    graph.transaction(|_| ());
    let after = graph.statistics();
    assert_eq!(after.transactions, 3);
    assert_eq!(after.ordered, 0, "nothing depends on the input");
    assert_eq!(after.evaluations, 0);
}

/// An empty runtime from each constructor, then a build, then a send and a
/// listener: the runtime is made first, and the build fixes nothing about
/// it but what it holds.
#[test]
fn an_empty_runtime_from_each_constructor_builds_sends_and_listens() {
    let mut local = Runtime::local();
    assert_eq!(local.live_nodes(), 0, "empty");
    let (numbers_in, total) = local.build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.accumulate(b, 0u32, |n, t| t + n))
    });
    let heard = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = heard.clone();
    local
        .listen_steps(total, move |t| sink.borrow_mut().push(*t))
        .keep();
    local.send(numbers_in, 2);
    local.send(numbers_in, 3);
    assert_eq!(*heard.borrow(), [2, 5]);

    let mut threaded = Runtime::threaded();
    assert_eq!(threaded.live_nodes(), 0, "empty");
    let (numbers_in, total) = threaded.build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.accumulate(b, 0u32, |n, t| t + n))
    });
    let heard = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = heard.clone();
    threaded
        .listen_steps(total, move |t| sink.lock().unwrap().push(*t))
        .keep();
    threaded.send(numbers_in, 2);
    threaded.send(numbers_in, 3);
    assert_eq!(*heard.lock().unwrap(), [2, 5]);
}

/// A later build on a running runtime is the semantics' `Execute` fired
/// once by an input nothing else fires (RFD 2). `sample` inside it reads
/// the value the earlier send committed, its nodes join the graph the
/// earlier build made, and nothing the earlier build made fires in it.
#[test]
fn a_later_build_samples_the_values_from_before_it() {
    let mut graph = Runtime::local();
    let (numbers_in, latest, numbers) = graph.build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let numbers = numbers.share(b);
        (numbers_in, numbers.hold(b, 0u32), numbers)
    });
    let fired = std::rc::Rc::new(std::cell::Cell::new(0));
    let count = fired.clone();
    graph
        .listen(numbers, move |_| count.set(count.get() + 1))
        .keep();
    graph.send(numbers_in, 5);
    assert_eq!(fired.get(), 1);

    let (seen, doubled) = graph.build(|b| {
        let seen = *latest.sample(b);
        (seen, numbers.map(|n| n * 2).hold(b, seen * 2))
    });
    assert_eq!(seen, 5, "the value from before the build");
    assert_eq!(fired.get(), 1, "nothing the first build made fired");
    assert_eq!(*graph.sample(doubled), 10);
    graph.send(numbers_in, 6);
    assert_eq!(
        *graph.sample(doubled),
        12,
        "the new hold follows the old input"
    );
}

/// A later build runs the child transactions it starts before it returns,
/// as the first build does: a defer of a `steps_with_current` built there
/// fires in a child of the build's transaction.
#[test]
fn a_later_build_runs_its_child_transactions_before_it_returns() {
    let mut graph = Runtime::local();
    let (count_in, count) = graph.build(|b| {
        let (count, count_in) = b.input_cell(3u32);
        (count_in, count)
    });
    let later = graph.build(|b| count.steps_with_current(b).defer(b).hold(b, 0u32));
    assert_eq!(*graph.sample(later), 3, "the child transaction ran");
    graph.send(count_in, 4);
    assert_eq!(*graph.sample(later), 4);
}

/// The same-instant cycle check covers a later build: closing a loop with
/// a cell defined from its own forward at the same instant is refused, and
/// the panic escapes the build's transaction, so the runtime is poisoned.
#[test]
fn a_later_build_refuses_a_same_instant_cycle() {
    let mut graph = Runtime::local();
    let numbers_in = graph.build(|b| b.input::<u32>().1);
    let result = catch_unwind(AssertUnwindSafe(|| {
        graph.build(|b| {
            let (count, count_loop) = b.cell_loop::<u32>();
            let next = count.map_cell(b, |n| n + 1);
            count_loop.close(b, next);
        })
    }));
    assert!(panic_text(result).contains("cycle"));
    assert_eq!(graph.try_send(numbers_in, 1), Err(SendError::Poisoned));
}

/// A poisoned runtime refuses a build as it refuses its other operations.
#[test]
fn a_poisoned_runtime_refuses_a_build() {
    let mut graph = Runtime::local();
    let a_in = graph.build(|b| b.input::<u32>().1);
    let _ = catch_unwind(AssertUnwindSafe(|| {
        graph.transaction(|tx| {
            tx.send(a_in, 1);
            tx.send(a_in, 2);
        })
    }));
    let result = catch_unwind(AssertUnwindSafe(|| {
        graph.build(|b| b.constant(1u32));
    }));
    assert!(panic_text(result).contains("poisoned"));
}
