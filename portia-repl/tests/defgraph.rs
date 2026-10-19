//! `defgraph`: a name for a body of graph building, which each call builds
//! afresh with its arguments for the parameters. The body names only its
//! parameters; its types are checked at each call.

mod common;

use portia_repl::Repl;
use common::{TICK, listing, ticking, transcript, with_manual_clock};

const WORD_FOR_MOD_N: &str = "(defgraph word-for-mod-n [x n word] (if (eq (mod x n) 0) word \"\"))";

fn repl(lines: &[&str]) -> Repl {
    let mut repl = Repl::new();
    for line in lines {
        assert_eq!(repl.run(line), Vec::<String>::new(), "`{line}` printed");
    }
    repl
}

/// The demo with Fizz, Buzz and Splash as three calls of one defgraph.
#[test]
fn fizz_buzz_and_splash_are_one_defgraph_called_three_times() {
    let (mut repl, ticks) = with_manual_clock();
    ticking(
        &mut repl,
        &ticks,
        &[
            ("(input in 1)", &[]),
            ("(tick t 1000)", &[]),
            ("(def x in)", &[]),
            (WORD_FOR_MOD_N, &[]),
            ("(def fizz (word-for-mod-n x 3 \"Fizz\"))", &[]),
            ("(def buzz (word-for-mod-n x 5 \"Buzz\"))", &[]),
            ("(def words (cat fizz buzz))", &[]),
            ("(def out (if (eq words \"\") (str x) words))", &[]),
            ("(watch out)", &["out = 1"]),
            ("(set in 3)", &["out = Fizz"]),
            ("(set in 10)", &["out = Buzz"]),
            // A binding built by a call follows its arguments' bindings.
            ("(def x t)", &["out = FizzBuzz"]),
            (TICK, &["out = 1"]),
            (TICK, &["out = 2"]),
            (TICK, &["out = Fizz"]),
            (TICK, &["out = 4"]),
            (TICK, &["out = Buzz"]),
            (TICK, &["out = Fizz"]),
            (TICK, &["out = 7"]),
            ("(def splash (word-for-mod-n x 7 \"Splash\"))", &[]),
            (
                "(def words (cat (cat fizz buzz) splash))",
                &["out = Splash"],
            ),
            (TICK, &["out = 8"]),
            (TICK, &["out = Fizz"]),
            (TICK, &["out = Buzz"]),
            (TICK, &["out = 11"]),
            (TICK, &["out = Fizz"]),
            (TICK, &["out = 13"]),
            (TICK, &["out = Splash"]),
        ],
    );
    assert_eq!(ticks.timers(), 1, "the timer was never restarted");
}

#[test]
fn a_call_builds_what_the_body_written_out_builds() {
    let mut repl = repl(&["(input x 1)", WORD_FOR_MOD_N]);
    let start = repl.live_nodes();
    repl.run("(def fizz (word-for-mod-n x 3 \"Fizz\"))");
    let called = repl.live_nodes() - start;
    let start = repl.live_nodes();
    repl.run("(def buzz (if (eq (mod x 3) 0) \"Fizz\" \"\"))");
    assert_eq!(repl.live_nodes() - start, called);
    // Four constants and three lifts, and the four nodes of the binding.
    assert_eq!(called, 4 + 3 + 4);
}

#[test]
fn an_argument_named_twice_in_the_body_is_built_once() {
    let mut repl = repl(&["(input x 1)", "(defgraph twice [a] (add a a))"]);
    let start = repl.live_nodes();
    // A constant and a lift for the argument, a lift for the body.
    repl.run("(def y (twice (mul x 2)))");
    assert_eq!(repl.live_nodes(), start + 1 + 1 + 1 + 4);
    // Written out, the argument is built twice.
    let start = repl.live_nodes();
    repl.run("(def z (add (mul x 2) (mul x 2)))");
    assert_eq!(repl.live_nodes(), start + 2 + 2 + 1 + 4);
    transcript(
        &mut repl,
        &[("(watch y)", &["y = 4"]), ("(set x 3)", &["y = 12"])],
    );
}

#[test]
fn a_defgraph_calls_one_defined_before_it_and_composes_in_nesting() {
    let mut repl = repl(&[
        "(input x 15)",
        WORD_FOR_MOD_N,
        "(defgraph fizzbuzz [x] (cat (word-for-mod-n x 3 \"Fizz\") (word-for-mod-n x 5 \"Buzz\")))",
        "(defgraph label [x] (if (eq (fizzbuzz x) \"\") (str x) (fizzbuzz x)))",
        "(def out (label (add x 0)))",
    ]);
    transcript(
        &mut repl,
        &[
            ("(watch out)", &["out = FizzBuzz"]),
            ("(set x 7)", &["out = 7"]),
            ("(set x 9)", &["out = Fizz"]),
        ],
    );
}

#[test]
fn a_body_s_types_are_checked_at_each_call() {
    let mut repl = repl(&[
        "(input p true)",
        "(input a 5)",
        "(defgraph pick [c x y] (if c x y))",
        "(def i (pick p a 0))",
        "(def s (pick p \"yes\" \"no\"))",
    ]);
    transcript(
        &mut repl,
        &[
            ("(watch i)", &["i = 5"]),
            ("(watch s)", &["s = yes"]),
            ("(set p false)", &["i = 0", "s = no"]),
            (
                "(def x (pick p a \"no\"))",
                &[
                    "error: in (pick p a \"no\"): if takes (Bool, Int, Int) or (Bool, Str, Str), \
                     and c x y is (Bool, Int, Str)",
                ],
            ),
        ],
    );
}

#[test]
fn a_defgraph_is_checked_for_what_it_names_and_builds_nothing() {
    let mut repl = repl(&["(input x 1)", "(def k 2)"]);
    let nodes = repl.live_nodes();
    transcript(
        &mut repl,
        &[
            (
                "(defgraph w [in n word] (if (eq (mod x n) 0) word \"\"))",
                &["error: the body of w names x, which is not one of its parameters"],
            ),
            (
                "(defgraph w [a] (frob a))",
                &["error: no function named frob"],
            ),
            (
                "(defgraph w [a] (add a))",
                &["error: add takes 2 arguments, not 1"],
            ),
            ("(defgraph w [a] (w a))", &["error: no function named w"]),
            ("(defgraph w [a] [a])", &["error: [a] is not an expression"]),
            (
                "(defgraph w [a] ((add a 1)))",
                &["error: ((add a 1)) does not start with a function's name"],
            ),
            ("(defgraph w [a a] a)", &["error: [a a] names a twice"]),
            ("(defgraph w [1] 1)", &["error: 1 cannot be a name"]),
            (
                "(defgraph w (a) a)",
                &["error: (a) is not a vector of parameters"],
            ),
            (
                "(defgraph w a)",
                &["error: defgraph takes 3 arguments, not 2: (defgraph name [param ...] expr)"],
            ),
            (
                "(defgraph add [a] a)",
                &["error: add is a function in the registry"],
            ),
            ("(defgraph k [a] a)", &["error: k is already bound"]),
            ("(defgraph twice [a] (add a a))", &[]),
            (
                "(defgraph twice [a] (mul a 2))",
                &[
                    "error: twice is a defgraph, and a defgraph cannot be redefined: \
                     the bindings built from it would not follow",
                ],
            ),
            ("(def twice 1)", &["error: twice is a defgraph"]),
            ("(input twice 1)", &["error: twice is a defgraph"]),
            // A defgraph of no parameters is a constant body.
            ("(defgraph seven [] 7)", &[]),
        ],
    );
    assert_eq!(repl.live_nodes(), nodes);
    assert_eq!(
        listing(&mut repl),
        [
            "(defgraph twice [a] (add a a))",
            "(defgraph seven [] 7)",
            "x : Int = (input 1)",
            "k : Int = 2",
        ]
    );
}

#[test]
fn a_rejected_call_names_the_call_and_touches_nothing() {
    let mut repl = repl(&[
        "(input x 1)",
        "(defgraph twice [a] (add a a))",
        "(defgraph quad [b] (twice (twice b)))",
        "(def y (quad x))",
    ]);
    repl.run("(watch y)");
    let before = listing(&mut repl);
    let nodes = repl.live_nodes();
    transcript(
        &mut repl,
        &[
            (
                "(def z (twice))",
                &["error: twice takes 1 arguments, not 0"],
            ),
            ("(def z (twice q))", &["error: no binding named q"]),
            (
                "(def z (twice true))",
                &["error: in (twice true): add takes Int as argument 1, and a is Bool"],
            ),
            (
                "(def z (quad \"s\"))",
                &[
                    "error: in (quad \"s\"): in (twice b): add takes Int as argument 1, and a is Str",
                ],
            ),
            (
                "(def y (str (quad x)))",
                &["error: y is Int, and (str (quad x)) is Str"],
            ),
        ],
    );
    assert_eq!(listing(&mut repl), before);
    assert_eq!(repl.live_nodes(), nodes);
    transcript(&mut repl, &[("(set x 2)", &["y = 8"])]);
}

#[test]
fn the_cycle_check_sees_a_binding_passed_to_a_call() {
    let mut repl = repl(&[
        "(input a 1)",
        "(defgraph twice [a] (add a a))",
        "(def x (add a 1))",
        "(def y (twice x))",
    ]);
    transcript(
        &mut repl,
        &[(
            "(def x (twice y))",
            &["error: (def x (twice y)) would make a cycle: x -> y -> x"],
        )],
    );
}
