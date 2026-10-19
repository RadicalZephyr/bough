//! The function registry: each entry's signature over [`Type`] tags, which
//! the REPL checks, and its wiring function, which runs in a `construct`
//! closure and matches on [`Node`] arms to reach the concrete cells.
//!
//! No polymorphism: a signature names concrete types, and an operation over
//! two types is two entries under one name, which `def` chooses between by
//! the types of its arguments. Every function is total, since a panic in a
//! cell's function poisons the runtime: arithmetic wraps, and `div 0` and
//! `mod 0` are 0.

use bough::{BuildContext, Lift};

use crate::ty::{Node, Type};

/// Builds a function's cell from its argument cells, inside a `construct`.
pub type Wire = fn(&mut BuildContext, &[Node]) -> Node;

/// A registry entry.
pub struct Function {
    /// What `def` calls it.
    pub name: &'static str,
    /// The argument types, in order.
    pub params: &'static [Type],
    /// The result type.
    pub result: Type,
    /// Builds the result from the arguments.
    pub wire: Wire,
    /// What `help` says of it; the signatures of one name share it.
    pub doc: &'static Doc,
}

/// A function's help, written as Markdown: parameter names and code in
/// backticks.
pub struct Doc {
    /// A call with named parameters, such as `(mod a n)`.
    pub usage: &'static str,
    /// One line, for `(help fns)`.
    pub summary: &'static str,
    /// The lines of `(help name)`, after the usage and the signatures.
    pub detail: &'static [&'static str],
}

/// The functions called `name`, one per signature.
pub fn named(name: &str) -> impl Iterator<Item = &'static Function> {
    FUNCTIONS.iter().filter(move |f| f.name == name)
}

/// Every function, in the order `help` lists them; the signatures of one
/// name are next to each other.
pub fn all() -> &'static [Function] {
    FUNCTIONS
}

const ADD: Doc = Doc {
    usage: "(add a b)",
    summary: "`a` + `b`, wrapping on overflow",
    detail: &[
        "Adds `a` and `b`. On overflow it wraps around, since a function in",
        "the graph must never panic.",
    ],
};

const SUB: Doc = Doc {
    usage: "(sub a b)",
    summary: "`a` - `b`, wrapping on overflow",
    detail: &[
        "Subtracts `b` from `a`. On overflow it wraps around, since a function",
        "in the graph must never panic.",
    ],
};

const MUL: Doc = Doc {
    usage: "(mul a b)",
    summary: "`a` * `b`, wrapping on overflow",
    detail: &[
        "Multiplies `a` by `b`. On overflow it wraps around, since a function",
        "in the graph must never panic.",
    ],
};

const DIV: Doc = Doc {
    usage: "(div a n)",
    summary: "the quotient of `a` / `n`, the one `mod` is the remainder of",
    detail: &[
        "The quotient of `a` divided by `n`, the one `mod` gives the remainder",
        "of: for `n` other than 0, `(add (mul n (div a n)) (mod a n))` is `a`.",
        "So `(div -7 3)` is -3, not -2. `(div a 0)` is 0, and on overflow it",
        "wraps around, since a function in the graph must never panic.",
    ],
};

const MOD: Doc = Doc {
    usage: "(mod a n)",
    summary: "the remainder of `a` / `n`, never negative",
    detail: &[
        "The remainder of `a` divided by `n`, never negative, so `(mod -7 3)`",
        "is 2. `(mod a 0)` is 0, since a function in the graph must never",
        "panic.",
    ],
};

const NEG: Doc = Doc {
    usage: "(neg a)",
    summary: "the negation of `a`, wrapping on overflow",
    detail: &[
        "The negation of `a`. On overflow it wraps around, since a function in",
        "the graph must never panic.",
    ],
};

const EQ: Doc = Doc {
    usage: "(eq a b)",
    summary: "whether `a` and `b` are equal",
    detail: &["Whether `a` and `b` are equal, as integers or as strings."],
};

const GT: Doc = Doc {
    usage: "(gt a b)",
    summary: "whether `a` > `b`",
    detail: &["Whether `a` is greater than `b`."],
};

const AND: Doc = Doc {
    usage: "(and p q)",
    summary: "whether `p` and `q` are both true",
    detail: &["Whether `p` and `q` are both true."],
};

const OR: Doc = Doc {
    usage: "(or p q)",
    summary: "whether `p` or `q` or both are true",
    detail: &["Whether `p` or `q` or both are true."],
};

const NOT: Doc = Doc {
    usage: "(not p)",
    summary: "the opposite of `p`",
    detail: &["The opposite of `p`."],
};

const IF: Doc = Doc {
    usage: "(if c a b)",
    summary: "`a` when `c` is true, else `b`",
    detail: &[
        "`a` when `c` is true, else `b`. Both `a` and `b` are live cells,",
        "computed at every step: `if` picks between them, it doesn't skip one.",
    ],
};

const STR: Doc = Doc {
    usage: "(str n)",
    summary: "`n` in decimal",
    detail: &["`n` in decimal, with a `-` if it's negative."],
};

const CAT: Doc = Doc {
    usage: "(cat s t)",
    summary: "`s` followed by `t`",
    detail: &["`s` followed by `t`."],
};

use Type::{Bool, Int, Str};

const FUNCTIONS: &[Function] = &[
    Function {
        name: "add",
        doc: &ADD,
        params: &[Int, Int],
        result: Int,
        wire: |b, a| int2(b, a, |x, y| x.wrapping_add(*y)),
    },
    Function {
        name: "sub",
        doc: &SUB,
        params: &[Int, Int],
        result: Int,
        wire: |b, a| int2(b, a, |x, y| x.wrapping_sub(*y)),
    },
    Function {
        name: "mul",
        doc: &MUL,
        params: &[Int, Int],
        result: Int,
        wire: |b, a| int2(b, a, |x, y| x.wrapping_mul(*y)),
    },
    Function {
        name: "div",
        doc: &DIV,
        params: &[Int, Int],
        result: Int,
        wire: |b, a| {
            int2(b, a, |x, y| {
                if *y == 0 {
                    0
                } else {
                    x.wrapping_div_euclid(*y)
                }
            })
        },
    },
    Function {
        name: "mod",
        doc: &MOD,
        params: &[Int, Int],
        result: Int,
        wire: |b, a| int2(b, a, |x, y| x.checked_rem_euclid(*y).unwrap_or(0)),
    },
    Function {
        name: "neg",
        doc: &NEG,
        params: &[Int],
        result: Int,
        wire: |b, a| Node::IntCell(a[0].int().map_cell(b, |x| x.wrapping_neg())),
    },
    Function {
        name: "eq",
        doc: &EQ,
        params: &[Int, Int],
        result: Bool,
        wire: |b, a| Node::BoolCell((a[0].int(), a[1].int()).lift(b, |x, y| x == y)),
    },
    Function {
        name: "eq",
        doc: &EQ,
        params: &[Str, Str],
        result: Bool,
        wire: |b, a| Node::BoolCell((a[0].str(), a[1].str()).lift(b, |x, y| x == y)),
    },
    Function {
        name: "gt",
        doc: &GT,
        params: &[Int, Int],
        result: Bool,
        wire: |b, a| Node::BoolCell((a[0].int(), a[1].int()).lift(b, |x, y| x > y)),
    },
    Function {
        name: "and",
        doc: &AND,
        params: &[Bool, Bool],
        result: Bool,
        wire: |b, a| Node::BoolCell((a[0].bool(), a[1].bool()).lift(b, |p, q| *p && *q)),
    },
    Function {
        name: "or",
        doc: &OR,
        params: &[Bool, Bool],
        result: Bool,
        wire: |b, a| Node::BoolCell((a[0].bool(), a[1].bool()).lift(b, |p, q| *p || *q)),
    },
    Function {
        name: "not",
        doc: &NOT,
        params: &[Bool],
        result: Bool,
        wire: |b, a| Node::BoolCell(a[0].bool().map_cell(b, |p| !p)),
    },
    Function {
        name: "if",
        doc: &IF,
        params: &[Bool, Int, Int],
        result: Int,
        wire: |b, a| {
            let (c, x, y) = (a[0].bool(), a[1].int(), a[2].int());
            Node::IntCell((c, x, y).lift(b, |c, x, y| if *c { *x } else { *y }))
        },
    },
    Function {
        name: "if",
        doc: &IF,
        params: &[Bool, Str, Str],
        result: Str,
        wire: |b, a| {
            let (c, x, y) = (a[0].bool(), a[1].str(), a[2].str());
            // A cell's value is read by reference, so the choice is a clone.
            Node::StrCell((c, x, y).lift(b, |c, x, y| if *c { x.clone() } else { y.clone() }))
        },
    },
    Function {
        name: "str",
        doc: &STR,
        params: &[Int],
        result: Str,
        wire: |b, a| Node::StrCell(a[0].int().map_cell(b, |n| n.to_string())),
    },
    Function {
        name: "cat",
        doc: &CAT,
        params: &[Str, Str],
        result: Str,
        wire: |b, a| Node::StrCell((a[0].str(), a[1].str()).lift(b, |p, s| p.to_string() + s)),
    },
];

/// An `Int, Int -> Int` function: a lift of `f` over both cells.
fn int2(b: &mut BuildContext, args: &[Node], f: fn(&i64, &i64) -> i64) -> Node {
    Node::IntCell((args[0].int(), args[1].int()).lift(b, f))
}
