//! The rollback probe on `wasm32-unknown-unknown`, where a panic is a
//! trap: each export builds a graph with rollback on, sends an event that
//! fails on 7, then one that doesn't, and returns what happened, for
//! `run.mjs` to print. 0: the failing event was refused and the next one
//! went through; anything else is a code for what went wrong instead.

use bough::{Refusal, Runtime, SendError, Source};

fn outcome(sent: Result<(), SendError>, next: Result<(), SendError>, value: u32) -> i32 {
    match (sent, next, value) {
        (Err(SendError::Refused(Refusal { .. })), Ok(()), 1) => 0,
        (Ok(()), ..) => 1,
        _ => 2,
    }
}

/// `undo`: a function that panics on 7.
#[unsafe(no_mangle)]
pub extern "C" fn undo_probe() -> i32 {
    let (mut runtime, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let checked = numbers
            .map(|n| {
                assert!(n != 7, "graph code on {n}");
                n
            })
            .hold(b, 0u32);
        (numbers_in, checked)
    });
    runtime.set_rollback(true);
    let (numbers_in, checked) = edge.keep();
    let sent = runtime.try_send(numbers_in, 7);
    let next = runtime.try_send(numbers_in, 1);
    outcome(sent, next, *runtime.sample(checked))
}

/// `stage`: a construct closure that returns an error on 7.
#[unsafe(no_mangle)]
pub extern "C" fn stage_probe() -> i32 {
    let (mut runtime, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let made = numbers.try_construct(b, |b, n: u32| {
            if n == 7 {
                return Err("a construct refusing 7");
            }
            Ok(b.constant(n))
        });
        let first = b.constant(0u32);
        let latest = made.hold(b, first).switch_cell(b);
        (numbers_in, latest)
    });
    runtime.set_rollback(true);
    let (numbers_in, latest) = edge.keep();
    let sent = runtime.try_send(numbers_in, 7);
    let next = runtime.try_send(numbers_in, 1);
    outcome(sent, next, *runtime.sample(latest))
}
