//! The function registry: each entry's signature over [`Type`] tags, which
//! the REPL checks, and its wiring function, which runs in a `construct`
//! closure and matches on [`Node`] arms to reach the concrete cells.
//!
//! No polymorphism: a signature names concrete types, and an operation over
//! two types is two entries. Every function is total, since a panic in a
//! cell's function poisons the runtime: arithmetic wraps, and `mod 0` is 0.

use bough::{Build, Lift};

use crate::ty::{Node, Type};

/// Builds a function's cell from its argument cells, inside a `construct`.
pub type Wire = fn(&mut Build, &[Node]) -> Node;

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
}

/// The function called `name`.
pub fn lookup(name: &str) -> Option<&'static Function> {
    FUNCTIONS.iter().find(|f| f.name == name)
}

use Type::{Bool, Int};

const FUNCTIONS: &[Function] = &[
    Function {
        name: "add",
        params: &[Int, Int],
        result: Int,
        wire: |b, a| int2(b, a, |x, y| x.wrapping_add(*y)),
    },
    Function {
        name: "sub",
        params: &[Int, Int],
        result: Int,
        wire: |b, a| int2(b, a, |x, y| x.wrapping_sub(*y)),
    },
    Function {
        name: "mul",
        params: &[Int, Int],
        result: Int,
        wire: |b, a| int2(b, a, |x, y| x.wrapping_mul(*y)),
    },
    Function {
        name: "mod",
        params: &[Int, Int],
        result: Int,
        wire: |b, a| int2(b, a, |x, y| x.checked_rem_euclid(*y).unwrap_or(0)),
    },
    Function {
        name: "neg",
        params: &[Int],
        result: Int,
        wire: |b, a| Node::IntCell(a[0].int().map_cell(b, |x| x.wrapping_neg())),
    },
    Function {
        name: "eq",
        params: &[Int, Int],
        result: Bool,
        wire: |b, a| Node::BoolCell((a[0].int(), a[1].int()).lift(b, |x, y| x == y)),
    },
    Function {
        name: "gt",
        params: &[Int, Int],
        result: Bool,
        wire: |b, a| Node::BoolCell((a[0].int(), a[1].int()).lift(b, |x, y| x > y)),
    },
    Function {
        name: "and",
        params: &[Bool, Bool],
        result: Bool,
        wire: |b, a| Node::BoolCell((a[0].bool(), a[1].bool()).lift(b, |p, q| *p && *q)),
    },
    Function {
        name: "or",
        params: &[Bool, Bool],
        result: Bool,
        wire: |b, a| Node::BoolCell((a[0].bool(), a[1].bool()).lift(b, |p, q| *p || *q)),
    },
    Function {
        name: "not",
        params: &[Bool],
        result: Bool,
        wire: |b, a| Node::BoolCell(a[0].bool().map_cell(b, |p| !p)),
    },
    Function {
        name: "if",
        params: &[Bool, Int, Int],
        result: Int,
        wire: |b, a| {
            let (c, x, y) = (a[0].bool(), a[1].int(), a[2].int());
            Node::IntCell((c, x, y).lift(b, |c, x, y| if *c { *x } else { *y }))
        },
    },
];

/// An `Int, Int -> Int` function: a lift of `f` over both cells.
fn int2(b: &mut Build, args: &[Node], f: fn(&i64, &i64) -> i64) -> Node {
    Node::IntCell((args[0].int(), args[1].int()).lift(b, f))
}
