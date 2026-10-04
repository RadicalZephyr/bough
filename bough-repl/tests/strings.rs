//! Step 5: the third type. String literals, `Str` inputs, the `Int -> Str`
//! conversion, and `if`'s second signature.

mod common;

use bough_repl::Repl;
use common::{listing, transcript};

#[test]
fn a_str_input_is_set_and_watched_like_any_other() {
    let mut repl = Repl::new();
    transcript(
        &mut repl,
        &[
            ("input s \"hi\"", &[]),
            ("watch s", &["s = hi"]),
            ("set s \"yo\"", &["s = yo"]),
            ("set s 3", &["error: s is Str, and 3 is Int"]),
            ("set s \"\"", &["s = "]),
            ("def k \"lit\"", &[]),
            ("watch k", &["k = lit"]),
        ],
    );
    assert_eq!(
        listing(&mut repl),
        ["s : Str = input \"hi\"", "k : Str = \"lit\""]
    );
}

#[test]
fn if_takes_the_signature_its_arguments_name() {
    let mut repl = Repl::new();
    transcript(
        &mut repl,
        &[
            ("input p true", &[]),
            ("input a 5", &[]),
            ("def i if p a 0", &[]),
            ("def s if p \"yes\" \"no\"", &[]),
            ("watch i", &["i = 5"]),
            ("watch s", &["s = yes"]),
            ("set p false", &["i = 0", "s = no"]),
            (
                "def x if p a \"no\"",
                &["error: if takes (Bool, Int, Int) or (Bool, Str, Str), \
                     and p a \"no\" is (Bool, Int, Str)"],
            ),
            ("def x if p a", &["error: if takes 3 arguments, not 2"]),
            (
                "def x str p",
                &["error: str takes Int as argument 1, and p is Bool"],
            ),
            (
                "def x add \"a\" 1",
                &["error: add takes Int as argument 1, and \"a\" is Str"],
            ),
        ],
    );
}

#[test]
fn a_str_binding_is_rebound_like_any_other() {
    let mut repl = Repl::new();
    transcript(
        &mut repl,
        &[
            ("input a 5", &[]),
            ("input p true", &[]),
            ("def s if p \"yes\" \"no\"", &[]),
            ("watch s", &["s = yes"]),
            ("def s str a", &["s = 5"]),
            ("set a 6", &["s = 6"]),
            ("def s a", &["error: s is Str, and a is Int"]),
            ("def t if p s s", &[]),
            (
                "def s if p t \"no\"",
                &["error: def s if p t \"no\" would make a cycle: s -> t -> s"],
            ),
        ],
    );
}
