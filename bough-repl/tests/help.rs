//! `help`: the topics, the lists of commands and functions, one of either
//! in detail, and the names it reserves.

mod common;

use bough_repl::Repl;
use bough_repl::registry;
use common::{listing, transcript, with_manual_clock};

const WORD_FOR_MOD_N: &str = "(defgraph word-for-mod-n [x n word] (if (eq (mod x n) 0) word \"\"))";

const COMMANDS: [&str; 8] = [
    "(input name literal)               declare an input; `literal` gives its type",
    "(tick name ms)                     an `Int` input a timer counts up, one step every `ms`",
    "(set name literal)                 send a value to an input",
    "(def name expr)                    bind `name` to an expression, or rebind it",
    "(defgraph name [param ...] expr)   name a piece of graph, built afresh at each call",
    "(watch name)                       print a binding's value now and at every step",
    "(graph)                            list every defgraph and binding",
    "(help topic)                       this help",
];

const FNS: [&str; 16] = [
    "(add a b)    (Int, Int) -> Int         `a` + `b`, wrapping on overflow",
    "(sub a b)    (Int, Int) -> Int         `a` - `b`, wrapping on overflow",
    "(mul a b)    (Int, Int) -> Int         `a` * `b`, wrapping on overflow",
    "(div a n)    (Int, Int) -> Int         the quotient of `a` / `n`, the one `mod` is the remainder of",
    "(mod a n)    (Int, Int) -> Int         the remainder of `a` / `n`, never negative",
    "(neg a)      (Int) -> Int              the negation of `a`, wrapping on overflow",
    "(eq a b)     (Int, Int) -> Bool        whether `a` and `b` are equal",
    "             (Str, Str) -> Bool",
    "(gt a b)     (Int, Int) -> Bool        whether `a` > `b`",
    "(and p q)    (Bool, Bool) -> Bool      whether `p` and `q` are both true",
    "(or p q)     (Bool, Bool) -> Bool      whether `p` or `q` or both are true",
    "(not p)      (Bool) -> Bool            the opposite of `p`",
    "(if c a b)   (Bool, Int, Int) -> Int   `a` when `c` is true, else `b`",
    "             (Bool, Str, Str) -> Str",
    "(str n)      (Int) -> Str              `n` in decimal",
    "(cat s t)    (Str, Str) -> Str         `s` followed by `t`",
];

#[test]
fn a_bare_help_lists_the_topics() {
    assert_eq!(
        Repl::new().run("(help)"),
        [
            "(help commands)   every command, one line each",
            "(help fns)        every function: the registry's, and your defgraphs",
            "(help all)        both lists",
            "(help name)       one command or function in detail",
        ]
    );
}

#[test]
fn help_commands_lists_every_command_on_a_line() {
    assert_eq!(Repl::new().run("(help commands)"), COMMANDS);
}

#[test]
fn help_fns_lists_every_signature_and_the_defgraphs_apart() {
    let mut repl = Repl::new();
    assert_eq!(repl.run("(help fns)"), FNS);
    repl.run(WORD_FOR_MOD_N);
    repl.run("(defgraph seven [] 7)");
    let mut fns: Vec<&str> = FNS.to_vec();
    fns.extend([
        "",
        "(word-for-mod-n x n word)   defgraph",
        "(seven)                     defgraph",
    ]);
    assert_eq!(repl.run("(help fns)"), fns);
}

#[test]
fn help_all_is_the_commands_and_then_the_fns() {
    let mut all: Vec<&str> = COMMANDS.to_vec();
    all.push("");
    all.extend(FNS);
    assert_eq!(Repl::new().run("(help all)"), all);
}

#[test]
fn help_shows_a_command_in_detail() {
    assert_eq!(
        Repl::new().run("(help def)"),
        [
            "(def name expr)",
            "",
            "Binds `name` to `expr`. `expr` can be a literal, a binding's name,",
            "or a call `(f arg ...)`, whose arguments are expressions too. Each",
            "call builds a cell, and only `name` is a binding.",
            "",
            "Redefining a name swaps its definition in place, under anything",
            "running, and everything downstream follows. The type must stay the",
            "same, and the new definition must not make a cycle.",
            "",
            "    (def y (add x 1))",
            "    (def label (if (eq (mod x 3) 0) \"Fizz\" (str x)))",
        ]
    );
}

#[test]
fn help_shows_a_function_with_each_of_its_signatures() {
    let mut repl = Repl::new();
    assert_eq!(
        repl.run("(help mod)"),
        [
            "(mod a n)",
            "    (Int, Int) -> Int",
            "",
            "The remainder of `a` divided by `n`, never negative, so `(mod -7 3)`",
            "is 2. `(mod a 0)` is 0, since a function in the graph must never",
            "panic.",
        ]
    );
    assert_eq!(
        repl.run("(help eq)"),
        [
            "(eq a b)",
            "    (Int, Int) -> Bool",
            "    (Str, Str) -> Bool",
            "",
            "Whether `a` and `b` are equal, as integers or as strings.",
        ]
    );
}

#[test]
fn help_shows_a_defgraph_as_written() {
    let mut repl = Repl::new();
    repl.run(WORD_FOR_MOD_N);
    assert_eq!(
        repl.run("(help word-for-mod-n)"),
        [
            "(word-for-mod-n x n word)",
            "",
            &format!("    {WORD_FOR_MOD_N}"),
        ]
    );
}

#[test]
fn every_command_and_function_has_help_that_starts_with_its_usage() {
    let mut repl = Repl::new();
    let commands = repl.run("(help commands)");
    let fns = repl.run("(help fns)");
    let usages = commands
        .iter()
        .chain(fns.iter().filter(|line| line.starts_with('(')))
        .map(|line| line.split("   ").next().unwrap().to_string())
        .collect::<Vec<_>>();
    // Eight commands, and fourteen names in the registry.
    assert_eq!(usages.len(), 8 + 14);
    for usage in usages {
        let name = usage[1..].split([' ', ')']).next().unwrap();
        let detail = repl.run(&format!("(help {name})"));
        assert_eq!(detail[0], usage, "(help {name})");
        assert!(detail.len() > 2, "(help {name}) says little: {detail:?}");
    }
}

#[test]
fn the_signatures_of_one_name_share_their_help() {
    for f in registry::all() {
        assert!(
            !f.doc.summary.is_empty() && !f.doc.detail.is_empty(),
            "{}",
            f.name
        );
        let first = registry::named(f.name).next().unwrap();
        assert_eq!(f.doc.usage, first.doc.usage, "{}", f.name);
        assert_eq!(f.doc.summary, first.doc.summary, "{}", f.name);
        assert_eq!(f.doc.detail, first.doc.detail, "{}", f.name);
    }
}

#[test]
fn the_commands_and_the_topics_are_reserved_and_parameters_may_use_them() {
    let (mut repl, ticks) = with_manual_clock();
    let commands = [
        "input", "tick", "set", "def", "defgraph", "watch", "graph", "help",
    ];
    for (words, what) in [
        (&commands[..], "a command"),
        (&["commands", "fns", "all"][..], "a help topic"),
    ] {
        for word in words {
            let refused = format!("error: {word} is {what}, and cannot be a name");
            for line in [
                format!("(def {word} 1)"),
                format!("(input {word} 1)"),
                format!("(tick {word} 10)"),
                format!("(defgraph {word} [a] a)"),
            ] {
                assert_eq!(repl.run(&line), [refused.as_str()], "{line}");
            }
        }
    }
    assert_eq!(ticks.timers(), 0);
    transcript(
        &mut repl,
        &[
            ("(defgraph either [all help] (or all help))", &[]),
            ("(def p (either true false))", &[]),
            ("(watch p)", &["p = true"]),
        ],
    );
}

#[test]
fn help_is_refused_for_what_it_does_not_know() {
    let mut repl = Repl::new();
    repl.run("(input x 1)");
    transcript(
        &mut repl,
        &[
            (
                "(help frob)",
                &["error: no command or function named frob; `(help)` lists the topics"],
            ),
            (
                "(help x)",
                &["error: no command or function named x; `(help)` lists the topics"],
            ),
            (
                "(help 3)",
                &["error: 3 is not a topic; `(help)` lists them"],
            ),
            (
                "(help (def))",
                &["error: (def) is not a topic; `(help)` lists them"],
            ),
            (
                "(help def mod)",
                &["error: help takes 0 or 1 arguments, not 2: (help topic)"],
            ),
        ],
    );
}

#[test]
fn help_touches_nothing() {
    let mut repl = Repl::new();
    for line in [
        "(input x 1)",
        WORD_FOR_MOD_N,
        "(def fizz (word-for-mod-n x 3 \"Fizz\"))",
    ] {
        repl.run(line);
    }
    let before = listing(&mut repl);
    let nodes = repl.live_nodes();
    for line in [
        "(help)",
        "(help all)",
        "(help def)",
        "(help word-for-mod-n)",
    ] {
        repl.run(line);
    }
    assert_eq!(listing(&mut repl), before);
    assert_eq!(repl.live_nodes(), nodes);
}
