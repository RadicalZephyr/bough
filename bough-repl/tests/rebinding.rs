//! Step 3: rebinding. `def` on an existing name swaps the binding's
//! definition through its switch, and dependents follow without a new
//! `watch`; check 3 refuses a cycle and check 4 a change of type.

mod common;

use bough_repl::Repl;
use common::{listing, transcript};

fn repl(lines: &[&str]) -> Repl {
    let mut repl = Repl::new();
    for line in lines {
        assert_eq!(repl.run(line), Vec::<String>::new(), "`{line}` printed");
    }
    repl
}

#[test]
fn a_redefinition_reaches_watched_dependents_without_a_new_watch() {
    let mut repl = repl(&["input a 1", "def b add a a", "def c mul a b"]);
    transcript(
        &mut repl,
        &[
            ("watch c", &["c = 2"]),
            // The switch steps at the redefinition, so `c` does too.
            ("def b mul a a", &["c = 1"]),
            ("set a 3", &["c = 27"]),
            ("def b 10", &["c = 30"]),
            ("set a 4", &["c = 40"]),
        ],
    );
    assert_eq!(
        listing(&mut repl),
        ["a : Int = input 1", "b : Int = 10", "c : Int = mul a b"]
    );
}

#[test]
fn a_watched_binding_redefined_prints_its_new_value() {
    let mut repl = repl(&["input a 2", "def b add a 1", "def d b"]);
    transcript(
        &mut repl,
        &[
            ("watch b", &["b = 3"]),
            ("watch d", &["d = 3"]),
            ("def b neg a", &["b = -2", "d = -2"]),
            ("set a 5", &["b = -5", "d = -5"]),
        ],
    );
}

#[test]
fn a_redefinition_to_an_equal_value_is_still_a_step() {
    // A switch steps whenever it moves, to an equal value too, and a step
    // to an equal value is a step (RFD 2): the watch prints it again.
    let mut repl = repl(&["input a 2", "def b add a a", "def c mul a b"]);
    transcript(
        &mut repl,
        &[
            ("watch c", &["c = 8"]),
            ("def b mul a a", &["c = 8"]),
            ("def b mul a a", &["c = 8"]),
        ],
    );
}

#[test]
fn a_cycle_creating_redefinition_is_rejected_and_leaves_the_graph_unchanged() {
    let mut repl = repl(&["input a 1", "def x add a 1", "def y add x 1", "def z y"]);
    repl.run("watch z");
    let before = listing(&mut repl);
    let nodes = repl.live_nodes();
    transcript(
        &mut repl,
        &[
            (
                "def x add x 1",
                &["error: def x add x 1 would make a cycle: x -> x"],
            ),
            (
                "def x add z 1",
                &["error: def x add z 1 would make a cycle: x -> z -> y -> x"],
            ),
            (
                "def x z",
                &["error: def x z would make a cycle: x -> z -> y -> x"],
            ),
        ],
    );
    assert_eq!(listing(&mut repl), before);
    assert_eq!(repl.live_nodes(), nodes);
    transcript(&mut repl, &[("set a 5", &["z = 7"])]);
}

#[test]
fn the_cycle_check_reads_the_definitions_bindings_have_now() {
    let mut repl = repl(&["input a 1", "def x add a 1", "def y add x 1"]);
    repl.run("watch y");
    transcript(
        &mut repl,
        &[
            (
                "def x add y 1",
                &["error: def x add y 1 would make a cycle: x -> y -> x"],
            ),
            // Once `y` no longer reads `x`, `x` may read `y`.
            ("def y add a 10", &["y = 11"]),
            ("def x add y 1", &[]),
            ("def y mul a 2", &["y = 2"]),
        ],
    );
}

#[test]
fn a_type_changing_redefinition_is_rejected_and_leaves_the_graph_unchanged() {
    let mut repl = repl(&[
        "input a 1",
        "input p true",
        "def b add a 1",
        "def c mul b 2",
    ]);
    repl.run("watch c");
    let before = listing(&mut repl);
    let nodes = repl.live_nodes();
    transcript(
        &mut repl,
        &[
            ("def b eq a 1", &["error: b is Int, and eq a 1 is Bool"]),
            ("def b p", &["error: b is Int, and p is Bool"]),
            ("def b true", &["error: b is Int, and true is Bool"]),
            // The checks before it still come first.
            (
                "def b and a p",
                &["error: and takes Bool as argument 1, and a is Int"],
            ),
        ],
    );
    assert_eq!(listing(&mut repl), before);
    assert_eq!(repl.live_nodes(), nodes);
    transcript(&mut repl, &[("set a 2", &["c = 6"])]);
}

#[test]
fn an_input_is_not_redefined() {
    let mut repl = repl(&["input a 1"]);
    transcript(
        &mut repl,
        &[(
            "def a 5",
            &["error: a is an input, and an input is not redefined"],
        )],
    );
}

#[test]
fn a_binding_costs_four_nodes_besides_its_definition() {
    let mut repl = repl(&["input a 1"]);
    let start = repl.live_nodes();
    repl.run("def k 5"); // a constant
    assert_eq!(repl.live_nodes(), start + 1 + 4);
    repl.run("def j a"); // an alias builds nothing
    assert_eq!(repl.live_nodes(), start + 5 + 4);
    repl.run("def s add a k"); // a lift
    assert_eq!(repl.live_nodes(), start + 9 + 1 + 4);
    repl.run("def t add a 1"); // a constant and a lift
    assert_eq!(repl.live_nodes(), start + 14 + 2 + 4);
}

#[test]
fn a_definition_left_behind_is_collected() {
    let mut repl = repl(&["input a 1", "def b add a 1", "def c mul b 2"]);
    repl.run("watch c");
    let nodes = repl.live_nodes();
    for i in 0..100 {
        let line = if i % 2 == 0 {
            "def b mul a 3"
        } else {
            "def b add a 1"
        };
        assert_eq!(repl.run(line).len(), 1);
    }
    assert_eq!(repl.live_nodes(), nodes);
    // An alias left behind frees nothing: what it named is another
    // binding's, and the namespace keeps that alive.
    repl.run("def b a");
    assert_eq!(repl.live_nodes(), nodes - 2);
    repl.run("def b add a 1");
    assert_eq!(repl.live_nodes(), nodes);
}
