//! The alternative to refusing a data event that the rollback probe writes
//! up beside the others: errors as values, as a spreadsheet's `#ERR`. A
//! function that can fail returns a `Result`, and its cell holds that, so
//! a failing event is neither refused nor dropped: it steps the cell to an
//! error, which flows downstream like any value, while every other cell
//! the event reaches steps as usual, and the next good event steps it
//! back. Bough needs nothing new for it; the cost is in the types, and in
//! every function downstream carrying the error through.

use std::cell::RefCell;
use std::rc::Rc;

use bough::{Runtime, SendError};

/// `boom`, as a function that says it failed rather than panicking.
fn boom(n: i64) -> Result<i64, String> {
    if n != 0 && n % 7 == 0 {
        Err(format!("boom on {n}"))
    } else {
        Ok(n)
    }
}

#[test]
fn a_failing_value_becomes_an_error_that_flows_and_clears() {
    let (mut runtime, edge) = Runtime::build(|b| {
        let (t, t_in) = b.input_cell(0i64);
        let boomed = t.map_cell(b, |n| boom(*n));
        // Downstream carries the error through.
        let doubled = boomed.map_cell(b, |r: &Result<i64, String>| r.clone().map(|n| n * 2));
        let sibling = t.map_cell(b, |n| n + 1);
        (t_in, doubled, sibling)
    });
    let (t_in, doubled, sibling) = edge.keep();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let log = seen.clone();
    runtime
        .listen_steps(doubled, move |r| {
            log.borrow_mut().push(match r {
                Ok(n) => format!("doubled = {n}"),
                Err(e) => format!("doubled = error: {e}"),
            })
        })
        .keep();
    let log = seen.clone();
    runtime
        .listen_steps(sibling, move |n| {
            log.borrow_mut().push(format!("sibling = {n}"))
        })
        .keep();
    for n in 6..=8 {
        runtime.send(t_in, n);
    }
    let mut seen = seen.borrow().clone();
    seen.sort();
    assert_eq!(
        seen,
        [
            "doubled = 12",
            "doubled = 16",
            "doubled = error: boom on 7",
            "sibling = 7",
            "sibling = 8",
            "sibling = 9",
        ]
    );
    assert_eq!(runtime.try_send(t_in, 9), Ok::<(), SendError>(()));
}
