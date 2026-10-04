//! The REPL's parallel enums: [`Type`], the data-free tag the REPL reasons
//! with, and [`Node`], the concrete Bough cell it wires. Every `match` on
//! them is a place a new type has to be added; the findings note counts
//! them.
//!
//! [`Literal`] and [`InputToken`] are two more enums with the same arms.
//! They exist because a literal in a command and the token of an input are
//! typed too, and neither can be erased.

use core::fmt;

use bough::{Build, Cell, Input, Runtime, Trace};

/// The static description of a binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    /// An `i64`.
    Int,
    /// A `bool`.
    Bool,
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Type::Int => "Int",
            Type::Bool => "Bool",
        })
    }
}

/// A binding's storage: a concrete, fully monomorphized Bough cell.
#[derive(Clone, Copy, Debug, Trace)]
pub enum Node {
    /// An `Int` cell.
    IntCell(Cell<i64>),
    /// A `Bool` cell.
    BoolCell(Cell<bool>),
}

impl Node {
    /// The `Int` cell. The REPL checked the argument's [`Type`] before it
    /// sent the command, so any other arm is a bug in the checks; it panics
    /// in graph code, which poisons the runtime.
    pub fn int(self) -> Cell<i64> {
        match self {
            Node::IntCell(cell) => cell,
            other => mismatch(Type::Int, other),
        }
    }

    /// The `Bool` cell, as [`int`](Node::int).
    pub fn bool(self) -> Cell<bool> {
        match self {
            Node::BoolCell(cell) => cell,
            other => mismatch(Type::Bool, other),
        }
    }
}

fn mismatch(wanted: Type, got: Node) -> ! {
    unreachable!("bough-repl: the checks passed a {got:?} where a {wanted} belongs")
}

/// A literal in a command: its syntax gives its [`Type`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Literal {
    /// An integer, such as `-3`.
    Int(i64),
    /// `true` or `false`.
    Bool(bool),
}

impl Literal {
    /// Parses a word as a literal, or `None` if it is not one.
    pub fn parse(word: &str) -> Option<Literal> {
        if let Ok(n) = word.parse::<i64>() {
            return Some(Literal::Int(n));
        }
        match word {
            "true" => Some(Literal::Bool(true)),
            "false" => Some(Literal::Bool(false)),
            _ => None,
        }
    }

    /// The literal's type.
    pub fn ty(&self) -> Type {
        match self {
            Literal::Int(_) => Type::Int,
            Literal::Bool(_) => Type::Bool,
        }
    }

    /// A constant cell holding the literal.
    pub fn constant(self, b: &mut Build) -> Node {
        match self {
            Literal::Int(n) => Node::IntCell(b.constant(n)),
            Literal::Bool(p) => Node::BoolCell(b.constant(p)),
        }
    }

    /// An input cell starting at the literal, and its token.
    pub fn input(self, b: &mut Build) -> (Node, InputToken) {
        match self {
            Literal::Int(n) => {
                let (cell, input) = b.input_cell(n);
                (Node::IntCell(cell), InputToken::Int(input))
            }
            Literal::Bool(p) => {
                let (cell, input) = b.input_cell(p);
                (Node::BoolCell(cell), InputToken::Bool(input))
            }
        }
    }
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Literal::Int(n) => write!(f, "{n}"),
            Literal::Bool(p) => write!(f, "{p}"),
        }
    }
}

/// The token of an input, which `set` sends to.
#[derive(Clone, Copy, Debug, Trace)]
pub enum InputToken {
    /// An `Int` input.
    Int(Input<i64>),
    /// A `Bool` input.
    Bool(Input<bool>),
}

impl InputToken {
    /// Sends `value` in a transaction of its own. The REPL checked that the
    /// literal's type is the input's.
    pub fn send(self, runtime: &mut Runtime, value: Literal) {
        match (self, value) {
            (InputToken::Int(input), Literal::Int(n)) => runtime.send(input, n),
            (InputToken::Bool(input), Literal::Bool(p)) => runtime.send(input, p),
            (input, value) => {
                unreachable!("bough-repl: the checks passed {value} to {input:?}")
            }
        }
    }
}
