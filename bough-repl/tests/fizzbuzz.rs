//! Step 4: `tick`, and FizzBuzz with sentinels. The timer keeps ticking
//! while `out` is redefined under it, and nothing restarts.

mod common;

use common::{TICK, Ticks, listing, ticking, with_manual_clock};

/// The brief's sentinel demo, with ticks between its commands. Each
/// redefinition of `out` shows at once, since the switch steps when it
/// moves, and every tick after it prints exactly one line for `out`.
fn sentinel_demo(seed: Option<u64>) -> Ticks {
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
            ("def out if fizz -1 t", &["out = Fizz"]),
            (TICK, &["out = 4"]),
            (TICK, &["out = 5"]),
            ("def m5 mod t 5", &[]),
            ("def buzz eq m5 0", &[]),
            ("def fb and fizz buzz", &[]),
            ("def o1 if buzz -2 t", &[]),
            ("def o2 if fizz -1 o1", &[]),
            (TICK, &["out = Fizz"]),
            (TICK, &["out = 7"]),
            (TICK, &["out = 8"]),
            (TICK, &["out = Fizz"]),
            // `out` doesn't read the new bindings yet.
            (TICK, &["out = 10"]),
            ("def out if fb -3 o2", &["out = Buzz"]),
            (TICK, &["out = 11"]),
            (TICK, &["out = Fizz"]),
            (TICK, &["out = 13"]),
            (TICK, &["out = 14"]),
            (TICK, &["out = FizzBuzz"]),
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
    ticks
}

#[test]
fn the_sentinel_demo_redefines_out_under_a_running_timer() {
    let ticks = sentinel_demo(None);
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
