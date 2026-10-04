//! Steps 4 and 5: `tick`, and FizzBuzz, first with sentinels and then with
//! strings. The timer keeps ticking while `out` is redefined under it, and
//! through both demos, and nothing restarts.

mod common;

use bough_repl::Repl;
use common::{TICK, Ticks, listing, ticking, with_manual_clock};

/// The brief's sentinel demo, with ticks between its commands. Each
/// redefinition of `out` shows at once, since the switch steps when it
/// moves, and every tick after it prints exactly one line for `out`. Since
/// step 5, `watch` prints what it is handed, so the sentinels print raw;
/// in step 4 they printed as Fizz, Buzz and FizzBuzz.
fn sentinel_demo(seed: Option<u64>) -> (Repl, Ticks) {
    let (mut repl, ticks) = with_manual_clock();
    repl.set_shuffle_seed(seed);
    ticking(
        &mut repl,
        &ticks,
        &[
            ("tick t 1000", &[]),
            ("def out t", &[]),
            ("watch out", &["out = 0"]),
            (TICK, &["out = 1"]),
            (TICK, &["out = 2"]),
            ("def m3 mod t 3", &[]),
            ("def fizz eq m3 0", &[]),
            (TICK, &["out = 3"]),
            ("def out if fizz -1 t", &["out = -1"]),
            (TICK, &["out = 4"]),
            (TICK, &["out = 5"]),
            ("def m5 mod t 5", &[]),
            ("def buzz eq m5 0", &[]),
            ("def fb and fizz buzz", &[]),
            ("def o1 if buzz -2 t", &[]),
            ("def o2 if fizz -1 o1", &[]),
            (TICK, &["out = -1"]),
            (TICK, &["out = 7"]),
            (TICK, &["out = 8"]),
            (TICK, &["out = -1"]),
            // `out` doesn't read the new bindings yet.
            (TICK, &["out = 10"]),
            ("def out if fb -3 o2", &["out = -2"]),
            (TICK, &["out = 11"]),
            (TICK, &["out = -1"]),
            (TICK, &["out = 13"]),
            (TICK, &["out = 14"]),
            (TICK, &["out = -3"]),
            (TICK, &["out = 16"]),
        ],
    );
    assert_eq!(
        listing(&mut repl),
        [
            "t : Int = tick 1000",
            "out : Int = if fb -3 o2",
            "m3 : Int = mod t 3",
            "fizz : Bool = eq m3 0",
            "m5 : Int = mod t 5",
            "buzz : Bool = eq m5 0",
            "fb : Bool = and fizz buzz",
            "o1 : Int = if buzz -2 t",
            "o2 : Int = if fizz -1 o1",
        ]
    );
    (repl, ticks)
}

#[test]
fn the_sentinel_demo_redefines_out_under_a_running_timer() {
    let (_, ticks) = sentinel_demo(None);
    assert_eq!(ticks.timers(), 1, "the timer was never restarted");
}

/// The brief's string demo, on from the sentinel demo under the same
/// timer. `out` can't become a `Str`, so the labels are a binding of their
/// own, watched beside it.
#[test]
fn the_string_demo_labels_beside_the_raw_sentinels() {
    let (mut repl, ticks) = sentinel_demo(None);
    ticking(
        &mut repl,
        &ticks,
        &[
            ("def out str t", &["error: out is Int, and str t is Str"]),
            ("def n str t", &[]),
            ("def s1 if buzz \"Buzz\" n", &[]),
            ("def s2 if fizz \"Fizz\" s1", &[]),
            ("def label if fb \"FizzBuzz\" s2", &[]),
            ("watch label", &["label = 16"]),
            (TICK, &["out = 17", "label = 17"]),
            (TICK, &["out = -1", "label = Fizz"]),
            (TICK, &["out = 19", "label = 19"]),
            (TICK, &["out = -2", "label = Buzz"]),
            (TICK, &["out = -1", "label = Fizz"]),
            (TICK, &["out = 22", "label = 22"]),
            (TICK, &["out = 23", "label = 23"]),
            (TICK, &["out = -1", "label = Fizz"]),
            (TICK, &["out = -2", "label = Buzz"]),
            (TICK, &["out = 26", "label = 26"]),
            (TICK, &["out = -1", "label = Fizz"]),
            (TICK, &["out = 28", "label = 28"]),
            (TICK, &["out = 29", "label = 29"]),
            (TICK, &["out = -3", "label = FizzBuzz"]),
        ],
    );
    assert_eq!(
        listing(&mut repl)[9..],
        [
            "n : Str = str t",
            "s1 : Str = if buzz \"Buzz\" n",
            "s2 : Str = if fizz \"Fizz\" s1",
            "label : Str = if fb \"FizzBuzz\" s2",
        ]
    );
    assert_eq!(ticks.timers(), 1, "the timer was never restarted");
}

#[test]
fn the_sentinel_demo_prints_the_same_in_every_evaluation_order() {
    for seed in 1..=16 {
        sentinel_demo(Some(seed));
    }
}

#[test]
fn a_tick_is_an_input_only_its_timer_sets() {
    let (mut repl, ticks) = with_manual_clock();
    ticking(
        &mut repl,
        &ticks,
        &[
            ("tick t 10", &[]),
            ("tick u 0", &["error: 0 is not a period in milliseconds"]),
            (
                "tick u soon",
                &["error: soon is not a period in milliseconds"],
            ),
            ("tick t 10", &["error: t is already bound"]),
            ("set t 5", &["error: t is a tick, which its timer sets"]),
            (
                "def t 5",
                &["error: t is an input, and an input is not redefined"],
            ),
            ("tick u 20", &[]),
            ("def sum add t u", &[]),
            ("watch sum", &["sum = 0"]),
            // Each timer's send is a transaction of its own.
            (TICK, &["sum = 1", "sum = 2"]),
            (TICK, &["sum = 3", "sum = 4"]),
        ],
    );
    assert_eq!(ticks.timers(), 2);
}
