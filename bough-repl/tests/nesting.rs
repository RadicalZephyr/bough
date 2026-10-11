//! Nested expressions: an argument can be an application of its own, which
//! builds a plain cell under the binding, with no name and no switch.

mod common;

use bough_repl::Repl;
use common::{listing, transcript, with_manual_clock};

/// A ticking REPL whose `splash` is defined by `lines`, and what it prints
/// over 15 ticks.
fn splash_transcript(lines: &[&str], seed: Option<u64>) -> Vec<Vec<String>> {
    let (mut repl, ticks) = with_manual_clock();
    repl.set_shuffle_seed(seed);
    repl.run("(tick t 1000)");
    for line in lines {
        assert_eq!(repl.run(line), Vec::<String>::new(), "`{line}` printed");
    }
    let mut printed = vec![repl.run("(watch splash)")];
    for _ in 0..15 {
        ticks.fire();
        printed.push(repl.pump());
    }
    printed
}

#[test]
fn a_nested_definition_prints_what_the_hand_wired_one_does() {
    let nested = ["(def splash (if (eq (mod t 7) 0) \"Splash\" \"\"))"];
    let wired = [
        "(def m7 (mod t 7))",
        "(def p7 (eq m7 0))",
        "(def splash (if p7 \"Splash\" \"\"))",
    ];
    for seed in [None, Some(1), Some(2), Some(3), Some(4)] {
        let printed = splash_transcript(&nested, seed);
        assert_eq!(printed, splash_transcript(&wired, seed));
        assert_eq!(printed[7], ["splash = Splash"]);
        assert_eq!(printed[8], ["splash = "]);
        assert_eq!(printed[14], ["splash = Splash"]);
    }
}

#[test]
fn a_nested_call_costs_its_cell_and_no_binding() {
    let mut repl = Repl::new();
    repl.run("(input a 1)");
    let start = repl.live_nodes();
    // Two constants and two lifts, and the four nodes of `y`'s binding.
    repl.run("(def y (add (mul a 2) 1))");
    assert_eq!(repl.live_nodes(), start + 2 + 2 + 4);
    // By hand, the inner call is a binding of its own: four nodes more.
    let start = repl.live_nodes();
    repl.run("(def m (mul a 2))");
    repl.run("(def z (add m 1))");
    assert_eq!(repl.live_nodes(), start + 2 + 2 + 4 + 4);
}

#[test]
fn a_nested_definition_left_behind_is_collected_whole() {
    let mut repl = Repl::new();
    for line in [
        "(input a 1)",
        "(def b (add (mul a 3) 1))",
        "(def c (mul b 2))",
    ] {
        repl.run(line);
    }
    repl.run("(watch c)");
    let nodes = repl.live_nodes();
    for i in 0..100 {
        let line = if i % 2 == 0 {
            "(def b (sub (neg (neg a)) 1))"
        } else {
            "(def b (add (mul a 3) 1))"
        };
        assert_eq!(repl.run(line).len(), 1);
    }
    assert_eq!(repl.live_nodes(), nodes);
}

#[test]
fn a_check_inside_a_nested_call_names_that_call_and_touches_nothing() {
    let mut repl = Repl::new();
    for line in ["(input a 1)", "(def b (add a 1))", "(watch b)"] {
        repl.run(line);
    }
    let before = listing(&mut repl);
    let nodes = repl.live_nodes();
    transcript(
        &mut repl,
        &[
            (
                "(def y (add (eq a 1) 2))",
                &["error: add takes Int as argument 1, and (eq a 1) is Bool"],
            ),
            (
                "(def y (add (frob a) 1))",
                &["error: no function named frob"],
            ),
            (
                "(def y (add (add a) 1))",
                &["error: add takes 2 arguments, not 1"],
            ),
            ("(def y (add (neg q) 1))", &["error: no binding named q"]),
            (
                "(def y (not (and (gt a 0) (add a 1))))",
                &["error: and takes Bool as argument 2, and (add a 1) is Int"],
            ),
            (
                "(def y (if (gt a 0) (add a 1) \"no\"))",
                &["error: if takes (Bool, Int, Int) or (Bool, Str, Str), \
                     and (gt a 0) (add a 1) \"no\" is (Bool, Int, Str)"],
            ),
            (
                "(def y (add ((add a 1)) 1))",
                &["error: ((add a 1)) does not start with a function's name"],
            ),
            // A redefinition is checked the same way, and keeps its type.
            (
                "(def b (add (eq a 1) 2))",
                &["error: add takes Int as argument 1, and (eq a 1) is Bool"],
            ),
            (
                "(def b (str (add a 1)))",
                &["error: b is Int, and (str (add a 1)) is Str"],
            ),
        ],
    );
    assert_eq!(listing(&mut repl), before);
    assert_eq!(repl.live_nodes(), nodes);
    transcript(&mut repl, &[("(set a 2)", &["b = 3"])]);
}

#[test]
fn the_cycle_check_sees_a_binding_named_at_any_depth() {
    let mut repl = Repl::new();
    for line in ["(input a 1)", "(def x (add a 1))", "(def y (mul x 2))"] {
        repl.run(line);
    }
    let nodes = repl.live_nodes();
    transcript(
        &mut repl,
        &[
            (
                "(def x (add (mul y 2) 1))",
                &["error: (def x (add (mul y 2) 1)) would make a cycle: x -> y -> x"],
            ),
            (
                "(def x (neg (neg (neg y))))",
                &["error: (def x (neg (neg (neg y)))) would make a cycle: x -> y -> x"],
            ),
        ],
    );
    assert_eq!(repl.live_nodes(), nodes);
    // `y` reads `x` through a nested call now, and the check still sees it.
    transcript(
        &mut repl,
        &[
            ("(def y (mul (add x 0) 2))", &[]),
            (
                "(def x y)",
                &["error: (def x y) would make a cycle: x -> y -> x"],
            ),
        ],
    );
}

#[test]
fn graph_prints_a_nested_definition_back() {
    // A manual clock, so the tick starts no timer thread.
    let (mut repl, _ticks) = with_manual_clock();
    transcript(
        &mut repl,
        &[
            ("(tick t 1000)", &[]),
            ("(def splash (if (eq (mod t 7) 0) \"Splash\" \"\"))", &[]),
            ("(def n (add  (mul t 2)\t1))", &[]),
            ("(def times-two-plus-1 (add (mul t 2) 1))", &[]),
        ],
    );
    assert_eq!(
        listing(&mut repl),
        [
            "t : Int = (tick 1000)",
            "splash : Str = (if (eq (mod t 7) 0) \"Splash\" \"\")",
            "n : Int = (add (mul t 2) 1)",
            "times-two-plus-1 : Int = (add (mul t 2) 1)",
        ]
    );
}
