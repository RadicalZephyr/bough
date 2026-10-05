//! The rollback probe's undo log, under `--features undo`, on the parts of
//! the engine `bough-repl` never reaches: a once-listener tied to a
//! refused transaction, and the child instants a `split` queued before the
//! instant failed.

#![cfg(feature = "undo")]

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use bough::{Runtime, Source};

/// A listener a refused transaction tied to itself goes with it, so it
/// can't hear an event of a later one.
#[test]
fn a_listener_tied_to_a_refused_transaction_goes_with_it() {
    let (mut runtime, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let numbers = numbers.share(b);
        let checked = numbers
            .map(|n| {
                assert!(n != 7, "graph code on {n}");
                n
            })
            .hold(b, 0u32);
        (numbers_in, numbers, checked)
    });
    runtime.set_rollback(true);
    let (numbers_in, numbers, _checked) = edge.keep();
    let heard = Rc::new(RefCell::new(Vec::new()));
    let log = heard.clone();
    let refused = catch_unwind(AssertUnwindSafe(|| {
        runtime.transaction(|tx| {
            tx.listen_once(numbers, move |n| log.borrow_mut().push(n));
            tx.send(numbers_in, 7);
        });
    }));
    assert!(refused.is_err(), "a refused transaction panics");
    runtime.send(numbers_in, 1);
    assert!(heard.borrow().is_empty(), "heard {:?}", heard.borrow());
}

/// A split that fired before the instant failed has queued children, which
/// the roll back ends, so the next unit runs only its own: each element
/// once, and none of the refused event's.
#[test]
fn a_split_s_children_queued_before_the_failure_go_with_it() {
    for seed in 0..16 {
        let (mut runtime, edge) = Runtime::build(|b| {
            let (lists, lists_in) = b.input::<Vec<u32>>();
            let lists = lists.share(b);
            let items = lists.split(b).share(b);
            let checked = lists
                .map(|list| {
                    assert!(list.len() != 7, "graph code on {} items", list.len());
                    list.len()
                })
                .hold(b, 0usize);
            (lists_in, items, checked)
        });
        runtime.set_rollback(true);
        runtime.set_shuffle_seed(Some(seed));
        let (lists_in, items, checked) = edge.keep();
        let heard = Rc::new(RefCell::new(Vec::new()));
        let log = heard.clone();
        runtime
            .listen(items, move |n| log.borrow_mut().push(n))
            .keep();
        let refused = runtime.try_send(lists_in, vec![9; 7]);
        assert!(refused.is_err(), "seed {seed}");
        runtime.send(lists_in, vec![1, 2]);
        assert_eq!(*heard.borrow(), [1, 2], "seed {seed}");
        assert_eq!(*runtime.sample(checked), 2);
    }
}
