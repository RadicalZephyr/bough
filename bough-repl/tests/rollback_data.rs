//! Rolling back a data event, F4: a tick that an installed definition,
//! `boom`, fails on. The baseline stance, under `--features undo` with
//! rollback on: the transaction is rolled back, its event is dropped, and
//! the refusal is reported once, naming the node that failed and the
//! input whose event was dropped. The program survives it, and the next
//! tick runs as if the failing one had never been sent.

#![cfg(feature = "undo")]

mod common;

use bough_repl::Repl;
use common::{TICK, Ticks, ticking, with_manual_clock};

/// A REPL with rollback on, ticking `t`, with `b = boom t` and a sibling
/// `c = add t 1`, all three watched, through six good ticks.
fn ticking_boom(seed: u64) -> (Repl, Ticks) {
    let (mut repl, ticks) = with_manual_clock();
    repl.set_rollback(true);
    repl.set_shuffle_seed(Some(seed));
    ticking(
        &mut repl,
        &ticks,
        &[
            ("tick t 1000", &[]),
            ("def c add t 1", &[]),
            ("def b boom t", &[]),
            ("watch t", &["t = 0"]),
            ("watch c", &["c = 1"]),
            ("watch b", &["b = 0"]),
            (TICK, &["t = 1", "c = 2", "b = 1"]),
            (TICK, &["t = 2", "c = 3", "b = 2"]),
            (TICK, &["t = 3", "c = 4", "b = 3"]),
            (TICK, &["t = 4", "c = 5", "b = 4"]),
            (TICK, &["t = 5", "c = 6", "b = 5"]),
            (TICK, &["t = 6", "c = 7", "b = 6"]),
        ],
    );
    (repl, ticks)
}

/// The seventh tick fails. Under every seed, nothing prints for it but
/// the one refusal, with no watcher's line, `c`'s included, since a
/// transaction is all or nothing; and the eighth tick runs normally, from
/// `t` at 6.
#[test]
fn f4_a_failing_tick_is_dropped_reported_once_and_the_next_tick_runs() {
    for seed in 0..32 {
        let (mut repl, ticks) = ticking_boom(seed);
        ticks.fire();
        let printed = repl.pump();
        assert_eq!(printed.len(), 1, "seed {seed}: {printed:?}");
        let refusal = &printed[0];
        assert!(
            refusal.starts_with("error: refused a transaction: node ")
                && refusal.contains("failed: boom on 7")
                && refusal.contains("dropped its events at node "),
            "seed {seed}: {refusal}"
        );
        ticking(&mut repl, &ticks, &[(TICK, &["t = 8", "c = 9", "b = 8"])]);
    }
}

/// Each failing tick is reported once, and only once: the timer goes on
/// sending, and nothing loops. Ticks 7 and 14 fail and the rest print.
#[test]
fn f4_every_failing_tick_is_reported_once_and_nothing_loops() {
    let (mut repl, ticks) = ticking_boom(0);
    let mut refusals = 0;
    for tick in 7..=15 {
        ticks.fire();
        let printed = repl.pump();
        if tick % 7 == 0 {
            assert_eq!(printed.len(), 1, "tick {tick}: {printed:?}");
            assert!(
                printed[0].contains(&format!("boom on {tick}")),
                "{printed:?}"
            );
            refusals += 1;
        } else {
            let mut printed = printed;
            printed.sort();
            let expected = [
                format!("b = {tick}"),
                format!("c = {}", tick + 1),
                format!("t = {tick}"),
            ];
            assert_eq!(printed, expected, "tick {tick}");
        }
    }
    assert_eq!(refusals, 2);
}

/// The fix is an edit: redefine `b`, and the next multiple of 7 runs.
#[test]
fn f4_after_a_failing_tick_redefining_the_binding_lets_the_next_one_through() {
    let (mut repl, ticks) = ticking_boom(0);
    ticks.fire();
    assert_eq!(repl.pump().len(), 1, "tick 7 refused");
    ticking(
        &mut repl,
        &ticks,
        &[
            ("def b add t 0", &["b = 6"]),
            (TICK, &["t = 8", "c = 9", "b = 8"]),
        ],
    );
}
