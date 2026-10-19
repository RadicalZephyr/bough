//! The program description, the same as `haskell/Oracle/Program.hs`.
//!
//! A [`Program`] prints, through [`Display`](fmt::Display), as one line in the
//! syntax of Haskell's derived `Read` for `Oracle.Program.Program`, which is
//! what the oracle process reads. The Rust names are spelled out; each
//! variant's documentation names the Haskell constructor it prints as. A
//! negative number prints in parentheses, `Lit (-5)`, as `Show` writes it and
//! as the Haskell Report's `Read` requires of an argument. GHC's derived
//! `Read` accepts it bare as well, so the oracle would not notice a missing
//! pair; this module's tests hold the printer to them.
//!
//! Closures cannot cross a process boundary, so every function is an
//! [`Expression`] that the oracle and the engine evaluate the same way: 64-bit
//! wrapping arithmetic (`Int` in GHC wraps; Rust uses `wrapping_*`),
//! [`Expression::Modulo`] with a positive divisor only (Haskell `mod`, Rust
//! `rem_euclid`), a boolean argument read as 0 or 1, and a boolean result
//! true when nonzero.
//!
//! The oracle checks a program before it runs it, and answers `ERR` with the
//! node at fault when a reference, a type or an argument does not fit.

use core::fmt::{self, Write as _};
use core::ops;

/// An instant: `[k]` for external transaction k, `[0]` for the build, and
/// `t ++ [n]` for the n-th child transaction of `t`.
pub type Time = Vec<i64>;

/// A program the oracle answers: `Program Window [Input] [Def] [Int]
/// [[(Int, V)]]`.
///
/// Node `i` is `definitions[i]`. A definition refers only to nodes defined
/// before it; a loop is the one way to use a node before its definition
/// exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    /// Which part of each observed node the answer carries.
    pub window: Window,
    /// The inputs, by index.
    pub inputs: Vec<Input>,
    /// The nodes of the top level.
    pub definitions: Vec<Definition>,
    /// The top-level nodes the answer carries, in this order.
    pub observe: Vec<usize>,
    /// One entry per external transaction k = 1, 2, …, each the sends
    /// (input index, value) in send order. Transaction k is at time `[k]`.
    pub schedule: Vec<Vec<(usize, Value)>>,
}

/// Which part of each observed node the answer carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    /// What the engine comparison uses: a cell's value after transaction
    /// zero and its child transactions, `at (steps c) [1]`, and its steps
    /// from `[1]`; a stream's events from `[1]`. Prints as
    /// `FromFirstTransaction`.
    FromFirstTransaction,
    /// A cell's initial value and every step, a stream's every event: for
    /// vectors whose events sit at `[0]`. Prints as `Everything`.
    Everything,
}

/// An input: the type of its events, and the function that combines two
/// sends in one transaction. `Input Ty (Maybe E)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Input {
    /// The type of the input's events: [`Type::Integer`], [`Type::Boolean`]
    /// or [`Type::List`].
    pub event_type: Type,
    /// The coalescing function, [`Expression::Argument`] the earlier send and
    /// [`Expression::SecondArgument`] the later. An input without one may be
    /// sent at most once per transaction.
    pub coalesce: Option<Expression>,
}

impl Input {
    /// An input that may be sent at most once per transaction.
    pub fn new(event_type: Type) -> Input {
        Input {
            event_type,
            coalesce: None,
        }
    }

    /// An input whose sends in one transaction fold left with `function`,
    /// first send on the left.
    pub fn coalescing(event_type: Type, function: Expression) -> Input {
        Input {
            event_type,
            coalesce: Some(function),
        }
    }
}

/// A value in a literal stream, in a send, or in [`Definition::MapTo`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// Prints as `I n`.
    Integer(i64),
    /// Prints as `B True` or `B False`.
    Boolean(bool),
    /// Prints as `L [n, …]`.
    List(Vec<i64>),
}

/// The type of an event or of a cell's value. Tokens are values too, for the
/// higher-order operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Type {
    /// Prints as `TInt`.
    Integer,
    /// Prints as `TBool`.
    Boolean,
    /// Prints as `TList`: a list of integers.
    List,
    /// Prints as `TStreamOf`: a stream token.
    StreamOf(Box<Type>),
    /// Prints as `TCellOf`: a cell token.
    CellOf(Box<Type>),
}

impl Type {
    /// A stream of the given type.
    pub fn stream_of(event_type: Type) -> Type {
        Type::StreamOf(Box::new(event_type))
    }

    /// A cell of the given type.
    pub fn cell_of(value_type: Type) -> Type {
        Type::CellOf(Box::new(value_type))
    }
}

/// A reference to a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reference {
    /// Node i of the program's top level. Prints as `N i`.
    TopLevel(usize),
    /// Node j of the innermost enclosing construct body. Prints as `Local j`.
    Local(usize),
}

/// An expression over integers. Each [`Definition`] says what its arguments
/// are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expression {
    /// The first argument, `ArgumentAt(0)`. Prints as `Arg`.
    Argument,
    /// The second argument, `ArgumentAt(1)`. Prints as `Arg2`.
    SecondArgument,
    /// Argument i: in [`Definition::Lift`], the value of cell i. Prints as
    /// `ArgN i`.
    ArgumentAt(usize),
    /// Inside a construct body: the construct's event. Prints as `CArg`.
    ConstructEvent,
    /// The value a cell had before the scope's instant: the construct's
    /// instant inside a body, the build instant `[0]` at the top level.
    /// Prints as `Sample`.
    Sample(Reference),
    /// Prints as `Lit n`.
    Literal(i64),
    /// Wrapping addition. Prints as `Add`.
    Add(Box<Expression>, Box<Expression>),
    /// Wrapping subtraction. Prints as `Sub`.
    Subtract(Box<Expression>, Box<Expression>),
    /// Wrapping multiplication. Prints as `Mul`.
    Multiply(Box<Expression>, Box<Expression>),
    /// Haskell's `mod`, Rust's `rem_euclid`; the divisor is positive. Prints
    /// as `Mod`.
    Modulo(Box<Expression>, i64),
    /// Prints as `Max`.
    Maximum(Box<Expression>, Box<Expression>),
    /// Prints as `Min`.
    Minimum(Box<Expression>, Box<Expression>),
    /// 1 when equal, 0 otherwise. Prints as `Eq`.
    Equal(Box<Expression>, Box<Expression>),
    /// 1 when the first is less, 0 otherwise. Prints as `Lt`.
    LessThan(Box<Expression>, Box<Expression>),
    /// 1 for 0, 0 for anything else. Prints as `Not`.
    Not(Box<Expression>),
    /// The second when the first is nonzero, the third otherwise. Prints as
    /// `If`.
    If(Box<Expression>, Box<Expression>, Box<Expression>),
}

impl Expression {
    /// `Mod self divisor`.
    pub fn modulo(self, divisor: i64) -> Expression {
        Expression::Modulo(Box::new(self), divisor)
    }

    /// `Max self other`.
    pub fn maximum(self, other: Expression) -> Expression {
        Expression::Maximum(Box::new(self), Box::new(other))
    }

    /// `Min self other`.
    pub fn minimum(self, other: Expression) -> Expression {
        Expression::Minimum(Box::new(self), Box::new(other))
    }

    /// `Eq self other`.
    pub fn equal(self, other: Expression) -> Expression {
        Expression::Equal(Box::new(self), Box::new(other))
    }

    /// `Lt self other`.
    pub fn less_than(self, other: Expression) -> Expression {
        Expression::LessThan(Box::new(self), Box::new(other))
    }

    /// `If condition then otherwise`.
    pub fn if_then_else(
        condition: Expression,
        then: Expression,
        otherwise: Expression,
    ) -> Expression {
        Expression::If(Box::new(condition), Box::new(then), Box::new(otherwise))
    }
}

impl ops::Add for Expression {
    type Output = Expression;

    fn add(self, other: Expression) -> Expression {
        Expression::Add(Box::new(self), Box::new(other))
    }
}

impl ops::Sub for Expression {
    type Output = Expression;

    fn sub(self, other: Expression) -> Expression {
        Expression::Subtract(Box::new(self), Box::new(other))
    }
}

impl ops::Mul for Expression {
    type Output = Expression;

    fn mul(self, other: Expression) -> Expression {
        Expression::Multiply(Box::new(self), Box::new(other))
    }
}

impl ops::Not for Expression {
    type Output = Expression;

    fn not(self) -> Expression {
        Expression::Not(Box::new(self))
    }
}

/// One node. Adapters are separate definitions; the engine side fuses a
/// linear run of them into the materializer that consumes it. Events are
/// integers unless a variant says otherwise, and wherever an integer is
/// read, a boolean reads as 0 or 1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Definition {
    /// The stream of an input. Prints as `SInput`.
    Input(usize),
    /// `input_cell`: a hold over an input, with an initial value. Prints as
    /// `CInput`.
    InputCell {
        /// The input's index.
        input: usize,
        /// The initial value.
        initial: Expression,
    },
    /// A stream that never fires. Prints as `SNever`.
    Never(Type),
    /// A cell of integers that never changes. Prints as `CConstant`.
    Constant(Expression),
    /// The stream of the given events, times increasing: the text's
    /// `MkStream`, which the engine cannot build. Prints as `SLit`.
    Literal {
        /// The type of the events.
        event_type: Type,
        /// The events.
        events: Vec<(Time, Value)>,
    },
    /// Prints as `SMap`.
    Map {
        /// [`Expression::Argument`]: the event.
        function: Expression,
        /// The stream.
        source: Reference,
    },
    /// Keeps the events for which the predicate is nonzero. Prints as
    /// `SFilter`.
    Filter {
        /// [`Expression::Argument`]: the event.
        predicate: Expression,
        /// The stream.
        source: Reference,
    },
    /// Keeps the events for which `keep` is nonzero, and emits `function`.
    /// Prints as `SFilterMap`.
    FilterMap {
        /// [`Expression::Argument`]: the event.
        keep: Expression,
        /// [`Expression::Argument`]: the event.
        function: Expression,
        /// The stream.
        source: Reference,
    },
    /// Replaces each event of any type with one value. Prints as `SMapTo`.
    MapTo {
        /// The value.
        value: Value,
        /// The stream.
        source: Reference,
    },
    /// Prints as `SSnapshot`.
    Snapshot {
        /// [`Expression::Argument`]: the event; [`Expression::SecondArgument`]:
        /// the cell's value before the instant.
        function: Expression,
        /// The stream.
        source: Reference,
        /// An integer cell.
        cell: Reference,
    },
    /// Keeps the events of any stream while a boolean cell was true before
    /// the instant. Prints as `SGate`.
    Gate {
        /// The stream.
        source: Reference,
        /// A boolean cell.
        cell: Reference,
    },
    /// The first event at or after the node's creation. Prints as `SOnce`.
    Once(Reference),
    /// An integer to a list. Prints as `SMapList`.
    MapList {
        /// The list's length is this `mod` 4; [`Expression::Argument`]: the
        /// event.
        length: Expression,
        /// Element i; [`Expression::Argument`]: the event,
        /// [`Expression::SecondArgument`]: i.
        element: Expression,
        /// The stream.
        source: Reference,
    },
    /// An integer to one of the listed streams. Prints as `SPickStream`.
    PickStream {
        /// The index, `mod` the number of streams; [`Expression::Argument`]:
        /// the event.
        index: Expression,
        /// The streams, all of one type.
        streams: Vec<Reference>,
        /// The stream of integers.
        source: Reference,
    },
    /// An integer to one of the listed cells. Prints as `SPickCell`.
    PickCell {
        /// The index, `mod` the number of cells; [`Expression::Argument`]:
        /// the event.
        index: Expression,
        /// The cells, all of one type.
        cells: Vec<Reference>,
        /// The stream of integers.
        source: Reference,
    },
    /// An identity in the semantics; `node` in the engine. Prints as `SNode`.
    Node(Reference),
    /// An identity in the semantics; `share` in the engine. Prints as
    /// `SShare`.
    Share(Reference),
    /// Prints as `SMerge`.
    Merge {
        /// [`Expression::Argument`]: the left event;
        /// [`Expression::SecondArgument`]: the right.
        function: Expression,
        /// The left stream.
        left: Reference,
        /// The right stream.
        right: Reference,
    },
    /// Merges two streams of any one type, the left winning. Prints as
    /// `SOrElse`.
    OrElse {
        /// The left stream.
        left: Reference,
        /// The right stream.
        right: Reference,
    },
    /// A stream of lists to a stream of integers. Prints as `SSplit`.
    Split(Reference),
    /// Prints as `SDefer`.
    Defer(Reference),
    /// Prints as `SScan`.
    Scan {
        /// The initial state.
        initial: Expression,
        /// The output; [`Expression::Argument`]: the event,
        /// [`Expression::SecondArgument`]: the state.
        output: Expression,
        /// The new state, with the same arguments.
        state: Expression,
        /// The stream.
        source: Reference,
    },
    /// Prints as `SSteps`.
    Steps(Reference),
    /// Prints as `SStepsWithCurrent`.
    StepsWithCurrent(Reference),
    /// A cell of streams to a stream. Prints as `SSwitch`.
    SwitchStream(Reference),
    /// Runs the body at each event's instant. Prints as `SConstruct`.
    Construct {
        /// The body.
        body: Body,
        /// The stream.
        source: Reference,
    },
    /// Prints as `CHold`.
    Hold {
        /// The initial value.
        initial: Expression,
        /// A stream of integers or booleans.
        source: Reference,
    },
    /// A hold of streams. Prints as `CHoldStream`.
    HoldStream {
        /// The initial stream.
        initial: Reference,
        /// The stream of streams.
        source: Reference,
    },
    /// A hold of cells. Prints as `CHoldCell`.
    HoldCell {
        /// The initial cell.
        initial: Reference,
        /// The stream of cells.
        source: Reference,
    },
    /// A constant cell holding a stream. Prints as `CConstantStream`.
    ConstantStream(Reference),
    /// A constant cell holding a cell. Prints as `CConstantCell`.
    ConstantCell(Reference),
    /// Prints as `CAccum`.
    Accumulate {
        /// The initial state.
        initial: Expression,
        /// The new state; [`Expression::Argument`]: the event,
        /// [`Expression::SecondArgument`]: the state.
        function: Expression,
        /// The stream.
        source: Reference,
    },
    /// The same denotation as [`Definition::Accumulate`]; the engine uses
    /// `accumulate_mut`. Prints as `CAccumMut`.
    AccumulateMut {
        /// The initial state.
        initial: Expression,
        /// The new state; [`Expression::Argument`]: the event,
        /// [`Expression::SecondArgument`]: the state.
        function: Expression,
        /// The stream.
        source: Reference,
    },
    /// An integer cell to an integer cell. Prints as `CMapCell`.
    MapCell {
        /// [`Expression::Argument`]: the cell's value.
        function: Expression,
        /// The cell.
        cell: Reference,
    },
    /// An integer cell to a boolean cell: nonzero is true. Prints as
    /// `CToBool`.
    ToBoolean(Reference),
    /// An integer cell to a cell of one of the listed cells. Prints as
    /// `CMapPickCell`.
    MapPickCell {
        /// The index, `mod` the number of cells; [`Expression::Argument`]:
        /// the value.
        index: Expression,
        /// The cells, all of one type.
        cells: Vec<Reference>,
        /// The cell of integers.
        cell: Reference,
    },
    /// Two to six integer cells. Prints as `CLift`.
    Lift {
        /// [`Expression::ArgumentAt`] i: the value of cell i.
        function: Expression,
        /// The cells.
        cells: Vec<Reference>,
    },
    /// A cell of cells to a cell. Prints as `CSwitch`.
    SwitchCell(Reference),
    /// A forward cell, closed later by [`Definition::Close`]. Prints as
    /// `CLoop`.
    CellLoop(Type),
    /// A forward stream, closed later by [`Definition::Close`]. Prints as
    /// `SLoop`.
    StreamLoop(Type),
    /// Closes a loop of the same scope with a definition. It defines no node
    /// that can be used. Prints as `Close`.
    Close {
        /// The loop's node index, in the same scope.
        forward: usize,
        /// The definition.
        definition: Reference,
    },
}

/// A construct body: its local nodes, and what it emits. References inside
/// may be [`Reference::TopLevel`] or [`Reference::Local`]. `Body [Def]
/// Result`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Body {
    /// The body's nodes; `Reference::Local(j)` is `definitions[j]`.
    pub definitions: Vec<Definition>,
    /// What the body emits for each event.
    pub result: BodyResult,
}

/// What a construct body emits for each event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BodyResult {
    /// An integer. Prints as `RValue`.
    Value(Expression),
    /// A token: a stream or a cell. Prints as `RNode`.
    Node(Reference),
}

// ----- the printer -----

/// Writes a value in the syntax of Haskell's derived `Read`.
trait Haskell {
    /// Writes `self`. An application with arguments in argument position
    /// (`nested`) is parenthesized; so is a negative number, always.
    fn write(&self, out: &mut String, nested: bool);
}

/// Writes a constructor applied to its arguments.
fn application(out: &mut String, nested: bool, constructor: &str, arguments: &[&dyn Haskell]) {
    let parenthesized = nested && !arguments.is_empty();
    if parenthesized {
        out.push('(');
    }
    out.push_str(constructor);
    for argument in arguments {
        out.push(' ');
        argument.write(out, true);
    }
    if parenthesized {
        out.push(')');
    }
}

impl Haskell for i64 {
    fn write(&self, out: &mut String, _nested: bool) {
        if *self < 0 {
            // `write!` into a `String` cannot fail.
            let _ = write!(out, "({self})");
        } else {
            let _ = write!(out, "{self}");
        }
    }
}

impl Haskell for usize {
    fn write(&self, out: &mut String, _nested: bool) {
        let _ = write!(out, "{self}");
    }
}

impl Haskell for bool {
    fn write(&self, out: &mut String, _nested: bool) {
        out.push_str(if *self { "True" } else { "False" });
    }
}

impl<T: Haskell> Haskell for Vec<T> {
    fn write(&self, out: &mut String, _nested: bool) {
        out.push('[');
        for (index, element) in self.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            element.write(out, false);
        }
        out.push(']');
    }
}

impl<A: Haskell, B: Haskell> Haskell for (A, B) {
    fn write(&self, out: &mut String, _nested: bool) {
        out.push('(');
        self.0.write(out, false);
        out.push(',');
        self.1.write(out, false);
        out.push(')');
    }
}

impl<T: Haskell> Haskell for Option<T> {
    fn write(&self, out: &mut String, nested: bool) {
        match self {
            None => out.push_str("Nothing"),
            Some(value) => application(out, nested, "Just", &[value]),
        }
    }
}

impl<T: Haskell> Haskell for Box<T> {
    fn write(&self, out: &mut String, nested: bool) {
        (**self).write(out, nested);
    }
}

impl Haskell for Program {
    fn write(&self, out: &mut String, nested: bool) {
        application(
            out,
            nested,
            "Program",
            &[
                &self.window,
                &self.inputs,
                &self.definitions,
                &self.observe,
                &self.schedule,
            ],
        );
    }
}

impl Haskell for Window {
    fn write(&self, out: &mut String, _nested: bool) {
        out.push_str(match self {
            Window::FromFirstTransaction => "FromFirstTransaction",
            Window::Everything => "Everything",
        });
    }
}

impl Haskell for Input {
    fn write(&self, out: &mut String, nested: bool) {
        application(out, nested, "Input", &[&self.event_type, &self.coalesce]);
    }
}

impl Haskell for Value {
    fn write(&self, out: &mut String, nested: bool) {
        match self {
            Value::Integer(value) => application(out, nested, "I", &[value]),
            Value::Boolean(value) => application(out, nested, "B", &[value]),
            Value::List(values) => application(out, nested, "L", &[values]),
        }
    }
}

impl Haskell for Type {
    fn write(&self, out: &mut String, nested: bool) {
        match self {
            Type::Integer => out.push_str("TInt"),
            Type::Boolean => out.push_str("TBool"),
            Type::List => out.push_str("TList"),
            Type::StreamOf(inner) => application(out, nested, "TStreamOf", &[inner]),
            Type::CellOf(inner) => application(out, nested, "TCellOf", &[inner]),
        }
    }
}

impl Haskell for Reference {
    fn write(&self, out: &mut String, nested: bool) {
        match self {
            Reference::TopLevel(index) => application(out, nested, "N", &[index]),
            Reference::Local(index) => application(out, nested, "Local", &[index]),
        }
    }
}

impl Haskell for Expression {
    fn write(&self, out: &mut String, nested: bool) {
        match self {
            Expression::Argument => out.push_str("Arg"),
            Expression::SecondArgument => out.push_str("Arg2"),
            Expression::ArgumentAt(index) => application(out, nested, "ArgN", &[index]),
            Expression::ConstructEvent => out.push_str("CArg"),
            Expression::Sample(cell) => application(out, nested, "Sample", &[cell]),
            Expression::Literal(value) => application(out, nested, "Lit", &[value]),
            Expression::Add(left, right) => application(out, nested, "Add", &[left, right]),
            Expression::Subtract(left, right) => application(out, nested, "Sub", &[left, right]),
            Expression::Multiply(left, right) => application(out, nested, "Mul", &[left, right]),
            Expression::Modulo(value, divisor) => {
                application(out, nested, "Mod", &[value, divisor]);
            }
            Expression::Maximum(left, right) => application(out, nested, "Max", &[left, right]),
            Expression::Minimum(left, right) => application(out, nested, "Min", &[left, right]),
            Expression::Equal(left, right) => application(out, nested, "Eq", &[left, right]),
            Expression::LessThan(left, right) => application(out, nested, "Lt", &[left, right]),
            Expression::Not(value) => application(out, nested, "Not", &[value]),
            Expression::If(condition, then, otherwise) => {
                application(out, nested, "If", &[condition, then, otherwise]);
            }
        }
    }
}

impl Haskell for Definition {
    fn write(&self, out: &mut String, nested: bool) {
        match self {
            Definition::Input(input) => application(out, nested, "SInput", &[input]),
            Definition::InputCell { input, initial } => {
                application(out, nested, "CInput", &[input, initial]);
            }
            Definition::Never(event_type) => application(out, nested, "SNever", &[event_type]),
            Definition::Constant(value) => application(out, nested, "CConstant", &[value]),
            Definition::Literal { event_type, events } => {
                application(out, nested, "SLit", &[event_type, events]);
            }
            Definition::Map { function, source } => {
                application(out, nested, "SMap", &[function, source]);
            }
            Definition::Filter { predicate, source } => {
                application(out, nested, "SFilter", &[predicate, source]);
            }
            Definition::FilterMap {
                keep,
                function,
                source,
            } => application(out, nested, "SFilterMap", &[keep, function, source]),
            Definition::MapTo { value, source } => {
                application(out, nested, "SMapTo", &[value, source]);
            }
            Definition::Snapshot {
                function,
                source,
                cell,
            } => application(out, nested, "SSnapshot", &[function, source, cell]),
            Definition::Gate { source, cell } => application(out, nested, "SGate", &[source, cell]),
            Definition::Once(source) => application(out, nested, "SOnce", &[source]),
            Definition::MapList {
                length,
                element,
                source,
            } => application(out, nested, "SMapList", &[length, element, source]),
            Definition::PickStream {
                index,
                streams,
                source,
            } => application(out, nested, "SPickStream", &[index, streams, source]),
            Definition::PickCell {
                index,
                cells,
                source,
            } => application(out, nested, "SPickCell", &[index, cells, source]),
            Definition::Node(source) => application(out, nested, "SNode", &[source]),
            Definition::Share(source) => application(out, nested, "SShare", &[source]),
            Definition::Merge {
                function,
                left,
                right,
            } => application(out, nested, "SMerge", &[function, left, right]),
            Definition::OrElse { left, right } => {
                application(out, nested, "SOrElse", &[left, right]);
            }
            Definition::Split(source) => application(out, nested, "SSplit", &[source]),
            Definition::Defer(source) => application(out, nested, "SDefer", &[source]),
            Definition::Scan {
                initial,
                output,
                state,
                source,
            } => application(out, nested, "SScan", &[initial, output, state, source]),
            Definition::Steps(cell) => application(out, nested, "SSteps", &[cell]),
            Definition::StepsWithCurrent(cell) => {
                application(out, nested, "SStepsWithCurrent", &[cell]);
            }
            Definition::SwitchStream(cell) => application(out, nested, "SSwitch", &[cell]),
            Definition::Construct { body, source } => {
                application(out, nested, "SConstruct", &[body, source]);
            }
            Definition::Hold { initial, source } => {
                application(out, nested, "CHold", &[initial, source]);
            }
            Definition::HoldStream { initial, source } => {
                application(out, nested, "CHoldStream", &[initial, source]);
            }
            Definition::HoldCell { initial, source } => {
                application(out, nested, "CHoldCell", &[initial, source]);
            }
            Definition::ConstantStream(stream) => {
                application(out, nested, "CConstantStream", &[stream]);
            }
            Definition::ConstantCell(cell) => application(out, nested, "CConstantCell", &[cell]),
            Definition::Accumulate {
                initial,
                function,
                source,
            } => application(out, nested, "CAccum", &[initial, function, source]),
            Definition::AccumulateMut {
                initial,
                function,
                source,
            } => application(out, nested, "CAccumMut", &[initial, function, source]),
            Definition::MapCell { function, cell } => {
                application(out, nested, "CMapCell", &[function, cell]);
            }
            Definition::ToBoolean(cell) => application(out, nested, "CToBool", &[cell]),
            Definition::MapPickCell { index, cells, cell } => {
                application(out, nested, "CMapPickCell", &[index, cells, cell]);
            }
            Definition::Lift { function, cells } => {
                application(out, nested, "CLift", &[function, cells]);
            }
            Definition::SwitchCell(cell) => application(out, nested, "CSwitch", &[cell]),
            Definition::CellLoop(value_type) => application(out, nested, "CLoop", &[value_type]),
            Definition::StreamLoop(event_type) => {
                application(out, nested, "SLoop", &[event_type]);
            }
            Definition::Close {
                forward,
                definition,
            } => application(out, nested, "Close", &[forward, definition]),
        }
    }
}

impl Haskell for Body {
    fn write(&self, out: &mut String, nested: bool) {
        application(out, nested, "Body", &[&self.definitions, &self.result]);
    }
}

impl Haskell for BodyResult {
    fn write(&self, out: &mut String, nested: bool) {
        match self {
            BodyResult::Value(value) => application(out, nested, "RValue", &[value]),
            BodyResult::Node(node) => application(out, nested, "RNode", &[node]),
        }
    }
}

/// Implements `Display` as the Haskell syntax, at the top level.
macro_rules! display_as_haskell {
    ($($type:ty),*) => {$(
        impl fmt::Display for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                let mut out = String::new();
                self.write(&mut out, false);
                formatter.write_str(&out)
            }
        }
    )*};
}

display_as_haskell!(
    Program, Window, Input, Value, Type, Reference, Expression, Definition, Body, BodyResult
);

#[cfg(test)]
mod tests {
    use super::*;

    use Expression::{Argument, Literal, SecondArgument};
    use Reference::{Local, TopLevel};

    #[test]
    fn negative_numbers_are_parenthesized_everywhere() {
        assert_eq!(Literal(-5).to_string(), "Lit (-5)");
        assert_eq!(Value::Integer(-5).to_string(), "I (-5)");
        assert_eq!(Value::List(vec![-1, 2, -3]).to_string(), "L [(-1),2,(-3)]");
        assert_eq!(
            Expression::Literal(i64::MIN).to_string(),
            "Lit (-9223372036854775808)"
        );
        assert_eq!(
            Expression::Literal(i64::MAX).to_string(),
            "Lit 9223372036854775807"
        );
        let literal = Definition::Literal {
            event_type: Type::Integer,
            events: vec![(vec![1], Value::Integer(-2))],
        };
        assert_eq!(literal.to_string(), "SLit TInt [([1],I (-2))]");
    }

    #[test]
    fn applications_in_argument_position_are_parenthesized() {
        let expression = (Argument + Literal(1)) * SecondArgument.modulo(4);
        assert_eq!(expression.to_string(), "Mul (Add Arg (Lit 1)) (Mod Arg2 4)");
        assert_eq!(
            Expression::if_then_else(!Argument, Literal(1), Argument.less_than(Literal(0)))
                .to_string(),
            "If (Not Arg) (Lit 1) (Lt Arg (Lit 0))"
        );
        assert_eq!(
            Definition::Never(Type::stream_of(Type::cell_of(Type::Integer))).to_string(),
            "SNever (TStreamOf (TCellOf TInt))"
        );
        assert_eq!(
            Input::coalescing(Type::Integer, Argument - SecondArgument).to_string(),
            "Input TInt (Just (Sub Arg Arg2))"
        );
        assert_eq!(Input::new(Type::Boolean).to_string(), "Input TBool Nothing");
    }

    #[test]
    fn a_program_prints_as_one_line_of_haskell() {
        let program = Program {
            window: Window::FromFirstTransaction,
            inputs: vec![Input::new(Type::Integer)],
            definitions: vec![
                Definition::Input(0),
                Definition::CellLoop(Type::Integer),
                Definition::Snapshot {
                    function: SecondArgument + Literal(1),
                    source: TopLevel(0),
                    cell: TopLevel(1),
                },
                Definition::Hold {
                    initial: Literal(0),
                    source: TopLevel(2),
                },
                Definition::Close {
                    forward: 1,
                    definition: TopLevel(3),
                },
                Definition::Construct {
                    body: Body {
                        definitions: vec![Definition::Map {
                            function: Expression::ConstructEvent,
                            source: TopLevel(0),
                        }],
                        result: BodyResult::Node(Local(0)),
                    },
                    source: TopLevel(0),
                },
            ],
            observe: vec![3],
            schedule: vec![
                vec![(0, Value::Integer(1))],
                vec![],
                vec![(0, Value::Boolean(true))],
            ],
        };
        assert_eq!(
            program.to_string(),
            "Program FromFirstTransaction [Input TInt Nothing] [SInput 0,CLoop TInt,\
             SSnapshot (Add Arg2 (Lit 1)) (N 0) (N 1),CHold (Lit 0) (N 2),Close 1 (N 3),\
             SConstruct (Body [SMap CArg (N 0)] (RNode (Local 0))) (N 0)] [3] \
             [[(0,I 1)],[],[(0,B True)]]"
        );
        assert!(!program.to_string().contains('\n'));
    }
}
