//! Step 2: a static REPL. `input`, `def` of new names, `set`, `watch` and
//! `graph`, and the first three checks of `def`.

mod common;

use bough_repl::Repl;
use common::{listing, transcript};

#[test]
fn a_diamond_prints_one_correct_update_per_set() {
    let mut repl = Repl::new();
    transcript(
        &mut repl,
        &[
            ("input a 1", &[]),
            ("def b add a a", &[]),
            ("def c mul a b", &[]),
            ("watch c", &["c = 2"]),
            ("set a 2", &["c = 8"]),
            ("set a 3", &["c = 18"]),
            ("set a -1", &["c = 2"]),
        ],
    );
}

#[test]
fn an_ill_typed_def_is_rejected_and_leaves_the_graph_unchanged() {
    let mut repl = Repl::new();
    for line in ["input a 1", "def b add a a", "def c mul a b", "watch c"] {
        repl.run(line);
    }
    let before = listing(&mut repl);
    let nodes = repl.live_nodes();
    transcript(
        &mut repl,
        &[(
            "def d add a true",
            &["error: add takes Int as argument 2, and true is Bool"],
        )],
    );
    assert_eq!(listing(&mut repl), before);
    assert_eq!(repl.live_nodes(), nodes);
    transcript(
        &mut repl,
        &[
            ("watch d", &["error: no binding named d"]),
            ("set a 2", &["c = 8"]),
        ],
    );
}

#[test]
fn each_check_rejects_before_the_graph_is_touched() {
    let mut repl = Repl::new();
    for line in ["input a 1", "input p true"] {
        repl.run(line);
    }
    let nodes = repl.live_nodes();
    transcript(
        &mut repl,
        &[
            // 1: the function exists and the argument count is its arity.
            ("def x frob a", &["error: no function named frob"]),
            ("def x add a", &["error: add takes 2 arguments, not 1"]),
            ("def x not p p", &["error: not takes 1 arguments, not 2"]),
            // 2: each argument's type is the signature's.
            (
                "def x if a a a",
                &["error: if takes Bool as argument 1, and a is Int"],
            ),
            (
                "def x and p 3",
                &["error: and takes Bool as argument 2, and 3 is Int"],
            ),
            // 3: no cycle. A new name is not bound until its definition
            // is, so the only cycle it could make is through itself.
            ("def x add x 1", &["error: no binding named x"]),
            // Names: an argument names a binding, a new name is fresh.
            ("def x add a q", &["error: no binding named q"]),
            ("def a add a 1", &["error: a is already bound"]),
            ("def 3 add a 1", &["error: 3 cannot be a name"]),
            ("def true p", &["error: true cannot be a name"]),
            ("input a 2", &["error: a is already bound"]),
            ("input z nope", &["error: nope is not a literal"]),
        ],
    );
    assert_eq!(repl.live_nodes(), nodes);
    assert_eq!(
        listing(&mut repl),
        ["a : Int = input 1", "p : Bool = input true"]
    );
}

#[test]
fn set_is_checked_against_the_input_s_type() {
    let mut repl = Repl::new();
    transcript(
        &mut repl,
        &[
            ("input a 1", &[]),
            ("def b neg a", &[]),
            ("watch b", &["b = -1"]),
            ("set a true", &["error: a is Int, and true is Bool"]),
            ("set b 4", &["error: b is not an input"]),
            ("set q 4", &["error: no binding named q"]),
            ("set a 4", &["b = -4"]),
        ],
    );
}

#[test]
fn every_function_in_the_registry_computes_what_it_says() {
    let mut repl = Repl::new();
    let mut lines = vec!["input x 7".to_string(), "input y 3".to_string()];
    lines.extend(["input p true", "input q false"].map(String::from));
    let defs = [
        ("add", "add x y", "10"),
        ("sub", "sub x y", "4"),
        ("mul", "mul x y", "21"),
        ("rem", "mod x y", "1"),
        ("negmod", "mod -7 y", "2"),
        ("zeromod", "mod x 0", "0"),
        ("negx", "neg x", "-7"),
        ("same", "eq x y", "false"),
        ("more", "gt x y", "true"),
        ("both", "and p q", "false"),
        ("either", "or p q", "true"),
        ("nay", "not p", "false"),
        ("pick", "if p x y", "7"),
        ("wrap", "add 9223372036854775807 1", "-9223372036854775808"),
    ];
    for line in lines {
        repl.run(&line);
    }
    for (name, def, value) in defs {
        assert_eq!(repl.run(&format!("def {name} {def}")), Vec::<String>::new());
        assert_eq!(
            repl.run(&format!("watch {name}")),
            [format!("{name} = {value}")]
        );
    }
}

#[test]
fn an_alias_follows_its_binding() {
    let mut repl = Repl::new();
    transcript(
        &mut repl,
        &[
            ("input a 1", &[]),
            ("def b a", &[]),
            ("def k 5", &[]),
            ("watch b", &["b = 1"]),
            ("watch k", &["k = 5"]),
            ("watch k", &["error: already watching k"]),
            ("set a 2", &["b = 2"]),
        ],
    );
    assert_eq!(
        listing(&mut repl),
        ["a : Int = input 1", "b : Int = a", "k : Int = 5"]
    );
}

#[test]
fn comments_and_blank_lines_print_nothing_and_nonsense_is_an_error() {
    let mut repl = Repl::new();
    transcript(
        &mut repl,
        &[
            ("", &[]),
            ("   ", &[]),
            ("# a comment", &[]),
            ("frobnicate", &["error: cannot read `frobnicate`"]),
            ("def y", &["error: cannot read `def y`"]),
        ],
    );
}
