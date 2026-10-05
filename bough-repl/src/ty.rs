//! The REPL's parallel enums: [`Type`], the data-free tag the REPL reasons
//! with, and [`Node`], the concrete Bough cell it wires. Every `match` on
//! them is a place a new type has to be added; the findings note counts
//! them.
//!
//! [`Literal`] and [`InputToken`] are two more enums with the same arms.
//! They exist because a literal in a command and the token of an input are
//! typed too, and neither can be erased.

use core::fmt;

use bough::{Build, Cell, Input, Runtime, SendError, Trace};

/// The static description of a binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    /// An `i64`.
    Int,
    /// A `bool`.
    Bool,
    /// A `String`.
    Str,
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Type::Int => "Int",
            Type::Bool => "Bool",
            Type::Str => "Str",
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
    /// A `Str` cell.
    StrCell(Cell<String>),
}

impl Node {
    /// The `Int` cell. The REPL checked the argument's [`Type`] before it
    /// sent the command, so any other arm is a bug in the checks; it panics
    /// in graph code, which poisons the runtime.
    pub fn int(self) -> Cell<i64> {
        self.try_int().unwrap_or_else(Mismatch::unreachable)
    }

    /// The `Bool` cell, as [`int`](Node::int).
    pub fn bool(self) -> Cell<bool> {
        self.try_bool().unwrap_or_else(Mismatch::unreachable)
    }

    /// The `Str` cell, as [`int`](Node::int).
    pub fn str(self) -> Cell<String> {
        self.try_str().unwrap_or_else(Mismatch::unreachable)
    }

    /// The `Int` cell, or the mismatch, for a closure that returns it: the
    /// rollback probe's `try_construct`.
    pub fn try_int(self) -> Result<Cell<i64>, Mismatch> {
        match self {
            Node::IntCell(cell) => Ok(cell),
            got => Err(Mismatch {
                wanted: Type::Int,
                got,
            }),
        }
    }

    /// The `Bool` cell, as [`try_int`](Node::try_int).
    pub fn try_bool(self) -> Result<Cell<bool>, Mismatch> {
        match self {
            Node::BoolCell(cell) => Ok(cell),
            got => Err(Mismatch {
                wanted: Type::Bool,
                got,
            }),
        }
    }

    /// The `Str` cell, as [`try_int`](Node::try_int).
    pub fn try_str(self) -> Result<Cell<String>, Mismatch> {
        match self {
            Node::StrCell(cell) => Ok(cell),
            got => Err(Mismatch {
                wanted: Type::Str,
                got,
            }),
        }
    }
}

/// A [`Node`] of another type than the one asked for: what the REPL's
/// checks rule out, and what a definition sent past them, through the
/// graph API, can still carry.
#[derive(Clone, Copy, Debug)]
pub struct Mismatch {
    /// The type asked for.
    pub wanted: Type,
    /// The node that came.
    pub got: Node,
}

impl Mismatch {
    /// The `unreachable!` arm: a bug in the checks, which panics in graph
    /// code and poisons the runtime.
    pub fn unreachable<T>(self) -> T {
        unreachable!("bough-repl: {self}")
    }
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Mismatch { wanted, got } = self;
        write!(f, "the checks passed a {got:?} where a {wanted} belongs")
    }
}

/// A literal in a command: its syntax gives its [`Type`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Literal {
    /// An integer, such as `-3`.
    Int(i64),
    /// `true` or `false`.
    Bool(bool),
    /// A string in double quotes, such as `"Fizz"`.
    Str(String),
}

impl Literal {
    /// Parses a word as a literal, or `None` if it is not one. A string
    /// is one word, so it has no spaces, and no escapes.
    pub fn parse(word: &str) -> Option<Literal> {
        if let Ok(n) = word.parse::<i64>() {
            return Some(Literal::Int(n));
        }
        if let Some(text) = word.strip_prefix('"').and_then(|w| w.strip_suffix('"')) {
            return Some(Literal::Str(text.to_string()));
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
            Literal::Str(_) => Type::Str,
        }
    }

    /// A constant cell holding the literal.
    pub fn constant(self, b: &mut Build) -> Node {
        match self {
            Literal::Int(n) => Node::IntCell(b.constant(n)),
            Literal::Bool(p) => Node::BoolCell(b.constant(p)),
            Literal::Str(text) => Node::StrCell(b.constant(text)),
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
            Literal::Str(text) => {
                let (cell, input) = b.input_cell(text);
                (Node::StrCell(cell), InputToken::Str(input))
            }
        }
    }
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Literal::Int(n) => write!(f, "{n}"),
            Literal::Bool(p) => write!(f, "{p}"),
            Literal::Str(text) => write!(f, "\"{text}\""),
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
    /// A `Str` input.
    Str(Input<String>),
}

impl InputToken {
    /// Sends `value` in a transaction of its own. The REPL checked that the
    /// literal's type is the input's.
    pub fn send(self, runtime: &mut Runtime, value: Literal) {
        match (self, value) {
            (InputToken::Int(input), Literal::Int(n)) => runtime.send(input, n),
            (InputToken::Bool(input), Literal::Bool(p)) => runtime.send(input, p),
            (InputToken::Str(input), Literal::Str(text)) => runtime.send(input, text),
            (input, value) => {
                unreachable!("bough-repl: the checks passed {value} to {input:?}")
            }
        }
    }

    /// [`send`](InputToken::send), returning the send's error.
    pub fn try_send(self, runtime: &mut Runtime, value: Literal) -> Result<(), SendError> {
        match (self, value) {
            (InputToken::Int(input), Literal::Int(n)) => runtime.try_send(input, n),
            (InputToken::Bool(input), Literal::Bool(p)) => runtime.try_send(input, p),
            (InputToken::Str(input), Literal::Str(text)) => runtime.try_send(input, text),
            (input, value) => {
                unreachable!("bough-repl: the checks passed {value} to {input:?}")
            }
        }
    }
}
