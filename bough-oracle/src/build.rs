//! A [`Program`] built with the Bough API, and its schedule run on it.
//!
//! [`run`] checks that a program is in the subset stages 1 and 2 of the
//! engine implement, builds it in one build closure the way a user would
//! write it, registers listeners on the observed nodes, and runs the
//! schedule: one `graph.transaction` per external transaction, with the
//! sends in the schedule's order unless [`RunOptions`] permutes them. It
//! records what each listener saw in each transaction, and what
//! `graph.sample` gives each observed cell after it.
//!
//! # The subset
//!
//! Inputs of integers or booleans, coalescing or not. The definitions
//! `Input`, `InputCell`, `Never` and `Constant`; the adapters `Map`,
//! `Filter`, `FilterMap`, `MapTo`, `Snapshot`, `Gate` and `Once`; the stream
//! materializers `Node`, `Share`, `Merge`, `OrElse`, `Scan`, `Steps` and
//! `StepsWithCurrent`; the cells `Hold`, `Accumulate`, `AccumulateMut`,
//! `MapCell`, `ToBoolean` and `Lift`. An expression may `Sample` a cell
//! defined before it, which reads the cell in the build closure, before
//! transaction zero, as the oracle's top level does. Anything else, and
//! anything the oracle would refuse, is a [`BuildError`] naming the node,
//! from [`check`], before any node is built.
//!
//! Expressions evaluate as the protocol says ([`evaluate`]): 64-bit
//! wrapping arithmetic, `Modulo` as `rem_euclid`, a boolean read as 0 or 1,
//! and a boolean result true when nonzero.
//!
//! # Types
//!
//! Every stream carries `i64`, a boolean stream 0 and 1, so that one family
//! of chain types serves both. A cell has its own type, `i64` or `bool`,
//! since a gate reads a `Cell<bool>`, and is a `State` when it is an
//! in-place accumulator or reads through one. Where the types meet, the
//! builder converts with an identity of the semantics: a hold of a boolean
//! stream fuses `map(|x| x != 0)` into its chain; a steps view of a boolean
//! cell is materialized as `steps.map(i64::from).node(b)`; and a snapshot or
//! lift that reads a boolean cell reads `map_cell(|b| i64::from(*b))` of it.
//!
//! # Fusion
//!
//! An adapter is not a node. The builder keeps a chain of adapters and
//! hands it to the materializer that consumes it, which fuses the chain
//! into its one node, as a user would write it. A chain's type nests its
//! adapters, so an interpreter can build only boundedly many: the builder
//! holds a chain as a trait object per depth, one adapter or two, and a
//! third adapter first materializes the chain with `node`, an identity of
//! the semantics. Each depth multiplies the chain types by the nine adapter
//! kinds (a snapshot and a gate each of a `Cell` and of a `State`): three
//! adapters would make 1,640 chain types per mode instead of 182, and in a
//! prototype of this builder, `Local` alone, they took 148 s to compile in
//! debug and 367 s in release, against 13 s and 36 s for two.
//!
//! `Local` fuses [`MAX_FUSED`] adapters and `Threaded` one
//! ([`EngineMode::FUSED`]): the mode's carrier is what `Threaded` adds, and
//! at one adapter it compiles 20 chain types instead of 182. The branch that
//! fuses a second adapter is dead for `Threaded`, and rustc does not compile
//! it: the release build of the test binary takes 71 s, against 51 s for
//! `Local` alone and 111 s with both modes at two.
//!
//! # Linearity
//!
//! A `Stream` has one consumer. [`check`] counts the consumers of every
//! stream node, the observation included, and refuses a node other than a
//! `Share` with more than one. Such a program is a generator's bug, not the
//! engine's.
//!
//! # Modes
//!
//! A function generic over the mode cannot store closures of its own
//! (finding F23): the bound `M: Accepts<Map<S, {closure}>>` cannot be
//! written. So every closure here is boxed and `Send`, and every operation
//! that stores something is a method of [`EngineMode`], implemented by one
//! macro for [`Local`] and for [`Threaded`](bough::Threaded), where the
//! bounds hold for every `Send` type. The builder is generic over
//! `EngineMode`, and a caller picks the mode with `run::<Local>` or
//! `run::<Threaded>`.

use std::fmt;
use std::mem;
use std::sync::{Arc, Mutex, PoisonError};

use bough::{
    Build, Cell, CellRef, Graph, Lift, Listener, Local, Node, Shared, Source, State, Stream, Trace,
    Tracer, Transaction,
};

use crate::program::{Definition, Expression, Input, Program, Reference, Type, Value};

// ----- the closures the builder stores -----

/// `map`'s function.
pub type MapFn = Box<dyn Fn(i64) -> i64 + Send>;
/// `filter`'s predicate.
pub type Predicate = Box<dyn Fn(&i64) -> bool + Send>;
/// `filter_map`'s function.
pub type FilterMapFn = Box<dyn Fn(i64) -> Option<i64> + Send>;
/// `snapshot`'s and `accumulate`'s function: the event and a value.
pub type BinaryFn = Box<dyn Fn(i64, &i64) -> i64 + Send>;
/// `accumulate_mut`'s function.
pub type InPlaceFn = Box<dyn FnMut(i64, &mut i64) + Send>;
/// `scan`'s function: the output and the next state.
pub type ScanFn = Box<dyn Fn(i64, &i64) -> (i64, i64) + Send>;
/// `merge`'s function, the left event first.
pub type CombineFn = Box<dyn Fn(i64, i64) -> i64 + Send>;
/// A coalescing input's function, the first send on the left.
pub type FoldFn<A> = Box<dyn Fn(A, A) -> A + Send>;
/// `map_cell`'s function.
pub type CellFn<A, B> = Box<dyn Fn(&A) -> B + Send>;
/// A stream listener.
pub type StreamSink = Box<dyn FnMut(i64) + Send>;
/// A cell listener.
pub type CellSink<A> = Box<dyn FnMut(&A) + Send>;

/// A chain of integer events that `Threaded` can store: what every
/// materializer of [`EngineMode`] takes.
pub trait Chain: Source<Event = i64> + Send {}

impl<S: Source<Event = i64> + Send> Chain for S {}

// ----- the modes -----

/// The Bough operations the builder uses, at its types, for one mode.
///
/// Implemented for [`Local`] and [`Threaded`](bough::Threaded) by one macro, so both run the
/// same builder: see the module documentation.
pub trait EngineMode: bough::Mode {
    /// `Graph::build` or `Graph::build_threaded`.
    fn build<R: Trace + Send>(f: impl FnOnce(&mut Build<Self>) -> R) -> (Graph<Self>, R);
    /// `b.input()`.
    fn input(b: &mut Build<Self>) -> (Stream<i64>, bough::Input<i64>);
    /// `b.input_coalescing(f)`.
    fn input_coalescing(b: &mut Build<Self>, f: FoldFn<i64>) -> (Stream<i64>, bough::Input<i64>);
    /// `b.input_cell(initial)`.
    fn input_cell<A: Trace + Send + 'static>(
        b: &mut Build<Self>,
        initial: A,
    ) -> (Cell<A>, bough::Input<A>);
    /// `b.input_cell_coalescing(initial, f)`.
    fn input_cell_coalescing<A: Trace + Send + 'static>(
        b: &mut Build<Self>,
        initial: A,
        f: FoldFn<A>,
    ) -> (Cell<A>, bough::Input<A>);
    /// `b.constant(value)`.
    fn constant(b: &mut Build<Self>, value: i64) -> Cell<i64>;
    /// `chain.hold(b, initial)`.
    fn hold<S: Chain>(b: &mut Build<Self>, chain: S, initial: i64) -> Cell<i64>;
    /// `chain.map(|x| x != 0).hold(b, initial)`: a boolean stream's hold.
    fn hold_boolean<S: Chain>(b: &mut Build<Self>, chain: S, initial: bool) -> Cell<bool>;
    /// `chain.accumulate(b, initial, f)`.
    fn accumulate<S: Chain>(b: &mut Build<Self>, chain: S, initial: i64, f: BinaryFn) -> Cell<i64>;
    /// `chain.accumulate_mut(b, initial, f)`.
    fn accumulate_mut<S: Chain>(
        b: &mut Build<Self>,
        chain: S,
        initial: i64,
        f: InPlaceFn,
    ) -> State<i64>;
    /// `chain.scan(b, initial, f)`.
    fn scan<S: Chain>(b: &mut Build<Self>, chain: S, initial: i64, f: ScanFn) -> Stream<i64>;
    /// `chain.node(b)`.
    fn node<S: Chain>(b: &mut Build<Self>, chain: S) -> Stream<i64>;
    /// `chain.share(b)`.
    fn share<S: Chain>(b: &mut Build<Self>, chain: S) -> Shared<i64>;
    /// `left.merge(b, right, f)`.
    fn merge<S: Chain, T: Chain>(
        b: &mut Build<Self>,
        left: S,
        right: T,
        f: CombineFn,
    ) -> Stream<i64>;
    /// `left.or_else(b, right)`.
    fn or_else<S: Chain, T: Chain>(b: &mut Build<Self>, left: S, right: T) -> Stream<i64>;
    /// `cell.map_cell(b, f)`.
    fn map_cell<A: 'static, B: Send + 'static>(
        b: &mut Build<Self>,
        cell: Cell<A>,
        f: CellFn<A, B>,
    ) -> Cell<B>;
    /// `state.map_cell(b, f)`.
    fn map_state<A: 'static, B: Send + 'static>(
        b: &mut Build<Self>,
        state: State<A>,
        f: CellFn<A, B>,
    ) -> State<B>;
    /// `cells.lift(b, f)`.
    fn lift<L: Lift<F, i64>, F: Send + 'static>(b: &mut Build<Self>, cells: L, f: F) -> L::Output;
    /// `cell.steps(b)`.
    fn steps<A: Clone + Send + 'static>(b: &mut Build<Self>, cell: Cell<A>) -> Stream<A>;
    /// `cell.steps_with_current(b)`.
    fn steps_with_current<A: Clone + Send + 'static>(
        b: &mut Build<Self>,
        cell: Cell<A>,
    ) -> Stream<A>;
    /// `graph.listen(node, f)`.
    fn listen<S: Node<Event = i64>>(
        graph: &mut Graph<Self>,
        node: S,
        f: StreamSink,
    ) -> Listener<Self>;
    /// `graph.listen_cell(cell, f)`.
    fn listen_cell<C: CellRef>(
        graph: &mut Graph<Self>,
        cell: C,
        f: CellSink<C::Value>,
    ) -> Listener<Self>;
    /// `graph.listen_steps(cell, f)`.
    fn listen_steps<C: CellRef>(
        graph: &mut Graph<Self>,
        cell: C,
        f: CellSink<C::Value>,
    ) -> Listener<Self>;
    /// `tx.send(input, value)`.
    fn send<A: Send + 'static>(tx: &mut Transaction<'_, Self>, input: bough::Input<A>, value: A);

    /// How many adapters a chain fuses before a `node`: [`MAX_FUSED`] in
    /// `Local`, one in `Threaded`, which so compiles a tenth of the chain
    /// types (see the module documentation).
    const FUSED: usize;
}

/// The same bodies for every mode: in each impl the mode is concrete, and it
/// accepts every `Send` type.
macro_rules! engine_mode {
    ($mode:ty, $build:ident, $fused:expr) => {
        impl EngineMode for $mode {
            const FUSED: usize = $fused;

            fn build<R: Trace + Send>(f: impl FnOnce(&mut Build<Self>) -> R) -> (Graph<Self>, R) {
                Graph::$build(f)
            }
            fn input(b: &mut Build<Self>) -> (Stream<i64>, bough::Input<i64>) {
                b.input()
            }
            fn input_coalescing(
                b: &mut Build<Self>,
                f: FoldFn<i64>,
            ) -> (Stream<i64>, bough::Input<i64>) {
                b.input_coalescing(f)
            }
            fn input_cell<A: Trace + Send + 'static>(
                b: &mut Build<Self>,
                initial: A,
            ) -> (Cell<A>, bough::Input<A>) {
                b.input_cell(initial)
            }
            fn input_cell_coalescing<A: Trace + Send + 'static>(
                b: &mut Build<Self>,
                initial: A,
                f: FoldFn<A>,
            ) -> (Cell<A>, bough::Input<A>) {
                b.input_cell_coalescing(initial, f)
            }
            fn constant(b: &mut Build<Self>, value: i64) -> Cell<i64> {
                b.constant(value)
            }
            fn hold<S: Chain>(b: &mut Build<Self>, chain: S, initial: i64) -> Cell<i64> {
                chain.hold(b, initial)
            }
            fn hold_boolean<S: Chain>(b: &mut Build<Self>, chain: S, initial: bool) -> Cell<bool> {
                chain.map(truthy as fn(i64) -> bool).hold(b, initial)
            }
            fn accumulate<S: Chain>(
                b: &mut Build<Self>,
                chain: S,
                initial: i64,
                f: BinaryFn,
            ) -> Cell<i64> {
                chain.accumulate(b, initial, f)
            }
            fn accumulate_mut<S: Chain>(
                b: &mut Build<Self>,
                chain: S,
                initial: i64,
                f: InPlaceFn,
            ) -> State<i64> {
                chain.accumulate_mut(b, initial, f)
            }
            fn scan<S: Chain>(
                b: &mut Build<Self>,
                chain: S,
                initial: i64,
                f: ScanFn,
            ) -> Stream<i64> {
                chain.scan(b, initial, f)
            }
            fn node<S: Chain>(b: &mut Build<Self>, chain: S) -> Stream<i64> {
                chain.node(b)
            }
            fn share<S: Chain>(b: &mut Build<Self>, chain: S) -> Shared<i64> {
                chain.share(b)
            }
            fn merge<S: Chain, T: Chain>(
                b: &mut Build<Self>,
                left: S,
                right: T,
                f: CombineFn,
            ) -> Stream<i64> {
                left.merge(b, right, f)
            }
            fn or_else<S: Chain, T: Chain>(b: &mut Build<Self>, left: S, right: T) -> Stream<i64> {
                left.or_else(b, right)
            }
            fn map_cell<A: 'static, B: Send + 'static>(
                b: &mut Build<Self>,
                cell: Cell<A>,
                f: CellFn<A, B>,
            ) -> Cell<B> {
                cell.map_cell(b, f)
            }
            fn map_state<A: 'static, B: Send + 'static>(
                b: &mut Build<Self>,
                state: State<A>,
                f: CellFn<A, B>,
            ) -> State<B> {
                state.map_cell(b, f)
            }
            fn lift<L: Lift<F, i64>, F: Send + 'static>(
                b: &mut Build<Self>,
                cells: L,
                f: F,
            ) -> L::Output {
                cells.lift(b, f)
            }
            fn steps<A: Clone + Send + 'static>(b: &mut Build<Self>, cell: Cell<A>) -> Stream<A> {
                cell.steps(b)
            }
            fn steps_with_current<A: Clone + Send + 'static>(
                b: &mut Build<Self>,
                cell: Cell<A>,
            ) -> Stream<A> {
                cell.steps_with_current(b)
            }
            fn listen<S: Node<Event = i64>>(
                graph: &mut Graph<Self>,
                node: S,
                f: StreamSink,
            ) -> Listener<Self> {
                graph.listen(node, f)
            }
            fn listen_cell<C: CellRef>(
                graph: &mut Graph<Self>,
                cell: C,
                f: CellSink<C::Value>,
            ) -> Listener<Self> {
                graph.listen_cell(cell, f)
            }
            fn listen_steps<C: CellRef>(
                graph: &mut Graph<Self>,
                cell: C,
                f: CellSink<C::Value>,
            ) -> Listener<Self> {
                graph.listen_steps(cell, f)
            }
            fn send<A: Send + 'static>(
                tx: &mut Transaction<'_, Self>,
                input: bough::Input<A>,
                value: A,
            ) {
                tx.send(input, value)
            }
        }
    };
}

engine_mode!(Local, build, MAX_FUSED);
#[cfg(target_has_atomic = "ptr")]
engine_mode!(bough::Threaded, build_threaded, 1);

// ----- expressions -----

/// Evaluates an expression that [`check`] accepted and whose `Sample`s the
/// builder replaced with the values they read: 64-bit wrapping arithmetic,
/// `Modulo` as `rem_euclid` (Haskell's `mod` for a positive divisor),
/// comparisons and `Not` as 0 or 1, `If` true when nonzero.
///
/// Panics on `ConstructEvent`, on a `Sample`, and on an argument past the
/// end of `arguments`, which `check` refuses.
pub fn evaluate(expression: &Expression, arguments: &[i64]) -> i64 {
    let go = |e: &Expression| evaluate(e, arguments);
    match expression {
        Expression::Argument => arguments[0],
        Expression::SecondArgument => arguments[1],
        Expression::ArgumentAt(index) => arguments[*index],
        Expression::Literal(value) => *value,
        Expression::Add(a, b) => go(a).wrapping_add(go(b)),
        Expression::Subtract(a, b) => go(a).wrapping_sub(go(b)),
        Expression::Multiply(a, b) => go(a).wrapping_mul(go(b)),
        Expression::Modulo(a, divisor) => go(a).rem_euclid(*divisor),
        Expression::Maximum(a, b) => go(a).max(go(b)),
        Expression::Minimum(a, b) => go(a).min(go(b)),
        Expression::Equal(a, b) => i64::from(go(a) == go(b)),
        Expression::LessThan(a, b) => i64::from(go(a) < go(b)),
        Expression::Not(a) => i64::from(go(a) == 0),
        Expression::If(condition, then, otherwise) => {
            if go(condition) != 0 {
                go(then)
            } else {
                go(otherwise)
            }
        }
        Expression::ConstructEvent | Expression::Sample(_) => {
            unreachable!(
                "bough-oracle: {expression} reached evaluation; check refuses CArg, and the \
                 builder replaces every Sample with the value it reads"
            )
        }
    }
}

/// A boolean result: nonzero is true.
fn truthy(value: i64) -> bool {
    value != 0
}

// ----- checking -----

/// A scalar of the subset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scalar {
    /// `TInt`: `i64`.
    Integer,
    /// `TBool`: `bool` in a cell, 0 or 1 in a stream.
    Boolean,
}

impl Scalar {
    fn of(value_type: &Type) -> Option<Scalar> {
        match value_type {
            Type::Integer => Some(Scalar::Integer),
            Type::Boolean => Some(Scalar::Boolean),
            _ => None,
        }
    }

    fn of_value(value: &Value) -> Option<Scalar> {
        match value {
            Value::Integer(_) => Some(Scalar::Integer),
            Value::Boolean(_) => Some(Scalar::Boolean),
            Value::List(_) => None,
        }
    }
}

/// What a node of the subset makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeType {
    /// A stream.
    Stream(Scalar),
    /// A cell; `state` when it is a `State`, which has no stream view.
    Cell {
        /// What it holds.
        value: Scalar,
        /// An in-place accumulator, or a read-through cell over one.
        state: bool,
    },
}

/// A program the builder cannot build: outside the subset, or malformed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildError {
    /// The node at fault, if one is.
    pub node: Option<usize>,
    /// What is wrong.
    pub message: String,
}

impl BuildError {
    fn program(message: impl Into<String>) -> BuildError {
        BuildError {
            node: None,
            message: message.into(),
        }
    }

    fn node(index: usize, definition: &Definition, message: impl fmt::Display) -> BuildError {
        BuildError {
            node: Some(index),
            message: format!("node {index} ({}): {message}", name(definition)),
        }
    }
}

impl fmt::Display for BuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for BuildError {}

impl Trace for BuildError {
    fn trace(&self, _tracer: &mut Tracer) {}
}

/// A definition's variant name, for messages.
pub fn name(definition: &Definition) -> String {
    let text = format!("{definition:?}");
    let end = text.find([' ', '(', '{']).unwrap_or(text.len());
    text[..end].to_owned()
}

/// The top-level node a reference names, if it is one defined before
/// `index`.
fn earlier(reference: &Reference, index: usize) -> Result<usize, String> {
    match reference {
        Reference::TopLevel(node) if *node < index => Ok(*node),
        Reference::TopLevel(node) => Err(format!(
            "N {node} does not name a node defined before this one"
        )),
        Reference::Local(node) => Err(format!(
            "Local {node} names a construct body's node, and the subset has no construct"
        )),
    }
}

/// The streams a definition consumes: what linearity counts.
fn consumed_streams(definition: &Definition) -> Vec<Reference> {
    match definition {
        Definition::Map { source, .. }
        | Definition::Filter { source, .. }
        | Definition::FilterMap { source, .. }
        | Definition::MapTo { source, .. }
        | Definition::Snapshot { source, .. }
        | Definition::Gate { source, .. }
        | Definition::Scan { source, .. }
        | Definition::Hold { source, .. }
        | Definition::Accumulate { source, .. }
        | Definition::AccumulateMut { source, .. } => vec![*source],
        Definition::Once(source) | Definition::Node(source) | Definition::Share(source) => {
            vec![*source]
        }
        Definition::Merge { left, right, .. } | Definition::OrElse { left, right } => {
            vec![*left, *right]
        }
        _ => Vec::new(),
    }
}

/// Checks that a program is in the subset and well formed, and returns what
/// each node makes. Every error names the node at fault where there is one.
pub fn check(program: &Program) -> Result<Vec<NodeType>, BuildError> {
    let mut inputs = Vec::with_capacity(program.inputs.len());
    for (index, input) in program.inputs.iter().enumerate() {
        inputs.push(check_input(index, input)?);
    }
    let mut types: Vec<NodeType> = Vec::with_capacity(program.definitions.len());
    for (index, definition) in program.definitions.iter().enumerate() {
        let made = check_definition(&inputs, &types, index, definition)
            .map_err(|message| BuildError::node(index, definition, message))?;
        types.push(made);
    }
    if program.observe.is_empty() {
        return Err(BuildError::program("observe: the program observes no node"));
    }
    for &node in &program.observe {
        if node >= types.len() {
            return Err(BuildError::program(format!(
                "observe: there is no node {node}"
            )));
        }
    }
    check_linearity(program, &types)?;
    check_schedule(program, &inputs)?;
    Ok(types)
}

fn check_input(index: usize, input: &Input) -> Result<Scalar, BuildError> {
    let at = |message: String| BuildError::program(format!("input {index}: {message}"));
    let scalar = Scalar::of(&input.event_type).ok_or_else(|| {
        at(format!(
            "an input of {} is outside the subset, which has integers and booleans",
            input.event_type
        ))
    })?;
    if let Some(function) = &input.coalesce {
        check_expression(function, 2, None).map_err(at)?;
    }
    Ok(scalar)
}

/// Checks an expression taking `arguments` arguments. `cells` are the nodes
/// a `Sample` may read, `None` where it may read none.
fn check_expression(
    expression: &Expression,
    arguments: usize,
    cells: Option<&[NodeType]>,
) -> Result<(), String> {
    let go = |e: &Expression| check_expression(e, arguments, cells);
    let argument = |label: String, index: usize| {
        if index < arguments {
            Ok(())
        } else {
            Err(format!(
                "{label} is not bound: this expression takes {arguments} argument(s)"
            ))
        }
    };
    match expression {
        Expression::Argument => argument("Arg".to_owned(), 0),
        Expression::SecondArgument => argument("Arg2".to_owned(), 1),
        Expression::ArgumentAt(index) => argument(format!("ArgN {index}"), *index),
        Expression::ConstructEvent => {
            Err("CArg is used outside a construct body, and the subset has no construct".into())
        }
        Expression::Sample(reference) => {
            let Some(cells) = cells else {
                return Err("an input's coalescing function cannot sample a cell".into());
            };
            let node = earlier(reference, cells.len())?;
            match cells[node] {
                NodeType::Cell { .. } => Ok(()),
                NodeType::Stream(_) => Err(format!("Sample {reference}: node {node} is a stream")),
            }
        }
        Expression::Literal(_) => Ok(()),
        Expression::Add(a, b)
        | Expression::Subtract(a, b)
        | Expression::Multiply(a, b)
        | Expression::Maximum(a, b)
        | Expression::Minimum(a, b)
        | Expression::Equal(a, b)
        | Expression::LessThan(a, b) => {
            go(a)?;
            go(b)
        }
        Expression::Modulo(a, divisor) => {
            if *divisor <= 0 {
                return Err(format!("Mod needs a positive divisor, not {divisor}"));
            }
            go(a)
        }
        Expression::Not(a) => go(a),
        Expression::If(condition, then, otherwise) => {
            go(condition)?;
            go(then)?;
            go(otherwise)
        }
    }
}

fn check_definition(
    inputs: &[Scalar],
    types: &[NodeType],
    index: usize,
    definition: &Definition,
) -> Result<NodeType, String> {
    let expression = |e: &Expression, arguments: usize| check_expression(e, arguments, Some(types));
    let stream = |r: &Reference| -> Result<Scalar, String> {
        let node = earlier(r, index)?;
        match types[node] {
            NodeType::Stream(scalar) => Ok(scalar),
            NodeType::Cell { .. } => Err(format!("{r} is a cell, not a stream")),
        }
    };
    let cell = |r: &Reference| -> Result<(Scalar, bool), String> {
        let node = earlier(r, index)?;
        match types[node] {
            NodeType::Cell { value, state } => Ok((value, state)),
            NodeType::Stream(_) => Err(format!("{r} is a stream, not a cell")),
        }
    };
    let input = |k: usize| {
        inputs
            .get(k)
            .copied()
            .ok_or_else(|| format!("there is no input {k}"))
    };
    let made = match definition {
        Definition::Input(k) => NodeType::Stream(input(*k)?),
        Definition::InputCell { input: k, initial } => {
            let value = input(*k)?;
            expression(initial, 0)?;
            NodeType::Cell {
                value,
                state: false,
            }
        }
        Definition::Never(event_type) => NodeType::Stream(Scalar::of(event_type).ok_or_else(|| {
            format!("a stream of {event_type} is outside the subset, which has integers and booleans")
        })?),
        Definition::Constant(value) => {
            expression(value, 0)?;
            NodeType::Cell {
                value: Scalar::Integer,
                state: false,
            }
        }
        Definition::Map { function, source } => {
            stream(source)?;
            expression(function, 1)?;
            NodeType::Stream(Scalar::Integer)
        }
        Definition::Filter { predicate, source } => {
            let scalar = stream(source)?;
            expression(predicate, 1)?;
            NodeType::Stream(scalar)
        }
        Definition::FilterMap {
            keep,
            function,
            source,
        } => {
            stream(source)?;
            expression(keep, 1)?;
            expression(function, 1)?;
            NodeType::Stream(Scalar::Integer)
        }
        Definition::MapTo { value, source } => {
            stream(source)?;
            NodeType::Stream(Scalar::of_value(value).ok_or_else(|| {
                format!("{value} is outside the subset, which has integers and booleans")
            })?)
        }
        Definition::Snapshot {
            function,
            source,
            cell: read,
        } => {
            stream(source)?;
            cell(read)?;
            expression(function, 2)?;
            NodeType::Stream(Scalar::Integer)
        }
        Definition::Gate {
            source,
            cell: read,
        } => {
            let scalar = stream(source)?;
            let (value, _) = cell(read)?;
            if value != Scalar::Integer {
                NodeType::Stream(scalar)
            } else {
                return Err(format!(
                    "{read} is a cell of integers; a gate reads a cell of booleans"
                ));
            }
        }
        Definition::Once(source) | Definition::Node(source) | Definition::Share(source) => {
            NodeType::Stream(stream(source)?)
        }
        Definition::Merge {
            function,
            left,
            right,
        } => {
            let scalar = stream(left)?;
            if stream(right)? != scalar {
                return Err("a merge needs two streams of one type".into());
            }
            expression(function, 2)?;
            NodeType::Stream(scalar)
        }
        Definition::OrElse { left, right } => {
            let scalar = stream(left)?;
            if stream(right)? != scalar {
                return Err("or_else needs two streams of one type".into());
            }
            NodeType::Stream(scalar)
        }
        Definition::Scan {
            initial,
            output,
            state,
            source,
        } => {
            stream(source)?;
            expression(initial, 0)?;
            expression(output, 2)?;
            expression(state, 2)?;
            NodeType::Stream(Scalar::Integer)
        }
        Definition::Steps(read) | Definition::StepsWithCurrent(read) => {
            let (value, state) = cell(read)?;
            if state {
                return Err(format!(
                    "{read} is a State, an in-place accumulator or a cell read through one, \
                     which has no stream view"
                ));
            }
            NodeType::Stream(value)
        }
        Definition::Hold { initial, source } => {
            let value = stream(source)?;
            expression(initial, 0)?;
            NodeType::Cell {
                value,
                state: false,
            }
        }
        Definition::Accumulate {
            initial,
            function,
            source,
        }
        | Definition::AccumulateMut {
            initial,
            function,
            source,
        } => {
            stream(source)?;
            expression(initial, 0)?;
            expression(function, 2)?;
            NodeType::Cell {
                value: Scalar::Integer,
                state: matches!(definition, Definition::AccumulateMut { .. }),
            }
        }
        Definition::MapCell {
            function,
            cell: read,
        } => {
            let (_, state) = cell(read)?;
            expression(function, 1)?;
            NodeType::Cell {
                value: Scalar::Integer,
                state,
            }
        }
        Definition::ToBoolean(read) => {
            let (_, state) = cell(read)?;
            NodeType::Cell {
                value: Scalar::Boolean,
                state,
            }
        }
        Definition::Lift { function, cells } => {
            if !(2..=6).contains(&cells.len()) {
                return Err(format!("lift takes two to six cells, not {}", cells.len()));
            }
            let mut state = false;
            for read in cells {
                state |= cell(read)?.1;
            }
            expression(function, cells.len())?;
            NodeType::Cell {
                value: Scalar::Integer,
                state,
            }
        }
        Definition::Literal { .. }
        | Definition::MapList { .. }
        | Definition::PickStream { .. }
        | Definition::PickCell { .. }
        | Definition::Split(_)
        | Definition::Defer(_)
        | Definition::SwitchStream(_)
        | Definition::Construct { .. }
        | Definition::HoldStream { .. }
        | Definition::HoldCell { .. }
        | Definition::ConstantStream(_)
        | Definition::ConstantCell(_)
        | Definition::MapPickCell { .. }
        | Definition::SwitchCell(_)
        | Definition::CellLoop(_)
        | Definition::StreamLoop(_)
        | Definition::Close { .. } => {
            return Err(format!(
                "{} is outside the subset of stages 1 and 2",
                name(definition)
            ));
        }
    };
    Ok(made)
}

/// A stream node other than a `Share` has at most one consumer, the
/// observation included.
fn check_linearity(program: &Program, types: &[NodeType]) -> Result<(), BuildError> {
    let mut consumers: Vec<Vec<String>> = vec![Vec::new(); types.len()];
    for (index, definition) in program.definitions.iter().enumerate() {
        for reference in consumed_streams(definition) {
            if let Reference::TopLevel(node) = reference {
                consumers[node].push(format!("node {index}"));
            }
        }
    }
    for &node in &program.observe {
        if matches!(types[node], NodeType::Stream(_)) {
            consumers[node].push("the observation".to_owned());
        }
    }
    for (node, users) in consumers.iter().enumerate() {
        let definition = &program.definitions[node];
        if users.len() > 1 && !matches!(definition, Definition::Share(_)) {
            return Err(BuildError::node(
                node,
                definition,
                format!(
                    "a linear stream has {} consumers ({}); only a Share may have more than one",
                    users.len(),
                    users.join(", ")
                ),
            ));
        }
    }
    Ok(())
}

fn check_schedule(program: &Program, inputs: &[Scalar]) -> Result<(), BuildError> {
    for (k, sends) in program.schedule.iter().enumerate() {
        let at = |message: String| BuildError::program(format!("transaction {}: {message}", k + 1));
        let mut counts = vec![0_usize; inputs.len()];
        for (input, value) in sends {
            let Some(scalar) = inputs.get(*input) else {
                return Err(at(format!("there is no input {input}")));
            };
            if Scalar::of_value(value) != Some(*scalar) {
                return Err(at(format!(
                    "input {input} carries {}, and the send is {value}",
                    program.inputs[*input].event_type
                )));
            }
            counts[*input] += 1;
            if counts[*input] > 1 && program.inputs[*input].coalesce.is_none() {
                return Err(at(format!(
                    "input {input} is sent more than once and does not coalesce"
                )));
            }
        }
    }
    Ok(())
}

// ----- chains -----

/// An adapter waiting for the chain it extends.
enum Adapter {
    Map(MapFn),
    Filter(Predicate),
    FilterMap(FilterMapFn),
    MapTo(i64),
    SnapshotCell(Cell<i64>, BinaryFn),
    SnapshotState(State<i64>, BinaryFn),
    GateCell(Cell<bool>),
    GateState(State<bool>),
    Once,
}

/// Applies an adapter to a source, boxed as the next depth's trait object.
/// Each arm is one adapter type over the source's type.
macro_rules! adapt {
    ($source:expr, $adapter:expr, $boxed:ty) => {
        match $adapter {
            Adapter::Map(f) => Box::new($source.map(f)) as $boxed,
            Adapter::Filter(predicate) => Box::new($source.filter(predicate)) as $boxed,
            Adapter::FilterMap(f) => Box::new($source.filter_map(f)) as $boxed,
            Adapter::MapTo(value) => Box::new($source.map_to(value)) as $boxed,
            Adapter::SnapshotCell(cell, f) => Box::new($source.snapshot(cell, f)) as $boxed,
            Adapter::SnapshotState(state, f) => Box::new($source.snapshot(state, f)) as $boxed,
            Adapter::GateCell(cell) => Box::new($source.gate(cell)) as $boxed,
            Adapter::GateState(state) => Box::new($source.gate(state)) as $boxed,
            Adapter::Once => Box::new($source.once()) as $boxed,
        }
    };
}

/// The most adapters the builder fuses into one materializer, which
/// `Local` does; see the module documentation.
pub const MAX_FUSED: usize = 2;

/// A materialized stream on the other side of a merge.
enum Base {
    Stream(Stream<i64>),
    Shared(Shared<i64>),
}

/// Which side of a merge a chain is on.
#[derive(Clone, Copy)]
enum Side {
    Left,
    Right,
}

/// A merge (`Some` function) or an `or_else` (`None`).
fn join_pair<M: EngineMode, L: Chain, R: Chain>(
    b: &mut Build<M>,
    left: L,
    right: R,
    f: Option<CombineFn>,
) -> Stream<i64> {
    match f {
        Some(f) => M::merge(b, left, right, f),
        None => M::or_else(b, left, right),
    }
}

/// What a materializer does with a chain held as a trait object.
trait Materialize<M: EngineMode> {
    fn hold(self: Box<Self>, b: &mut Build<M>, initial: i64) -> Cell<i64>;
    fn hold_boolean(self: Box<Self>, b: &mut Build<M>, initial: bool) -> Cell<bool>;
    fn accumulate(self: Box<Self>, b: &mut Build<M>, initial: i64, f: BinaryFn) -> Cell<i64>;
    fn accumulate_mut(self: Box<Self>, b: &mut Build<M>, initial: i64, f: InPlaceFn) -> State<i64>;
    fn scan(self: Box<Self>, b: &mut Build<M>, initial: i64, f: ScanFn) -> Stream<i64>;
    fn node(self: Box<Self>, b: &mut Build<M>) -> Stream<i64>;
    fn share(self: Box<Self>, b: &mut Build<M>) -> Shared<i64>;
    /// A merge or an `or_else` with a materialized stream on the other side.
    fn join(
        self: Box<Self>,
        b: &mut Build<M>,
        other: Base,
        side: Side,
        f: Option<CombineFn>,
    ) -> Stream<i64>;
}

impl<M: EngineMode, S: Chain> Materialize<M> for S {
    fn hold(self: Box<Self>, b: &mut Build<M>, initial: i64) -> Cell<i64> {
        M::hold(b, *self, initial)
    }
    fn hold_boolean(self: Box<Self>, b: &mut Build<M>, initial: bool) -> Cell<bool> {
        M::hold_boolean(b, *self, initial)
    }
    fn accumulate(self: Box<Self>, b: &mut Build<M>, initial: i64, f: BinaryFn) -> Cell<i64> {
        M::accumulate(b, *self, initial, f)
    }
    fn accumulate_mut(self: Box<Self>, b: &mut Build<M>, initial: i64, f: InPlaceFn) -> State<i64> {
        M::accumulate_mut(b, *self, initial, f)
    }
    fn scan(self: Box<Self>, b: &mut Build<M>, initial: i64, f: ScanFn) -> Stream<i64> {
        M::scan(b, *self, initial, f)
    }
    fn node(self: Box<Self>, b: &mut Build<M>) -> Stream<i64> {
        M::node(b, *self)
    }
    fn share(self: Box<Self>, b: &mut Build<M>) -> Shared<i64> {
        M::share(b, *self)
    }
    fn join(
        self: Box<Self>,
        b: &mut Build<M>,
        other: Base,
        side: Side,
        f: Option<CombineFn>,
    ) -> Stream<i64> {
        match (side, other) {
            (Side::Left, Base::Stream(other)) => join_pair(b, *self, other, f),
            (Side::Left, Base::Shared(other)) => join_pair(b, *self, other, f),
            (Side::Right, Base::Stream(other)) => join_pair(b, other, *self, f),
            (Side::Right, Base::Shared(other)) => join_pair(b, other, *self, f),
        }
    }
}

/// A chain of one adapter, which a second extends, or, in a mode that
/// fuses one, a `node` of it.
trait OneAdapter<M: EngineMode>: Materialize<M> {
    fn adapt(self: Box<Self>, b: &mut Build<M>, adapter: Adapter) -> Chained<M>;
}

/// A chain of two adapters: a third needs a node first.
trait TwoAdapters<M: EngineMode>: Materialize<M> {}

impl<M: EngineMode, S: Chain> OneAdapter<M> for S {
    fn adapt(self: Box<Self>, b: &mut Build<M>, adapter: Adapter) -> Chained<M> {
        if M::FUSED >= 2 {
            Chained::Two(adapt!((*self), adapter, Box<dyn TwoAdapters<M>>))
        } else {
            let s = M::node(b, *self);
            Chained::One(adapt!(s, adapter, Box<dyn OneAdapter<M>>))
        }
    }
}

impl<M: EngineMode, S: Chain> TwoAdapters<M> for S {}

/// A stream of the program as the builder holds it: a materialized node, or
/// a chain of adapters over one, waiting for the materializer that fuses it.
enum Chained<M: EngineMode> {
    Stream(Stream<i64>),
    Shared(Shared<i64>),
    One(Box<dyn OneAdapter<M>>),
    Two(Box<dyn TwoAdapters<M>>),
}

/// Calls a materializer on whichever form a chain has.
macro_rules! materialize {
    ($chain:expr, $mode:ident, $b:expr, $method:ident($($argument:expr),*)) => {
        match $chain {
            Chained::Stream(s) => $mode::$method($b, s $(, $argument)*),
            Chained::Shared(s) => $mode::$method($b, s $(, $argument)*),
            Chained::One(chain) => chain.$method($b $(, $argument)*),
            Chained::Two(chain) => chain.$method($b $(, $argument)*),
        }
    };
}

impl<M: EngineMode> Chained<M> {
    /// Extends the chain by one adapter, materializing it with `node` first
    /// when it already has as many as the mode fuses.
    fn adapt(self, b: &mut Build<M>, adapter: Adapter) -> Chained<M> {
        match self {
            Chained::Stream(s) => Chained::One(adapt!(s, adapter, Box<dyn OneAdapter<M>>)),
            Chained::Shared(s) => Chained::One(adapt!(s, adapter, Box<dyn OneAdapter<M>>)),
            Chained::One(chain) => chain.adapt(b, adapter),
            Chained::Two(chain) => {
                let s = chain.node(b);
                Chained::One(adapt!(s, adapter, Box<dyn OneAdapter<M>>))
            }
        }
    }

    fn hold(self, b: &mut Build<M>, initial: i64) -> Cell<i64> {
        materialize!(self, M, b, hold(initial))
    }

    fn hold_boolean(self, b: &mut Build<M>, initial: bool) -> Cell<bool> {
        materialize!(self, M, b, hold_boolean(initial))
    }

    fn accumulate(self, b: &mut Build<M>, initial: i64, f: BinaryFn) -> Cell<i64> {
        materialize!(self, M, b, accumulate(initial, f))
    }

    fn accumulate_mut(self, b: &mut Build<M>, initial: i64, f: InPlaceFn) -> State<i64> {
        materialize!(self, M, b, accumulate_mut(initial, f))
    }

    fn scan(self, b: &mut Build<M>, initial: i64, f: ScanFn) -> Stream<i64> {
        materialize!(self, M, b, scan(initial, f))
    }

    fn node(self, b: &mut Build<M>) -> Stream<i64> {
        materialize!(self, M, b, node())
    }

    fn share(self, b: &mut Build<M>) -> Shared<i64> {
        materialize!(self, M, b, share())
    }

    /// The materialized stream itself, or a node for a chain.
    fn materialized(self, b: &mut Build<M>) -> Base {
        match self {
            Chained::Stream(s) => Base::Stream(s),
            Chained::Shared(s) => Base::Shared(s),
            chain => Base::Stream(chain.node(b)),
        }
    }

    /// A merge or an `or_else`, fusing whichever side is a chain. When both
    /// are, the right one becomes a node first, since a node type over two
    /// chains would multiply the chain types.
    fn join(b: &mut Build<M>, left: Self, right: Self, f: Option<CombineFn>) -> Stream<i64> {
        match (left, right) {
            (Chained::One(left), right) => {
                let right = right.materialized(b);
                left.join(b, right, Side::Left, f)
            }
            (Chained::Two(left), right) => {
                let right = right.materialized(b);
                left.join(b, right, Side::Left, f)
            }
            (left, Chained::One(right)) => {
                let left = left.materialized(b);
                right.join(b, left, Side::Right, f)
            }
            (left, Chained::Two(right)) => {
                let left = left.materialized(b);
                right.join(b, left, Side::Right, f)
            }
            (left, right) => match (left.materialized(b), right.materialized(b)) {
                (Base::Stream(l), Base::Stream(r)) => join_pair(b, l, r, f),
                (Base::Stream(l), Base::Shared(r)) => join_pair(b, l, r, f),
                (Base::Shared(l), Base::Stream(r)) => join_pair(b, l, r, f),
                (Base::Shared(l), Base::Shared(r)) => join_pair(b, l, r, f),
            },
        }
    }
}

// ----- cells -----

/// A cell of the program: its token, as its type and kind say.
#[derive(Clone, Copy)]
enum CellToken {
    Integer(Cell<i64>),
    IntegerState(State<i64>),
    Boolean(Cell<bool>),
    BooleanState(State<bool>),
}

/// A cell of integers, a `Cell` or a `State`: what a snapshot and a lift
/// read.
#[derive(Clone, Copy)]
enum IntegerCell {
    Cell(Cell<i64>),
    State(State<i64>),
}

impl From<Cell<i64>> for IntegerCell {
    fn from(cell: Cell<i64>) -> IntegerCell {
        IntegerCell::Cell(cell)
    }
}

impl From<State<i64>> for IntegerCell {
    fn from(state: State<i64>) -> IntegerCell {
        IntegerCell::State(state)
    }
}

impl IntegerCell {
    fn token(self) -> CellToken {
        match self {
            IntegerCell::Cell(cell) => CellToken::Integer(cell),
            IntegerCell::State(state) => CellToken::IntegerState(state),
        }
    }
}

fn boolean_to_integer(value: bool) -> i64 {
    i64::from(value)
}

impl CellToken {
    /// The cell read as integers: itself, or `map_cell(i64::from)` of a
    /// boolean cell.
    fn integers<M: EngineMode>(self, b: &mut Build<M>) -> IntegerCell {
        match self {
            CellToken::Integer(cell) => IntegerCell::Cell(cell),
            CellToken::IntegerState(state) => IntegerCell::State(state),
            CellToken::Boolean(cell) => {
                IntegerCell::Cell(M::map_cell(b, cell, Box::new(|v: &bool| i64::from(*v))))
            }
            CellToken::BooleanState(state) => {
                IntegerCell::State(M::map_state(b, state, Box::new(|v: &bool| i64::from(*v))))
            }
        }
    }

    /// Its value before transaction zero, read in the build closure.
    fn sample<M: EngineMode>(self, b: &Build<M>) -> i64 {
        match self {
            CellToken::Integer(cell) => *cell.sample(b),
            CellToken::IntegerState(state) => *state.sample(b),
            CellToken::Boolean(cell) => i64::from(*cell.sample(b)),
            CellToken::BooleanState(state) => i64::from(*state.sample(b)),
        }
    }

    /// Its value now, from I/O code.
    fn sample_graph<M: EngineMode>(self, graph: &Graph<M>) -> i64 {
        match self {
            CellToken::Integer(cell) => *graph.sample(cell),
            CellToken::IntegerState(state) => *graph.sample(state),
            CellToken::Boolean(cell) => i64::from(*graph.sample(cell)),
            CellToken::BooleanState(state) => i64::from(*graph.sample(state)),
        }
    }

    fn visit(self, tracer: &mut Tracer) {
        match self {
            CellToken::Integer(cell) => tracer.visit(&cell),
            CellToken::IntegerState(state) => tracer.visit(&state),
            CellToken::Boolean(cell) => tracer.visit(&cell),
            CellToken::BooleanState(state) => tracer.visit(&state),
        }
    }
}

/// Lifts two to six cells of integers: one arm per tuple of `Cell` and
/// `State`, since each tuple is its own type.
macro_rules! lift_cells {
    ($mode:ident, $b:ident, $f:ident; [$($bound:ident)*]) => {
        IntegerCell::from($mode::lift($b, ($($bound,)*), $f))
    };
    ($mode:ident, $b:ident, $f:ident; [$($bound:ident)*] $head:ident $($rest:ident)*) => {
        match $head {
            IntegerCell::Cell($head) => lift_cells!($mode, $b, $f; [$($bound)* $head] $($rest)*),
            IntegerCell::State($head) => lift_cells!($mode, $b, $f; [$($bound)* $head] $($rest)*),
        }
    };
}

/// `lift`'s function at each arity.
type Lift2 = Box<dyn Fn(&i64, &i64) -> i64 + Send>;
type Lift3 = Box<dyn Fn(&i64, &i64, &i64) -> i64 + Send>;
type Lift4 = Box<dyn Fn(&i64, &i64, &i64, &i64) -> i64 + Send>;
type Lift5 = Box<dyn Fn(&i64, &i64, &i64, &i64, &i64) -> i64 + Send>;
type Lift6 = Box<dyn Fn(&i64, &i64, &i64, &i64, &i64, &i64) -> i64 + Send>;

/// `lift` over the cells, with the function of their values.
fn lift<M: EngineMode>(b: &mut Build<M>, cells: &[IntegerCell], e: Expression) -> IntegerCell {
    match *cells {
        [c0, c1] => {
            let f: Lift2 = Box::new(move |a: &i64, c: &i64| evaluate(&e, &[*a, *c]));
            lift_cells!(M, b, f; [] c0 c1)
        }
        [c0, c1, c2] => {
            let f: Lift3 = Box::new(move |a: &i64, c: &i64, d: &i64| evaluate(&e, &[*a, *c, *d]));
            lift_cells!(M, b, f; [] c0 c1 c2)
        }
        [c0, c1, c2, c3] => {
            let f: Lift4 =
                Box::new(move |a: &i64, c: &i64, d: &i64, g: &i64| evaluate(&e, &[*a, *c, *d, *g]));
            lift_cells!(M, b, f; [] c0 c1 c2 c3)
        }
        [c0, c1, c2, c3, c4] => {
            let f: Lift5 = Box::new(move |a: &i64, c: &i64, d: &i64, g: &i64, h: &i64| {
                evaluate(&e, &[*a, *c, *d, *g, *h])
            });
            lift_cells!(M, b, f; [] c0 c1 c2 c3 c4)
        }
        [c0, c1, c2, c3, c4, c5] => {
            let f: Lift6 = Box::new(
                move |a: &i64, c: &i64, d: &i64, g: &i64, h: &i64, k: &i64| {
                    evaluate(&e, &[*a, *c, *d, *g, *h, *k])
                },
            );
            lift_cells!(M, b, f; [] c0 c1 c2 c3 c4 c5)
        }
        _ => unreachable!(
            "bough-oracle: check refuses a lift of {} cells",
            cells.len()
        ),
    }
}

// ----- building -----

/// A node of the program while the build closure runs.
enum Built<M: EngineMode> {
    Stream(Chained<M>),
    /// A linear stream, moved into its one consumer.
    Consumed,
    Cell(CellToken),
}

/// An engine input a program input drives. A program input has one for
/// each `Input` and `InputCell` over it; each send goes to all of them.
#[derive(Clone, Copy)]
enum EngineInput {
    Integer(bough::Input<i64>),
    Boolean(bough::Input<bool>),
}

/// An observed node, as I/O code listens to it.
enum Observed {
    Stream(Stream<i64>),
    Shared(Shared<i64>),
    Cell(CellToken),
}

/// What the build closure returns: the edge of the graph.
struct Edge {
    inputs: Vec<Vec<EngineInput>>,
    observed: Vec<Observed>,
}

impl Trace for Edge {
    fn trace(&self, tracer: &mut Tracer) {
        for input in self.inputs.iter().flatten() {
            match input {
                EngineInput::Integer(input) => tracer.visit(input),
                EngineInput::Boolean(input) => tracer.visit(input),
            }
        }
        for observed in &self.observed {
            match observed {
                Observed::Stream(stream) => tracer.visit(stream),
                Observed::Shared(shared) => tracer.visit(shared),
                Observed::Cell(cell) => cell.visit(tracer),
            }
        }
    }
}

/// The builder's state in the build closure.
struct Builder<'p, M: EngineMode> {
    program: &'p Program,
    types: &'p [NodeType],
    nodes: Vec<Built<M>>,
    inputs: Vec<Vec<EngineInput>>,
}

impl<M: EngineMode> Builder<'_, M> {
    /// A stream a definition consumes: moved out if linear, copied if shared.
    fn take(&mut self, reference: &Reference) -> Chained<M> {
        let node = top(reference);
        match &self.nodes[node] {
            Built::Stream(Chained::Shared(shared)) => Chained::Shared(*shared),
            Built::Stream(_) => match mem::replace(&mut self.nodes[node], Built::Consumed) {
                Built::Stream(chain) => chain,
                _ => unreachable!("bough-oracle: node {node} was a stream"),
            },
            Built::Consumed => {
                unreachable!("bough-oracle: check refuses a second consumer of node {node}")
            }
            Built::Cell(_) => unreachable!("bough-oracle: check refuses a cell as a stream"),
        }
    }

    fn cell(&self, reference: &Reference) -> CellToken {
        match &self.nodes[top(reference)] {
            Built::Cell(cell) => *cell,
            _ => unreachable!("bough-oracle: check refuses a stream as a cell"),
        }
    }

    fn scalar(&self, reference: &Reference) -> Scalar {
        match self.types[top(reference)] {
            NodeType::Stream(scalar) | NodeType::Cell { value: scalar, .. } => scalar,
        }
    }

    /// The expression with every `Sample` replaced by the value it reads:
    /// the cell's value before transaction zero, read in the build closure.
    fn resolve(&self, b: &Build<M>, expression: &Expression) -> Expression {
        let go = |e: &Expression| Box::new(self.resolve(b, e));
        match expression {
            Expression::Sample(reference) => Expression::Literal(self.cell(reference).sample(b)),
            Expression::Add(x, y) => Expression::Add(go(x), go(y)),
            Expression::Subtract(x, y) => Expression::Subtract(go(x), go(y)),
            Expression::Multiply(x, y) => Expression::Multiply(go(x), go(y)),
            Expression::Modulo(x, divisor) => Expression::Modulo(go(x), *divisor),
            Expression::Maximum(x, y) => Expression::Maximum(go(x), go(y)),
            Expression::Minimum(x, y) => Expression::Minimum(go(x), go(y)),
            Expression::Equal(x, y) => Expression::Equal(go(x), go(y)),
            Expression::LessThan(x, y) => Expression::LessThan(go(x), go(y)),
            Expression::Not(x) => Expression::Not(go(x)),
            Expression::If(c, x, y) => Expression::If(go(c), go(x), go(y)),
            leaf => leaf.clone(),
        }
    }

    /// An expression of no arguments, evaluated in the build closure.
    fn value(&self, b: &Build<M>, expression: &Expression) -> i64 {
        evaluate(&self.resolve(b, expression), &[])
    }

    fn adapt(&mut self, b: &mut Build<M>, source: &Reference, adapter: Adapter) -> Built<M> {
        let chain = self.take(source);
        Built::Stream(chain.adapt(b, adapter))
    }

    fn define(&mut self, b: &mut Build<M>, definition: &Definition) -> Built<M> {
        match definition {
            Definition::Input(k) => {
                let input = &self.program.inputs[*k];
                let (stream, token) = match &input.coalesce {
                    None => M::input(b),
                    Some(e) => {
                        let e = e.clone();
                        let fold: FoldFn<i64> = match Scalar::of(&input.event_type) {
                            Some(Scalar::Boolean) => {
                                Box::new(move |x, y| i64::from(truthy(evaluate(&e, &[x, y]))))
                            }
                            _ => Box::new(move |x, y| evaluate(&e, &[x, y])),
                        };
                        M::input_coalescing(b, fold)
                    }
                };
                self.inputs[*k].push(EngineInput::Integer(token));
                Built::Stream(Chained::Stream(stream))
            }
            Definition::InputCell { input: k, initial } => {
                let input = &self.program.inputs[*k];
                let initial = self.value(b, initial);
                let coalesce = input.coalesce.clone();
                if Scalar::of(&input.event_type) == Some(Scalar::Boolean) {
                    let (cell, token) = match coalesce {
                        None => M::input_cell(b, truthy(initial)),
                        Some(e) => M::input_cell_coalescing(
                            b,
                            truthy(initial),
                            Box::new(move |x: bool, y: bool| {
                                truthy(evaluate(&e, &[i64::from(x), i64::from(y)]))
                            }),
                        ),
                    };
                    self.inputs[*k].push(EngineInput::Boolean(token));
                    Built::Cell(CellToken::Boolean(cell))
                } else {
                    let (cell, token) = match coalesce {
                        None => M::input_cell(b, initial),
                        Some(e) => M::input_cell_coalescing(
                            b,
                            initial,
                            Box::new(move |x: i64, y: i64| evaluate(&e, &[x, y])),
                        ),
                    };
                    self.inputs[*k].push(EngineInput::Integer(token));
                    Built::Cell(CellToken::Integer(cell))
                }
            }
            Definition::Never(_) => Built::Stream(Chained::Stream(b.never())),
            Definition::Constant(value) => {
                let value = self.value(b, value);
                Built::Cell(CellToken::Integer(M::constant(b, value)))
            }
            Definition::Map { function, source } => {
                let e = self.resolve(b, function);
                self.adapt(
                    b,
                    source,
                    Adapter::Map(Box::new(move |x| evaluate(&e, &[x]))),
                )
            }
            Definition::Filter { predicate, source } => {
                let e = self.resolve(b, predicate);
                let predicate: Predicate = Box::new(move |x: &i64| truthy(evaluate(&e, &[*x])));
                self.adapt(b, source, Adapter::Filter(predicate))
            }
            Definition::FilterMap {
                keep,
                function,
                source,
            } => {
                let keep = self.resolve(b, keep);
                let function = self.resolve(b, function);
                let f: FilterMapFn = Box::new(move |x| {
                    truthy(evaluate(&keep, &[x])).then(|| evaluate(&function, &[x]))
                });
                self.adapt(b, source, Adapter::FilterMap(f))
            }
            Definition::MapTo { value, source } => {
                let value = match value {
                    Value::Integer(value) => *value,
                    Value::Boolean(value) => i64::from(*value),
                    Value::List(_) => unreachable!("bough-oracle: check refuses a list"),
                };
                self.adapt(b, source, Adapter::MapTo(value))
            }
            Definition::Snapshot {
                function,
                source,
                cell,
            } => {
                let e = self.resolve(b, function);
                let f: BinaryFn = Box::new(move |x, y: &i64| evaluate(&e, &[x, *y]));
                let adapter = match self.cell(cell).integers(b) {
                    IntegerCell::Cell(cell) => Adapter::SnapshotCell(cell, f),
                    IntegerCell::State(state) => Adapter::SnapshotState(state, f),
                };
                self.adapt(b, source, adapter)
            }
            Definition::Gate { source, cell } => {
                let adapter = match self.cell(cell) {
                    CellToken::Boolean(cell) => Adapter::GateCell(cell),
                    CellToken::BooleanState(state) => Adapter::GateState(state),
                    _ => unreachable!("bough-oracle: check refuses a gate on integers"),
                };
                self.adapt(b, source, adapter)
            }
            Definition::Once(source) => self.adapt(b, source, Adapter::Once),
            Definition::Node(source) => {
                let chain = self.take(source);
                Built::Stream(Chained::Stream(chain.node(b)))
            }
            Definition::Share(source) => {
                let chain = self.take(source);
                Built::Stream(Chained::Shared(chain.share(b)))
            }
            Definition::Merge {
                function,
                left,
                right,
            } => {
                let e = self.resolve(b, function);
                let f: CombineFn = match self.scalar(left) {
                    Scalar::Integer => Box::new(move |x, y| evaluate(&e, &[x, y])),
                    Scalar::Boolean => {
                        Box::new(move |x, y| i64::from(truthy(evaluate(&e, &[x, y]))))
                    }
                };
                let (left, right) = (self.take(left), self.take(right));
                Built::Stream(Chained::Stream(Chained::join(b, left, right, Some(f))))
            }
            Definition::OrElse { left, right } => {
                let (left, right) = (self.take(left), self.take(right));
                Built::Stream(Chained::Stream(Chained::join(b, left, right, None)))
            }
            Definition::Scan {
                initial,
                output,
                state,
                source,
            } => {
                let initial = self.value(b, initial);
                let output = self.resolve(b, output);
                let state = self.resolve(b, state);
                let f: ScanFn = Box::new(move |x, s: &i64| {
                    (evaluate(&output, &[x, *s]), evaluate(&state, &[x, *s]))
                });
                let chain = self.take(source);
                Built::Stream(Chained::Stream(chain.scan(b, initial, f)))
            }
            Definition::Steps(cell) | Definition::StepsWithCurrent(cell) => {
                let current = matches!(definition, Definition::StepsWithCurrent(_));
                let stream = match self.cell(cell) {
                    CellToken::Integer(cell) if current => M::steps_with_current(b, cell),
                    CellToken::Integer(cell) => M::steps(b, cell),
                    CellToken::Boolean(cell) => {
                        let steps = if current {
                            M::steps_with_current(b, cell)
                        } else {
                            M::steps(b, cell)
                        };
                        M::node(b, steps.map(boolean_to_integer as fn(bool) -> i64))
                    }
                    _ => unreachable!("bough-oracle: check refuses a steps view of a State"),
                };
                Built::Stream(Chained::Stream(stream))
            }
            Definition::Hold { initial, source } => {
                let initial = self.value(b, initial);
                let scalar = self.scalar(source);
                let chain = self.take(source);
                Built::Cell(match scalar {
                    Scalar::Integer => CellToken::Integer(chain.hold(b, initial)),
                    Scalar::Boolean => CellToken::Boolean(chain.hold_boolean(b, truthy(initial))),
                })
            }
            Definition::Accumulate {
                initial,
                function,
                source,
            } => {
                let initial = self.value(b, initial);
                let e = self.resolve(b, function);
                let f: BinaryFn = Box::new(move |x, s: &i64| evaluate(&e, &[x, *s]));
                let chain = self.take(source);
                Built::Cell(CellToken::Integer(chain.accumulate(b, initial, f)))
            }
            Definition::AccumulateMut {
                initial,
                function,
                source,
            } => {
                let initial = self.value(b, initial);
                let e = self.resolve(b, function);
                let f: InPlaceFn = Box::new(move |x, s: &mut i64| *s = evaluate(&e, &[x, *s]));
                let chain = self.take(source);
                Built::Cell(CellToken::IntegerState(chain.accumulate_mut(b, initial, f)))
            }
            Definition::MapCell { function, cell } => {
                let e = self.resolve(b, function);
                Built::Cell(match self.cell(cell) {
                    CellToken::Integer(cell) => CellToken::Integer(M::map_cell(
                        b,
                        cell,
                        Box::new(move |v: &i64| evaluate(&e, &[*v])),
                    )),
                    CellToken::IntegerState(state) => CellToken::IntegerState(M::map_state(
                        b,
                        state,
                        Box::new(move |v: &i64| evaluate(&e, &[*v])),
                    )),
                    CellToken::Boolean(cell) => CellToken::Integer(M::map_cell(
                        b,
                        cell,
                        Box::new(move |v: &bool| evaluate(&e, &[i64::from(*v)])),
                    )),
                    CellToken::BooleanState(state) => CellToken::IntegerState(M::map_state(
                        b,
                        state,
                        Box::new(move |v: &bool| evaluate(&e, &[i64::from(*v)])),
                    )),
                })
            }
            Definition::ToBoolean(cell) => Built::Cell(match self.cell(cell) {
                CellToken::Integer(cell) => {
                    CellToken::Boolean(M::map_cell(b, cell, Box::new(|v: &i64| truthy(*v))))
                }
                CellToken::IntegerState(state) => {
                    CellToken::BooleanState(M::map_state(b, state, Box::new(|v: &i64| truthy(*v))))
                }
                CellToken::Boolean(cell) => {
                    CellToken::Boolean(M::map_cell(b, cell, Box::new(|v: &bool| *v)))
                }
                CellToken::BooleanState(state) => {
                    CellToken::BooleanState(M::map_state(b, state, Box::new(|v: &bool| *v)))
                }
            }),
            Definition::Lift { function, cells } => {
                let e = self.resolve(b, function);
                let tokens: Vec<CellToken> = cells.iter().map(|cell| self.cell(cell)).collect();
                let cells: Vec<IntegerCell> =
                    tokens.into_iter().map(|cell| cell.integers(b)).collect();
                Built::Cell(lift(b, &cells, e).token())
            }
            _ => unreachable!(
                "bough-oracle: check refuses {}, outside the subset",
                name(definition)
            ),
        }
    }

    /// An observed node as I/O code listens to it; a chain gets a node.
    fn observe(&mut self, b: &mut Build<M>, node: usize) -> Observed {
        match &self.nodes[node] {
            Built::Cell(cell) => Observed::Cell(*cell),
            Built::Stream(Chained::Shared(shared)) => Observed::Shared(*shared),
            Built::Stream(_) => match self.take(&Reference::TopLevel(node)).materialized(b) {
                Base::Stream(stream) => Observed::Stream(stream),
                Base::Shared(shared) => Observed::Shared(shared),
            },
            Built::Consumed => {
                unreachable!("bough-oracle: check refuses observing a consumed stream")
            }
        }
    }
}

/// The node a checked top-level reference names.
fn top(reference: &Reference) -> usize {
    match reference {
        Reference::TopLevel(node) => *node,
        Reference::Local(_) => unreachable!("bough-oracle: check refuses Local"),
    }
}

/// The build closure's body.
fn build_program<M: EngineMode>(b: &mut Build<M>, program: &Program, types: &[NodeType]) -> Edge {
    let mut builder = Builder::<M> {
        program,
        types,
        nodes: Vec::with_capacity(program.definitions.len()),
        inputs: program.inputs.iter().map(|_| Vec::new()).collect(),
    };
    for definition in &program.definitions {
        let built = builder.define(b, definition);
        builder.nodes.push(built);
    }
    let observed = program
        .observe
        .iter()
        .map(|&node| builder.observe(b, node))
        .collect();
    Edge {
        inputs: builder.inputs,
        observed,
    }
}

// ----- running -----

/// How to drive one run of a program.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunOptions {
    /// `Graph::set_shuffle_seed`, set before the first transaction: `None`
    /// runs the plain order.
    pub shuffle_seed: Option<u64>,
    /// Permutes the sends of each transaction with this seed: the sends to
    /// different engine inputs interleave at random, and each input's own
    /// sends keep their order, so a coalescing input folds the same values
    /// in the same order. `None` sends in the schedule's order.
    pub permute_sends: Option<u64>,
}

impl fmt::Display for RunOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.shuffle_seed {
            None => formatter.write_str("plain order")?,
            Some(seed) => write!(formatter, "shuffle seed {seed}")?,
        }
        match self.permute_sends {
            None => formatter.write_str(", sends as scheduled"),
            Some(seed) => write!(formatter, ", sends permuted with seed {seed}"),
        }
    }
}

/// What the engine showed for one observed node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineObservation {
    /// A stream's listener: the events of transaction k at index k - 1.
    Stream {
        /// The events, one list per transaction.
        events: Vec<Vec<i64>>,
    },
    /// A cell's listeners and samples.
    Cell {
        /// `listen_cell`'s calls at registration: one, the value after
        /// transaction zero.
        registration: Vec<i64>,
        /// `listen_steps`'s calls at registration: none.
        steps_registration: Vec<i64>,
        /// `listen_cell`'s calls in transaction k, at index k - 1.
        values: Vec<Vec<i64>>,
        /// `listen_steps`'s calls in transaction k, at index k - 1.
        steps: Vec<Vec<i64>>,
        /// `graph.sample` after transaction k, at index k - 1.
        samples: Vec<i64>,
    },
}

/// Everything one run showed, one observation per observed node, in the
/// order the program lists them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineRun {
    /// The observations.
    pub observations: Vec<EngineObservation>,
    /// `graph.live_nodes()` after the build: how many nodes the chains
    /// fused into.
    pub live_nodes: usize,
}

/// A listener's log, shared with its closure.
type Log = Arc<Mutex<Vec<i64>>>;

fn drain(log: &Log) -> Vec<i64> {
    mem::take(&mut *log.lock().unwrap_or_else(PoisonError::into_inner))
}

fn stream_sink(log: &Log) -> StreamSink {
    let log = log.clone();
    Box::new(move |value| {
        log.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(value)
    })
}

fn integer_sink(log: &Log) -> CellSink<i64> {
    let log = log.clone();
    Box::new(move |value: &i64| {
        log.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(*value)
    })
}

fn boolean_sink(log: &Log) -> CellSink<bool> {
    let log = log.clone();
    Box::new(move |value: &bool| {
        log.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(i64::from(*value))
    })
}

/// The listeners of one observed node, and what they saw so far.
enum Recorder {
    Stream {
        log: Log,
        events: Vec<Vec<i64>>,
    },
    Cell {
        cell: CellToken,
        values_log: Log,
        steps_log: Log,
        observation: EngineObservation,
    },
}

impl Recorder {
    fn attach<M: EngineMode>(
        graph: &mut Graph<M>,
        observed: Observed,
        listeners: &mut Vec<Listener<M>>,
    ) -> Recorder {
        let log = Log::default();
        match observed {
            Observed::Stream(stream) => {
                listeners.push(M::listen(graph, stream, stream_sink(&log)));
                Recorder::Stream {
                    log,
                    events: Vec::new(),
                }
            }
            Observed::Shared(shared) => {
                listeners.push(M::listen(graph, shared, stream_sink(&log)));
                Recorder::Stream {
                    log,
                    events: Vec::new(),
                }
            }
            Observed::Cell(cell) => {
                let steps_log = Log::default();
                match cell {
                    CellToken::Integer(c) => {
                        listeners.push(M::listen_cell(graph, c, integer_sink(&log)));
                        listeners.push(M::listen_steps(graph, c, integer_sink(&steps_log)));
                    }
                    CellToken::IntegerState(c) => {
                        listeners.push(M::listen_cell(graph, c, integer_sink(&log)));
                        listeners.push(M::listen_steps(graph, c, integer_sink(&steps_log)));
                    }
                    CellToken::Boolean(c) => {
                        listeners.push(M::listen_cell(graph, c, boolean_sink(&log)));
                        listeners.push(M::listen_steps(graph, c, boolean_sink(&steps_log)));
                    }
                    CellToken::BooleanState(c) => {
                        listeners.push(M::listen_cell(graph, c, boolean_sink(&log)));
                        listeners.push(M::listen_steps(graph, c, boolean_sink(&steps_log)));
                    }
                }
                let observation = EngineObservation::Cell {
                    registration: drain(&log),
                    steps_registration: drain(&steps_log),
                    values: Vec::new(),
                    steps: Vec::new(),
                    samples: Vec::new(),
                };
                Recorder::Cell {
                    cell,
                    values_log: log,
                    steps_log,
                    observation,
                }
            }
        }
    }

    /// Files what the listeners saw in the transaction that just ended, and
    /// samples a cell.
    fn end_transaction<M: EngineMode>(&mut self, graph: &Graph<M>) {
        match self {
            Recorder::Stream { log, events } => events.push(drain(log)),
            Recorder::Cell {
                cell,
                values_log,
                steps_log,
                observation:
                    EngineObservation::Cell {
                        values,
                        steps,
                        samples,
                        ..
                    },
            } => {
                values.push(drain(values_log));
                steps.push(drain(steps_log));
                samples.push(cell.sample_graph(graph));
            }
            Recorder::Cell { .. } => unreachable!("bough-oracle: a cell records a cell"),
        }
    }

    fn finish(self) -> EngineObservation {
        match self {
            Recorder::Stream { events, .. } => EngineObservation::Stream { events },
            Recorder::Cell { observation, .. } => observation,
        }
    }
}

/// A small splitmix64, for permuting sends.
struct Mix(u64);

impl Mix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

/// The engine's sends of one transaction: each program send goes to every
/// engine input of its program input, in the order they were built. With
/// a seed, the sends to different engine inputs interleave at random, each
/// engine input's own sends in their order.
fn engine_sends(
    sends: &[(usize, Value)],
    inputs: &[Vec<EngineInput>],
    permute: Option<(u64, usize)>,
) -> Vec<(EngineInput, i64)> {
    // One queue per engine input, in first-send order.
    let mut queues: Vec<(usize, usize, Vec<i64>)> = Vec::new();
    for (input, value) in sends {
        let value = match value {
            Value::Integer(value) => *value,
            Value::Boolean(value) => i64::from(*value),
            Value::List(_) => unreachable!("bough-oracle: check refuses a list send"),
        };
        for which in 0..inputs[*input].len() {
            match queues
                .iter_mut()
                .find(|(i, w, _)| (*i, *w) == (*input, which))
            {
                Some((_, _, queue)) => queue.push(value),
                None => queues.push((*input, which, vec![value])),
            }
        }
    }
    let mut order = Vec::new();
    match permute {
        None => {
            // The schedule's order: program sends in order, each to its
            // engine inputs in the order they were built.
            for (input, value) in sends {
                for engine in &inputs[*input] {
                    let value = match value {
                        Value::Integer(value) => *value,
                        Value::Boolean(value) => i64::from(*value),
                        Value::List(_) => unreachable!(),
                    };
                    order.push((*engine, value));
                }
            }
        }
        Some((seed, k)) => {
            let mut mix = Mix(seed ^ (k as u64).wrapping_mul(0xD1B5_4A32_D192_ED03));
            let mut fronts = vec![0_usize; queues.len()];
            let mut left: usize = queues.iter().map(|(_, _, q)| q.len()).sum();
            while left > 0 {
                let open: Vec<usize> = (0..queues.len())
                    .filter(|&q| fronts[q] < queues[q].2.len())
                    .collect();
                let q = open[mix.below(open.len())];
                let (input, which, queue) = &queues[q];
                order.push((inputs[*input][*which], queue[fronts[q]]));
                fronts[q] += 1;
                left -= 1;
            }
        }
    }
    order
}

/// Builds the program in mode `M`, listens to its observed nodes, and runs
/// its schedule. The window of the program is not read: what the listeners
/// see is the window `FromFirstTransaction`.
///
/// Panics only where the engine panics.
pub fn run<M: EngineMode>(program: &Program, options: RunOptions) -> Result<EngineRun, BuildError> {
    let types = check(program)?;
    let (mut graph, edge) = M::build(|b| build_program(b, program, &types));
    let live_nodes = graph.live_nodes();
    graph.set_shuffle_seed(options.shuffle_seed);
    let mut listeners = Vec::new();
    let mut recorders: Vec<Recorder> = edge
        .observed
        .into_iter()
        .map(|observed| Recorder::attach(&mut graph, observed, &mut listeners))
        .collect();
    for (k, sends) in program.schedule.iter().enumerate() {
        let order = engine_sends(
            sends,
            &edge.inputs,
            options.permute_sends.map(|seed| (seed, k)),
        );
        graph.transaction(|tx| {
            for (input, value) in order {
                match input {
                    EngineInput::Integer(input) => M::send(tx, input, value),
                    EngineInput::Boolean(input) => M::send(tx, input, truthy(value)),
                }
            }
        });
        for recorder in &mut recorders {
            recorder.end_transaction(&graph);
        }
    }
    drop(listeners);
    Ok(EngineRun {
        observations: recorders.into_iter().map(Recorder::finish).collect(),
        live_nodes,
    })
}
