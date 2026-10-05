//! The rollback probe under `panic = "abort"`, where nothing can be
//! caught: `undo`, which refuses a transaction by catching its panic, and
//! `stage`, whose refusals are values. Each mode builds a graph with
//! rollback on, sends an event that fails on 7, and then an event that
//! doesn't, printing what happened to each.

use std::process::ExitCode;

use bough::{Runtime, Source};

fn main() -> ExitCode {
    let mode = std::env::args().nth(1).unwrap_or_default();
    match mode.as_str() {
        "undo" => undo(),
        "stage" => stage(),
        _ => {
            eprintln!("usage: abort-probe undo|stage");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

/// A function that panics on 7: under `undo`, a refusal where panics
/// unwind.
fn undo() {
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
    println!("undo: sending 7");
    let sent = runtime.try_send(numbers_in, 7);
    println!("undo: 7 came back {sent:?}");
    runtime.try_send(numbers_in, 1).unwrap();
    println!("undo: then 1, and the cell is {}", runtime.sample(checked));
}

/// A construct closure that returns an error on 7: under `stage`, a
/// refusal with nothing to catch.
fn stage() {
    let (mut runtime, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let made = numbers.try_construct(b, |b, n: u32| {
            if n == 7 {
                return Err(format!("a construct refusing {n}"));
            }
            Ok(b.constant(n))
        });
        let first = b.constant(0u32);
        let latest = made.hold(b, first).switch_cell(b);
        (numbers_in, latest)
    });
    runtime.set_rollback(true);
    let (numbers_in, latest) = edge.keep();
    println!("stage: sending 7");
    let sent = runtime.try_send(numbers_in, 7);
    println!("stage: 7 came back {sent:?}");
    runtime.try_send(numbers_in, 1).unwrap();
    println!("stage: then 1, and the cell is {}", runtime.sample(latest));
}
