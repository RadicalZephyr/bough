//! The function registry: each entry's signature over [`Type`] tags, which
//! the REPL checks, and its wiring function, which runs in a `construct`
//! closure and matches on [`Node`] arms to reach the concrete cells.
//!
//! No polymorphism: a signature names concrete types, and an operation over
//! two types is two entries under one name, which `def` chooses between by
//! the types of its arguments. Every function is total, since a panic in a
//! cell's function poisons the runtime: arithmetic wraps, and `mod 0` is 0.
//! All but one: `boom` panics on purpose, for the rollback probe, which
//! needs a function that fails on some values only.

use bough::{Build, Lift};

use crate::ty::{Mismatch, Node, Type};

/// Builds a function's cell from its argument cells, inside a `construct`,
/// or says which argument was of the wrong type, which the REPL's checks
/// rule out.
pub type Wire = fn(&mut Build, &[Node]) -> Result<Node, Mismatch>;

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

/// The functions called `name`, one per signature.
pub fn named(name: &str) -> impl Iterator<Item = &'static Function> {
    FUNCTIONS.iter().filter(move |f| f.name == name)
}

use Type::{Bool, Int, Str};

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
        wire: |b, a| {
            Ok(Node::IntCell(
                a[0].try_int()?.map_cell(b, |x| x.wrapping_neg()),
            ))
        },
    },
    Function {
        name: "eq",
        params: &[Int, Int],
        result: Bool,
        wire: |b, a| {
            Ok(Node::BoolCell(
                (a[0].try_int()?, a[1].try_int()?).lift(b, |x, y| x == y),
            ))
        },
    },
    Function {
        name: "gt",
        params: &[Int, Int],
        result: Bool,
        wire: |b, a| {
            Ok(Node::BoolCell(
                (a[0].try_int()?, a[1].try_int()?).lift(b, |x, y| x > y),
            ))
        },
    },
    Function {
        name: "and",
        params: &[Bool, Bool],
        result: Bool,
        wire: |b, a| {
            Ok(Node::BoolCell(
                (a[0].try_bool()?, a[1].try_bool()?).lift(b, |p, q| *p && *q),
            ))
        },
    },
    Function {
        name: "or",
        params: &[Bool, Bool],
        result: Bool,
        wire: |b, a| {
            Ok(Node::BoolCell(
                (a[0].try_bool()?, a[1].try_bool()?).lift(b, |p, q| *p || *q),
            ))
        },
    },
    Function {
        name: "not",
        params: &[Bool],
        result: Bool,
        wire: |b, a| Ok(Node::BoolCell(a[0].try_bool()?.map_cell(b, |p| !p))),
    },
    Function {
        name: "if",
        params: &[Bool, Int, Int],
        result: Int,
        wire: |b, a| {
            let (c, x, y) = (a[0].try_bool()?, a[1].try_int()?, a[2].try_int()?);
            Ok(Node::IntCell(
                (c, x, y).lift(b, |c, x, y| if *c { *x } else { *y }),
            ))
        },
    },
    Function {
        name: "if",
        params: &[Bool, Str, Str],
        result: Str,
        wire: |b, a| {
            let (c, x, y) = (a[0].try_bool()?, a[1].try_str()?, a[2].try_str()?);
            // A cell's value is read by reference, so the choice is a clone.
            Ok(Node::StrCell(
                (c, x, y).lift(b, |c, x, y| if *c { x.clone() } else { y.clone() }),
            ))
        },
    },
    Function {
        name: "str",
        params: &[Int],
        result: Str,
        wire: |b, a| {
            Ok(Node::StrCell(
                a[0].try_int()?.map_cell(b, |n| n.to_string()),
            ))
        },
    },
    Function {
        name: "boom",
        params: &[Int],
        result: Int,
        wire: |b, a| Ok(Node::IntCell(a[0].try_int()?.map_cell(b, |n| boom(*n)))),
    },
];

/// `boom`: its argument, except that it panics on a multiple of 7 other
/// than 0, so a tick, which starts at 0, can be watched through it.
pub fn boom(n: i64) -> i64 {
    assert!(n == 0 || n % 7 != 0, "boom on {n}");
    n
}

/// An `Int, Int -> Int` function: a lift of `f` over both cells.
fn int2(b: &mut Build, args: &[Node], f: fn(&i64, &i64) -> i64) -> Result<Node, Mismatch> {
    Ok(Node::IntCell(
        (args[0].try_int()?, args[1].try_int()?).lift(b, f),
    ))
}
