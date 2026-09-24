//! A [`Program`] built with the Bough API, and its schedule run on it.
//!
//! [`run`] checks that a program is in the subset stages 1 to 5 of the
//! engine implement, builds it in one build closure the way a user would
//! write it, registers listeners on the observed nodes, and runs the
//! schedule: one `graph.transaction` per external transaction, with the
//! sends in the schedule's order unless [`RunOptions`] permutes them. It
//! records what each listener saw in each transaction, child transactions
//! included, the order of every listener call across the observed nodes,
//! and what `graph.sample` gives each observed cell after the transaction
//! and its children.
//!
//! # The subset
//!
//! Inputs of integers or booleans, coalescing or not. The definitions
//! `Input`, `InputCell`, `Never` and `Constant`; the adapters `Map`,
//! `Filter`, `FilterMap`, `MapTo`, `Snapshot`, `Gate` and `Once`; the stream
//! materializers `Node`, `Share`, `Merge`, `OrElse`, `Scan`, `Steps` and
//! `StepsWithCurrent`; the cells `Hold`, `Accumulate`, `AccumulateMut`,
//! `MapCell`, `ToBoolean` and `Lift` (stages 1 and 2). The loops `CellLoop`
//! and `StreamLoop` of integers or booleans, and `Close` (stage 3). `Split`
//! and `Defer`, and `MapList`, the one node that carries lists, which only a
//! `Split` may read (stage 4). The switches and the tokens they switch
//! among, without `construct` (stage 5): `PickStream`, `PickCell`,
//! `HoldStream`, `HoldCell`, `ConstantStream`, `ConstantCell`,
//! `MapPickCell`, `SwitchStream` and `SwitchCell`. An expression may
//! `Sample` a cell defined before it, which reads the cell in the build
//! closure, before transaction zero, as the oracle's top level does; the
//! engine has no value for a loop's forward before its `Close`, so a
//! `Sample` may not read one that is still open, directly, through a
//! read-through cell, or through a switch that may select it. Anything
//! else, and anything the oracle would refuse, is a [`BuildError`] naming
//! the node, from [`check`], before any node is built.
//!
//! [`check`] does not refuse a loop the engine refuses at its close, one
//! whose definition depends on its own forward in the same instant, or a
//! switch whose first link or move the engine refuses, one that selects a
//! cell or stream that depends on the switch itself: those refusals are
//! the engine's to make, and tests hold it to them.
//!
//! # Loops
//!
//! A `CellLoop` is declared with `b.cell_loop()` where the definition its
//! `Close` names is a `Cell`, and with `b.state_loop()` where it is a
//! `State`: an in-place accumulator, or a read-through cell over one. The
//! definition decides the forward's kind, so [`check`] looks ahead at each
//! loop's `Close`, and the loop's own node type is a `State` exactly when
//! its definition's is. A `StreamLoop` is `b.stream_loop()`, and its close
//! fuses the definition's chain into the forward's node, as a user's close
//! does. A `Close` makes no node.
//!
//! # Child transactions
//!
//! A `Defer` fuses the chain it consumes into its capture. A `MapList` is a
//! list-producing map, and it is not an adapter: a tenth adapter kind would
//! multiply the chain types (finding F36), so the builder materializes it
//! with `node` at once, fusing the chain before it, and the `Split` that
//! reads it takes that one stream of lists.
//!
//! # Switches
//!
//! A switch reads a cell of tokens, its outer: `HoldStream` or
//! `ConstantStream` for `switch_stream`, and `HoldCell`, `ConstantCell` or
//! `MapPickCell` for `switch_cell`. A pick maps an integer to one of the
//! tokens it lists, `index mod n`, and changes the event type, so like a
//! `MapList` it is not an adapter: the builder gives the chain before it a
//! node, and makes the pick `map(..).node(b)` over that node, which a hold
//! of tokens reads. The pick is compiled once per node type and token type
//! rather than once per chain type (finding F36).
//!
//! The tokens are shared streams of integers or booleans, `Shared<i64>`,
//! and cells of integers, `Cell<i64>` or `State<i64>`: `switch_stream`
//! gives a `Stream<i64>`, `switch_cell` over cells a `Cell<i64>` and over
//! `State`s a `State<i64>`. A cell of linear streams needs `construct`
//! (stage 6), so a stream a switch may follow is a `Share`. The engine
//! types a `Cell` and a `State` apart, so the cells one outer holds are all
//! of one kind; and it has no switch over a `State` of cells, so a
//! `MapPickCell` reads a `Cell`. A stream of tokens has one reader, a hold
//! of them, and a cell of tokens is read by switches alone, never observed.
//!
//! A pick's function and a `MapPickCell`'s capture the tokens they list,
//! and the collector cannot see a closure's captures (RFD 3). A hold's
//! value reaches the token it holds, and a switch its current inner, but a
//! token no selection has reached yet is reached by nothing else, and
//! would be collected before the pick selects it, which is then a stale
//! token. So the builder declares, with `Build::depends`, that a pick's
//! node and a `MapPickCell` keep every token they list.
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
//! it: when this was measured, the release build of the test binary took
//! 71 s, against 51 s for `Local` alone and 111 s with both modes at two.
//!
//! Every materializer is compiled once per chain type, so each one that
//! fuses a chain costs build time. Stages 3 and 4 add three, a `Defer`, a
//! `MapList` and a stream loop's close, to the eight of stages 1 and 2.
//! Measured again, one build after the other on one machine, they take the
//! release build of the test binary from 67 s to 80 s.
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
    Build, Cell, CellLoop, CellRef, Graph, Lift, Listener, Local, Node, Shared, Source, State,
    StateLoop, Stream, StreamLoop, TokenRef, Trace, Tracer, Transaction,
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
/// A `MapList`'s function: an event to a list, which a split reads.
pub type ListFn = Box<dyn Fn(i64) -> Vec<i64> + Send>;
/// A pick's function: an event to one of the tokens it lists.
pub type PickFn<T> = Box<dyn Fn(i64) -> T + Send>;
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
    /// `chain.defer(b)`.
    fn defer<S: Chain>(b: &mut Build<Self>, chain: S) -> Stream<i64>;
    /// `chain.map(f).node(b)`: a stream of lists, for a split to read.
    fn map_list<S: Chain>(b: &mut Build<Self>, chain: S, f: ListFn) -> Stream<Vec<i64>>;
    /// `lists.split(b)`.
    fn split(b: &mut Build<Self>, lists: Stream<Vec<i64>>) -> Stream<i64>;
    /// `closer.close(b, chain)`: the chain fused into the forward's node.
    fn close_stream_loop<S: Chain>(b: &mut Build<Self>, chain: S, closer: StreamLoop<i64>);
    /// `node.map(f).node(b)`: a stream of tokens, for a hold of them to read.
    fn pick<S: Chain, T: Send + 'static>(b: &mut Build<Self>, node: S, f: PickFn<T>) -> Stream<T>;
    /// `tokens.hold(b, initial)`: a cell of tokens.
    fn hold_tokens<T: Trace + Send + 'static>(
        b: &mut Build<Self>,
        tokens: Stream<T>,
        initial: T,
    ) -> Cell<T>;
    /// `b.constant(token)`: a cell of tokens that never steps.
    fn constant_token<T: Trace + Send + 'static>(b: &mut Build<Self>, token: T) -> Cell<T>;
    /// `outer.switch_stream(b)` over shared streams.
    fn switch_stream(b: &mut Build<Self>, outer: Cell<Shared<i64>>) -> Stream<i64>;
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
            fn defer<S: Chain>(b: &mut Build<Self>, chain: S) -> Stream<i64> {
                chain.defer(b)
            }
            fn map_list<S: Chain>(b: &mut Build<Self>, chain: S, f: ListFn) -> Stream<Vec<i64>> {
                chain.map(f).node(b)
            }
            fn split(b: &mut Build<Self>, lists: Stream<Vec<i64>>) -> Stream<i64> {
                lists.split(b)
            }
            fn close_stream_loop<S: Chain>(b: &mut Build<Self>, chain: S, closer: StreamLoop<i64>) {
                closer.close(b, chain)
            }
            fn pick<S: Chain, T: Send + 'static>(
                b: &mut Build<Self>,
                node: S,
                f: PickFn<T>,
            ) -> Stream<T> {
                node.map(f).node(b)
            }
            fn hold_tokens<T: Trace + Send + 'static>(
                b: &mut Build<Self>,
                tokens: Stream<T>,
                initial: T,
            ) -> Cell<T> {
                tokens.hold(b, initial)
            }
            fn constant_token<T: Trace + Send + 'static>(b: &mut Build<Self>, token: T) -> Cell<T> {
                b.constant(token)
            }
            fn switch_stream(b: &mut Build<Self>, outer: Cell<Shared<i64>>) -> Stream<i64> {
                outer.switch_stream(b)
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

    /// The scalar's plural, for messages: "integers" or "booleans".
    pub fn plural(self) -> &'static str {
        match self {
            Scalar::Integer => "integers",
            Scalar::Boolean => "booleans",
        }
    }
}

/// The tokens a stream of tokens carries or a cell of them holds, which a
/// switch switches among.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held {
    /// Shared streams of the scalar: `Shared<i64>`, a boolean as 0 or 1.
    Streams(Scalar),
    /// Cells of integers: `Cell<i64>`, or `State<i64>` when `state`.
    Cells {
        /// `State`s rather than `Cell`s.
        state: bool,
    },
}

impl fmt::Display for Held {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Held::Streams(scalar) => write!(formatter, "shared streams of {}", scalar.plural()),
            Held::Cells { state: false } => formatter.write_str("cells of integers"),
            Held::Cells { state: true } => formatter.write_str("States of integers"),
        }
    }
}

/// What a node of the subset makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeType {
    /// A stream.
    Stream(Scalar),
    /// A stream of lists of integers: a `MapList`, which only a `Split`
    /// reads.
    Lists,
    /// A stream of tokens: a pick, which only a hold of tokens reads.
    Tokens(Held),
    /// A cell; `state` when it is a `State`, which has no stream view.
    Cell {
        /// What it holds.
        value: Scalar,
        /// An in-place accumulator, a read-through cell over one, a switch
        /// over them, or a loop's forward whose definition is one of those.
        state: bool,
    },
    /// A cell of tokens, a switch's outer: a hold or a constant of tokens,
    /// or a `MapPickCell`. Only a switch reads it.
    Outer(Held),
    /// A `Close`, which makes no node.
    Closed,
}

impl fmt::Display for NodeType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeType::Stream(scalar) => write!(formatter, "a stream of {}", scalar.plural()),
            NodeType::Lists => formatter.write_str("a stream of lists"),
            NodeType::Tokens(held) => write!(formatter, "a stream of {held}"),
            NodeType::Cell {
                value,
                state: false,
            } => write!(formatter, "a cell of {}", value.plural()),
            NodeType::Cell { value, state: true } => {
                write!(formatter, "a State of {}", value.plural())
            }
            NodeType::Outer(held) => write!(formatter, "a cell of {held}"),
            NodeType::Closed => formatter.write_str("a Close, which makes no node"),
        }
    }
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

/// The streams a definition consumes: what linearity counts. A `Close`
/// names a stream for a stream loop and a cell for a cell loop, and
/// [`check_linearity`] counts only the streams.
pub(crate) fn consumed_streams(definition: &Definition) -> Vec<Reference> {
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
        | Definition::AccumulateMut { source, .. }
        | Definition::MapList { source, .. }
        | Definition::PickStream { source, .. }
        | Definition::PickCell { source, .. }
        | Definition::HoldStream { source, .. }
        | Definition::HoldCell { source, .. } => vec![*source],
        Definition::Once(source)
        | Definition::Node(source)
        | Definition::Share(source)
        | Definition::Split(source)
        | Definition::Defer(source) => vec![*source],
        Definition::Merge { left, right, .. } | Definition::OrElse { left, right } => {
            vec![*left, *right]
        }
        Definition::Close { definition, .. } => vec![*definition],
        _ => Vec::new(),
    }
}

/// The streams or cells a switch over the outer at `outer` may follow: a
/// hold of tokens' initial token and every token its pick lists, a
/// constant's token, or every cell a `MapPickCell` lists. Each is a
/// potential dependency of the switch: a `switch_cell` depends on the cell
/// it follows, and a `switch_stream` on the stream. Empty for a node that
/// is not an outer.
pub fn switch_candidates(definitions: &[Definition], outer: usize) -> Vec<Reference> {
    match definitions.get(outer) {
        Some(
            Definition::HoldStream { initial, source } | Definition::HoldCell { initial, source },
        ) => {
            let mut candidates = vec![*initial];
            if let Reference::TopLevel(pick) = source {
                match definitions.get(*pick) {
                    Some(Definition::PickStream {
                        streams: listed, ..
                    })
                    | Some(Definition::PickCell { cells: listed, .. }) => {
                        candidates.extend(listed.iter().copied());
                    }
                    _ => {}
                }
            }
            candidates
        }
        Some(Definition::ConstantStream(token) | Definition::ConstantCell(token)) => vec![*token],
        Some(Definition::MapPickCell { cells, .. }) => cells.clone(),
        _ => Vec::new(),
    }
}

/// Checks that a program is in the subset and well formed, and returns what
/// each node makes. Every error names the node at fault where there is one.
///
/// A cell loop is a `State` when the definition its `Close` names is one,
/// and the `Close` comes later. So [`states`] first works out which cells
/// are `State`s, loops included, and the definitions are checked once,
/// each loop declared as what its definition is.
pub fn check(program: &Program) -> Result<Vec<NodeType>, BuildError> {
    let mut inputs = Vec::with_capacity(program.inputs.len());
    for (index, input) in program.inputs.iter().enumerate() {
        inputs.push(check_input(index, input)?);
    }
    let states = states(program);
    let types = check_definitions(program, &inputs, &states)?;
    if program.observe.is_empty() {
        return Err(BuildError::program("observe: the program observes no node"));
    }
    for &node in &program.observe {
        match types.get(node) {
            None => {
                return Err(BuildError::program(format!(
                    "observe: there is no node {node}"
                )));
            }
            Some(
                made @ (NodeType::Lists
                | NodeType::Tokens(_)
                | NodeType::Outer(_)
                | NodeType::Closed),
            ) => {
                return Err(BuildError::program(format!(
                    "observe: node {node} is {made}; the comparison observes streams and cells \
                     of integers and booleans"
                )));
            }
            Some(_) => {}
        }
    }
    check_linearity(program, &types)?;
    check_schedule(program, &inputs)?;
    Ok(types)
}

/// Which nodes are `State`s, if the program is well typed: an in-place
/// accumulator; a read-through cell over a `State`; a switch that may
/// select one; and a cell loop whose definition is one. A loop's
/// definition comes after it, so this repeats until nothing changes; a
/// node only ever turns into a `State`, so it ends after at most one pass
/// per loop. [`check_definitions`] reports a program that is not well
/// typed.
fn states(program: &Program) -> Vec<bool> {
    let definitions = &program.definitions;
    let mut states = vec![false; definitions.len()];
    let mut closes = vec![None; definitions.len()];
    for definition in definitions {
        if let Definition::Close {
            forward,
            definition: Reference::TopLevel(node),
        } = definition
        {
            if let Some(close) = closes.get_mut(*forward) {
                *close = Some(*node);
            }
        }
    }
    loop {
        let mut changed = false;
        for (index, definition) in definitions.iter().enumerate() {
            let state = |reference: &Reference| match reference {
                Reference::TopLevel(node) => states.get(*node).copied().unwrap_or(false),
                Reference::Local(_) => false,
            };
            let candidates = |outer: &Reference| match outer {
                Reference::TopLevel(outer) => switch_candidates(definitions, *outer),
                Reference::Local(_) => Vec::new(),
            };
            let is = match definition {
                Definition::AccumulateMut { .. } => true,
                Definition::MapCell { cell, .. } | Definition::ToBoolean(cell) => state(cell),
                Definition::Lift { cells, .. } => cells.iter().any(state),
                Definition::SwitchCell(outer) => candidates(outer).iter().any(state),
                Definition::CellLoop(_) => {
                    closes[index].is_some_and(|node| states.get(node).copied().unwrap_or(false))
                }
                _ => false,
            };
            if is && !states[index] {
                states[index] = true;
                changed = true;
            }
        }
        if !changed {
            return states;
        }
    }
}

/// Checks every definition in order, the cell loops `states` marks taken
/// as `State`s, and then that every loop is closed.
fn check_definitions(
    program: &Program,
    inputs: &[Scalar],
    states: &[bool],
) -> Result<Vec<NodeType>, BuildError> {
    let mut types: Vec<NodeType> = Vec::with_capacity(program.definitions.len());
    let mut closes: Vec<Option<usize>> = vec![None; program.definitions.len()];
    for (index, definition) in program.definitions.iter().enumerate() {
        let scope = Scope {
            program,
            inputs,
            states,
            types: &types,
            closes: &closes,
            index,
        };
        let made = scope
            .definition(definition)
            .map_err(|message| BuildError::node(index, definition, message))?;
        if let Definition::Close {
            forward,
            definition: Reference::TopLevel(node),
        } = definition
        {
            closes[*forward] = Some(*node);
        }
        types.push(made);
    }
    for (index, definition) in program.definitions.iter().enumerate() {
        if is_loop(definition) && closes[index].is_none() {
            return Err(BuildError::node(
                index,
                definition,
                "the loop is never closed",
            ));
        }
    }
    Ok(types)
}

/// A `CellLoop` or a `StreamLoop`.
pub(crate) fn is_loop(definition: &Definition) -> bool {
    matches!(
        definition,
        Definition::CellLoop(_) | Definition::StreamLoop(_)
    )
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

/// Where a definition is checked: the program, what the nodes before it
/// make, and the loops closed before it.
struct Scope<'a> {
    program: &'a Program,
    inputs: &'a [Scalar],
    /// The cell loops taken as `State`s.
    states: &'a [bool],
    /// What each node before this one makes.
    types: &'a [NodeType],
    /// For each loop closed before this node, the node its `Close` names.
    closes: &'a [Option<usize>],
    /// This node.
    index: usize,
}

impl Scope<'_> {
    fn node(&self, reference: &Reference) -> Result<usize, String> {
        earlier(reference, self.index)
    }

    fn input(&self, k: usize) -> Result<Scalar, String> {
        self.inputs
            .get(k)
            .copied()
            .ok_or_else(|| format!("there is no input {k}"))
    }

    fn stream(&self, reference: &Reference) -> Result<Scalar, String> {
        let node = self.node(reference)?;
        match self.types[node] {
            NodeType::Stream(scalar) => Ok(scalar),
            NodeType::Cell { .. } | NodeType::Outer(_) => {
                Err(format!("{reference} is a cell, not a stream"))
            }
            NodeType::Lists => Err(format!(
                "{reference} is a stream of lists, which only a Split reads"
            )),
            NodeType::Tokens(held) => Err(format!(
                "{reference} is a stream of {held}, which only a hold of them reads"
            )),
            NodeType::Closed => Err(format!("{reference} is a Close, which makes no node")),
        }
    }

    fn cell(&self, reference: &Reference) -> Result<(Scalar, bool), String> {
        let node = self.node(reference)?;
        match self.types[node] {
            NodeType::Cell { value, state } => Ok((value, state)),
            NodeType::Stream(_) | NodeType::Lists | NodeType::Tokens(_) => {
                Err(format!("{reference} is a stream, not a cell"))
            }
            NodeType::Outer(held) => Err(format!(
                "{reference} is a cell of {held}, which only a switch reads"
            )),
            NodeType::Closed => Err(format!("{reference} is a Close, which makes no node")),
        }
    }

    /// A stream a switch may follow: a `Share`, of integers or booleans.
    fn shared(&self, reference: &Reference) -> Result<Scalar, String> {
        let scalar = self.stream(reference)?;
        match self.program.definitions[self.node(reference)?] {
            Definition::Share(_) => Ok(scalar),
            _ => Err(format!(
                "{reference} is a linear stream; a cell of linear streams needs construct \
                 (stage 6), so a switch here follows a Share"
            )),
        }
    }

    /// The streams a pick lists: one or more `Share`s of one scalar.
    fn shared_list(&self, streams: &[Reference]) -> Result<Scalar, String> {
        let mut scalars = streams.iter().map(|stream| self.shared(stream));
        let Some(first) = scalars.next() else {
            return Err("the list of choices is empty".into());
        };
        let first = first?;
        for scalar in scalars {
            if scalar? != first {
                return Err("the listed streams must have one type".into());
            }
        }
        Ok(first)
    }

    /// A cell a switch may follow: a cell of integers, whether it is a
    /// `State`.
    fn integer_cell(&self, reference: &Reference) -> Result<bool, String> {
        match self.cell(reference)? {
            (Scalar::Integer, state) => Ok(state),
            (Scalar::Boolean, _) => Err(format!(
                "{reference} is a cell of booleans; a switch here follows cells of integers"
            )),
        }
    }

    /// The cells a pick lists: one or more cells of integers, all `Cell`s
    /// or all `State`s, which the engine types apart.
    fn cell_list(&self, cells: &[Reference]) -> Result<bool, String> {
        let mut states = cells.iter().map(|cell| self.integer_cell(cell));
        let Some(first) = states.next() else {
            return Err("the list of choices is empty".into());
        };
        let first = first?;
        for state in states {
            if state? != first {
                return Err(
                    "the listed cells mix Cells and States, which the engine types apart".into(),
                );
            }
        }
        Ok(first)
    }

    /// What a hold of tokens reads: a pick's stream of tokens.
    fn tokens(&self, reference: &Reference) -> Result<Held, String> {
        let node = self.node(reference)?;
        match self.types[node] {
            NodeType::Tokens(held) => Ok(held),
            made => Err(format!(
                "{reference} is {made}; a hold of tokens reads a pick's stream of tokens"
            )),
        }
    }

    /// What a switch reads: a cell of tokens.
    fn outer(&self, reference: &Reference) -> Result<Held, String> {
        let node = self.node(reference)?;
        match self.types[node] {
            NodeType::Outer(held) => Ok(held),
            made => Err(format!(
                "{reference} is {made}; a switch reads a cell of tokens"
            )),
        }
    }

    fn expression(&self, expression: &Expression, arguments: usize) -> Result<(), String> {
        check_expression(expression, arguments, Some(self))
    }

    /// Whether the build closure can read cell `node` here. A loop's
    /// forward has no value before its `Close`, so it can be read once
    /// closed, if its definition can be; a read-through cell can be read if
    /// every cell it reads can be, and a switch if its outer and every cell
    /// the outer may select can be. `depth` ends the walk on a loop closed
    /// through itself, which the engine refuses at its close, or through a
    /// switch that may select it, which it refuses at the switch's first
    /// link or move, and where a read before that goes round the cycle and
    /// panics (finding F49).
    fn readable(&self, node: usize, depth: usize) -> Result<(), String> {
        if depth > self.types.len() {
            return Err(format!(
                "a Sample reads N {node} through a loop closed with itself"
            ));
        }
        let read = |reference: &Reference| match reference {
            Reference::TopLevel(cell) => self.readable(*cell, depth + 1),
            Reference::Local(_) => Ok(()),
        };
        match &self.program.definitions[node] {
            Definition::CellLoop(_) => match self.closes[node] {
                Some(definition) => self.readable(definition, depth + 1),
                None => Err(format!(
                    "a Sample reads N {node}, a loop's forward that is not closed here; \
                     the engine has no value for it before its Close"
                )),
            },
            Definition::MapCell { cell, .. }
            | Definition::ToBoolean(cell)
            | Definition::MapPickCell { cell, .. } => read(cell),
            Definition::Lift { cells, .. } => cells.iter().try_for_each(read),
            Definition::SwitchCell(outer) => {
                read(outer)?;
                match outer {
                    Reference::TopLevel(outer) => {
                        switch_candidates(&self.program.definitions, *outer)
                            .iter()
                            .try_for_each(read)
                    }
                    Reference::Local(_) => Ok(()),
                }
            }
            _ => Ok(()),
        }
    }

    fn definition(&self, definition: &Definition) -> Result<NodeType, String> {
        let made = match definition {
            Definition::Input(k) => NodeType::Stream(self.input(*k)?),
            Definition::InputCell { input: k, initial } => {
                let value = self.input(*k)?;
                self.expression(initial, 0)?;
                NodeType::Cell {
                    value,
                    state: false,
                }
            }
            Definition::Never(event_type) => {
                NodeType::Stream(Scalar::of(event_type).ok_or_else(|| {
                    format!(
                        "a stream of {event_type} is outside the subset, which has integers \
                         and booleans"
                    )
                })?)
            }
            Definition::Constant(value) => {
                self.expression(value, 0)?;
                NodeType::Cell {
                    value: Scalar::Integer,
                    state: false,
                }
            }
            Definition::Map { function, source } => {
                self.stream(source)?;
                self.expression(function, 1)?;
                NodeType::Stream(Scalar::Integer)
            }
            Definition::Filter { predicate, source } => {
                let scalar = self.stream(source)?;
                self.expression(predicate, 1)?;
                NodeType::Stream(scalar)
            }
            Definition::FilterMap {
                keep,
                function,
                source,
            } => {
                self.stream(source)?;
                self.expression(keep, 1)?;
                self.expression(function, 1)?;
                NodeType::Stream(Scalar::Integer)
            }
            Definition::MapTo { value, source } => {
                self.stream(source)?;
                NodeType::Stream(Scalar::of_value(value).ok_or_else(|| {
                    format!("{value} is outside the subset, which has integers and booleans")
                })?)
            }
            Definition::Snapshot {
                function,
                source,
                cell,
            } => {
                self.stream(source)?;
                self.cell(cell)?;
                self.expression(function, 2)?;
                NodeType::Stream(Scalar::Integer)
            }
            Definition::Gate { source, cell } => {
                let scalar = self.stream(source)?;
                let (value, _) = self.cell(cell)?;
                if value != Scalar::Boolean {
                    return Err(format!(
                        "{cell} is a cell of integers; a gate reads a cell of booleans"
                    ));
                }
                NodeType::Stream(scalar)
            }
            Definition::Once(source)
            | Definition::Node(source)
            | Definition::Share(source)
            | Definition::Defer(source) => NodeType::Stream(self.stream(source)?),
            Definition::Merge {
                function,
                left,
                right,
            } => {
                let scalar = self.stream(left)?;
                if self.stream(right)? != scalar {
                    return Err("a merge needs two streams of one type".into());
                }
                self.expression(function, 2)?;
                NodeType::Stream(scalar)
            }
            Definition::OrElse { left, right } => {
                let scalar = self.stream(left)?;
                if self.stream(right)? != scalar {
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
                self.stream(source)?;
                self.expression(initial, 0)?;
                self.expression(output, 2)?;
                self.expression(state, 2)?;
                NodeType::Stream(Scalar::Integer)
            }
            Definition::Steps(cell) | Definition::StepsWithCurrent(cell) => {
                let (value, state) = self.cell(cell)?;
                if state {
                    return Err(format!(
                        "{cell} is a State, an in-place accumulator, a cell read through one, \
                         or a loop closed with one, which has no stream view"
                    ));
                }
                NodeType::Stream(value)
            }
            Definition::Hold { initial, source } => {
                let value = self.stream(source)?;
                self.expression(initial, 0)?;
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
                self.stream(source)?;
                self.expression(initial, 0)?;
                self.expression(function, 2)?;
                NodeType::Cell {
                    value: Scalar::Integer,
                    state: matches!(definition, Definition::AccumulateMut { .. }),
                }
            }
            Definition::MapCell { function, cell } => {
                let (_, state) = self.cell(cell)?;
                self.expression(function, 1)?;
                NodeType::Cell {
                    value: Scalar::Integer,
                    state,
                }
            }
            Definition::ToBoolean(cell) => {
                let (_, state) = self.cell(cell)?;
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
                for cell in cells {
                    state |= self.cell(cell)?.1;
                }
                self.expression(function, cells.len())?;
                NodeType::Cell {
                    value: Scalar::Integer,
                    state,
                }
            }
            Definition::MapList {
                length,
                element,
                source,
            } => {
                self.stream(source)?;
                self.expression(length, 1)?;
                self.expression(element, 2)?;
                NodeType::Lists
            }
            Definition::Split(source) => {
                let node = self.node(source)?;
                if self.types[node] != NodeType::Lists {
                    return Err(format!(
                        "{source} is {}; a split reads the lists of a MapList",
                        self.types[node]
                    ));
                }
                NodeType::Stream(Scalar::Integer)
            }
            Definition::CellLoop(value_type) => NodeType::Cell {
                value: loop_scalar(value_type)?,
                state: self.states[self.index],
            },
            Definition::StreamLoop(event_type) => NodeType::Stream(loop_scalar(event_type)?),
            Definition::Close {
                forward,
                definition: reference,
            } => {
                let declared = match self.program.definitions.get(*forward) {
                    Some(declaration) if *forward < self.index && is_loop(declaration) => {
                        self.types[*forward]
                    }
                    _ => {
                        return Err(format!(
                            "node {forward} is not a loop declared before this Close"
                        ));
                    }
                };
                if self.closes[*forward].is_some() {
                    return Err(format!(
                        "the loop at node {forward} is closed more than once"
                    ));
                }
                let actual = self.types[self.node(reference)?];
                let fits = match (declared, actual) {
                    (NodeType::Cell { value: a, .. }, NodeType::Cell { value: b, .. })
                    | (NodeType::Stream(a), NodeType::Stream(b)) => a == b,
                    _ => false,
                };
                if !fits {
                    return Err(format!(
                        "the loop at node {forward} is {declared}, and {reference} is {actual}"
                    ));
                }
                NodeType::Closed
            }
            Definition::PickStream {
                index,
                streams,
                source,
            } => {
                self.stream(source)?;
                self.expression(index, 1)?;
                NodeType::Tokens(Held::Streams(self.shared_list(streams)?))
            }
            Definition::PickCell {
                index,
                cells,
                source,
            } => {
                self.stream(source)?;
                self.expression(index, 1)?;
                NodeType::Tokens(Held::Cells {
                    state: self.cell_list(cells)?,
                })
            }
            Definition::HoldStream { initial, source } => {
                let held = self.tokens(source)?;
                let scalar = self.shared(initial)?;
                if held != Held::Streams(scalar) {
                    return Err(format!(
                        "{source} carries {held}, and the initial {initial} is a shared stream \
                         of {}",
                        scalar.plural()
                    ));
                }
                NodeType::Outer(held)
            }
            Definition::HoldCell { initial, source } => {
                let held = self.tokens(source)?;
                let state = self.integer_cell(initial)?;
                if held != (Held::Cells { state }) {
                    return Err(format!(
                        "{source} carries {held}, and the initial {initial} is {}",
                        self.types[self.node(initial)?]
                    ));
                }
                NodeType::Outer(held)
            }
            Definition::ConstantStream(stream) => {
                NodeType::Outer(Held::Streams(self.shared(stream)?))
            }
            Definition::ConstantCell(cell) => NodeType::Outer(Held::Cells {
                state: self.integer_cell(cell)?,
            }),
            Definition::MapPickCell { index, cells, cell } => {
                let (_, state) = self.cell(cell)?;
                if state {
                    return Err(format!(
                        "{cell} is a State; a map_cell of it would be a State of cells, which \
                         the engine has no switch over"
                    ));
                }
                self.expression(index, 1)?;
                NodeType::Outer(Held::Cells {
                    state: self.cell_list(cells)?,
                })
            }
            Definition::SwitchStream(outer) => match self.outer(outer)? {
                Held::Streams(scalar) => NodeType::Stream(scalar),
                held => {
                    return Err(format!(
                        "{outer} is a cell of {held}; switch_stream needs a cell of streams"
                    ));
                }
            },
            Definition::SwitchCell(outer) => match self.outer(outer)? {
                Held::Cells { state } => NodeType::Cell {
                    value: Scalar::Integer,
                    state,
                },
                held => {
                    return Err(format!(
                        "{outer} is a cell of {held}; switch_cell needs a cell of cells"
                    ));
                }
            },
            Definition::Literal { .. } | Definition::Construct { .. } => {
                return Err(format!(
                    "{} is outside the subset of stages 1 to 5",
                    name(definition)
                ));
            }
        };
        Ok(made)
    }
}

/// The scalar a loop carries.
fn loop_scalar(value_type: &Type) -> Result<Scalar, String> {
    Scalar::of(value_type).ok_or_else(|| {
        format!("a loop of {value_type} is outside the subset, which has integers and booleans")
    })
}

/// Checks an expression taking `arguments` arguments. `scope` says what a
/// `Sample` may read, and is `None` where it may read nothing.
fn check_expression(
    expression: &Expression,
    arguments: usize,
    scope: Option<&Scope<'_>>,
) -> Result<(), String> {
    let go = |e: &Expression| check_expression(e, arguments, scope);
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
            let Some(scope) = scope else {
                return Err("an input's coalescing function cannot sample a cell".into());
            };
            let node = scope.node(reference)?;
            match scope.types[node] {
                NodeType::Cell { .. } => scope.readable(node, 0),
                made => Err(format!("Sample {reference}: node {node} is {made}")),
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

/// A stream node other than a `Share` has at most one consumer, the
/// observation included; so has a stream of lists or of tokens.
fn check_linearity(program: &Program, types: &[NodeType]) -> Result<(), BuildError> {
    let mut consumers: Vec<Vec<String>> = vec![Vec::new(); types.len()];
    for (index, definition) in program.definitions.iter().enumerate() {
        for reference in consumed_streams(definition) {
            if let Reference::TopLevel(node) = reference {
                if matches!(
                    types[node],
                    NodeType::Stream(_) | NodeType::Lists | NodeType::Tokens(_)
                ) {
                    consumers[node].push(format!("node {index}"));
                }
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
    fn defer(self: Box<Self>, b: &mut Build<M>) -> Stream<i64>;
    fn map_list(self: Box<Self>, b: &mut Build<M>, f: ListFn) -> Stream<Vec<i64>>;
    fn close_stream_loop(self: Box<Self>, b: &mut Build<M>, closer: StreamLoop<i64>);
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
    fn defer(self: Box<Self>, b: &mut Build<M>) -> Stream<i64> {
        M::defer(b, *self)
    }
    fn map_list(self: Box<Self>, b: &mut Build<M>, f: ListFn) -> Stream<Vec<i64>> {
        M::map_list(b, *self, f)
    }
    fn close_stream_loop(self: Box<Self>, b: &mut Build<M>, closer: StreamLoop<i64>) {
        M::close_stream_loop(b, *self, closer)
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

    fn defer(self, b: &mut Build<M>) -> Stream<i64> {
        materialize!(self, M, b, defer())
    }

    fn map_list(self, b: &mut Build<M>, f: ListFn) -> Stream<Vec<i64>> {
        materialize!(self, M, b, map_list(f))
    }

    fn close_stream_loop(self, b: &mut Build<M>, closer: StreamLoop<i64>) {
        materialize!(self, M, b, close_stream_loop(closer))
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

// ----- switches -----

/// A pick's stream of tokens, as its tokens' type says.
enum Tokens {
    Streams(Stream<Shared<i64>>),
    Cells(Stream<Cell<i64>>),
    States(Stream<State<i64>>),
}

/// A cell of tokens, which a switch reads, as its tokens' type says.
#[derive(Clone, Copy)]
enum Outer {
    Streams(Cell<Shared<i64>>),
    Cells(Cell<Cell<i64>>),
    States(Cell<State<i64>>),
}

/// The cells one outer may select: all `Cell`s or all `State`s.
#[derive(Clone)]
enum Choices {
    Cells(Vec<Cell<i64>>),
    States(Vec<State<i64>>),
}

impl Choices {
    /// Declares that `node`, whose function captures these cells, keeps
    /// them alive.
    fn declare<M: EngineMode>(&self, b: &mut Build<M>, node: &impl TokenRef) {
        match self {
            Choices::Cells(cells) => declare(b, node, cells),
            Choices::States(states) => declare(b, node, states),
        }
    }

    fn of(tokens: Vec<CellToken>) -> Choices {
        match tokens.first() {
            Some(CellToken::IntegerState(_)) => Choices::States(
                tokens
                    .into_iter()
                    .map(|token| match token {
                        CellToken::IntegerState(state) => state,
                        _ => unreachable!("bough-oracle: check lists States or Cells, not both"),
                    })
                    .collect(),
            ),
            _ => Choices::Cells(
                tokens
                    .into_iter()
                    .map(|token| match token {
                        CellToken::Integer(cell) => cell,
                        _ => unreachable!("bough-oracle: check lists cells of integers"),
                    })
                    .collect(),
            ),
        }
    }
}

/// The token an index picks: `choices[index mod n]`, Haskell's `mod` for
/// a positive `n`.
fn picked<T: Copy>(choices: &[T], index: i64) -> T {
    choices[index.rem_euclid(choices.len() as i64) as usize]
}

/// A pick's function: the index expression of the event, to a token.
fn picker<T: Copy + Send + 'static>(index: Expression, choices: Vec<T>) -> PickFn<T> {
    Box::new(move |x| picked(&choices, evaluate(&index, &[x])))
}

/// Declares that `node`, whose function captures `tokens`, keeps them
/// alive: the collector cannot see a closure's captures (RFD 3).
fn declare<M: EngineMode, T: TokenRef>(b: &mut Build<M>, node: &impl TokenRef, tokens: &[T]) {
    let on: Vec<&dyn TokenRef> = tokens.iter().map(|token| token as &dyn TokenRef).collect();
    b.depends(node, &on);
}

/// `map(f).node(b)` over a materialized stream.
fn pick<M: EngineMode, T: Send + 'static>(b: &mut Build<M>, node: Base, f: PickFn<T>) -> Stream<T> {
    match node {
        Base::Stream(stream) => M::pick(b, stream, f),
        Base::Shared(shared) => M::pick(b, shared, f),
    }
}

// ----- building -----

/// A node of the program while the build closure runs.
enum Built<M: EngineMode> {
    Stream(Chained<M>),
    /// A `MapList`'s node, moved into the split that reads it.
    Lists(Stream<Vec<i64>>),
    /// A pick's node, moved into the hold that reads it.
    Tokens(Tokens),
    /// A linear stream, moved into its one consumer.
    Consumed,
    Cell(CellToken),
    /// A cell of tokens, which switches read.
    Outer(Outer),
    /// A `Close`, which makes no node.
    Closed,
}

/// The closer of a loop, waiting in the builder for the loop's `Close`.
enum Closer {
    Integer(CellLoop<i64>),
    IntegerState(StateLoop<i64>),
    Boolean(CellLoop<bool>),
    BooleanState(StateLoop<bool>),
    Stream(StreamLoop<i64>),
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
    /// Each loop's closer, from its declaration to its `Close`.
    closers: Vec<Option<Closer>>,
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
            Built::Lists(_)
            | Built::Tokens(_)
            | Built::Cell(_)
            | Built::Outer(_)
            | Built::Closed => {
                unreachable!("bough-oracle: check refuses node {node} as a stream of the subset")
            }
        }
    }

    /// The lists a split reads, moved out of the `MapList`'s node.
    fn take_lists(&mut self, reference: &Reference) -> Stream<Vec<i64>> {
        let node = top(reference);
        match mem::replace(&mut self.nodes[node], Built::Consumed) {
            Built::Lists(lists) => lists,
            _ => unreachable!("bough-oracle: check lets a split read only a MapList, once"),
        }
    }

    /// The tokens a hold of them reads, moved out of the pick's node.
    fn take_tokens(&mut self, reference: &Reference) -> Tokens {
        let node = top(reference);
        match mem::replace(&mut self.nodes[node], Built::Consumed) {
            Built::Tokens(tokens) => tokens,
            _ => unreachable!("bough-oracle: check lets a hold of tokens read only a pick, once"),
        }
    }

    /// A shared stream, as a token a switch may follow.
    fn shared(&self, reference: &Reference) -> Shared<i64> {
        match &self.nodes[top(reference)] {
            Built::Stream(Chained::Shared(shared)) => *shared,
            _ => unreachable!("bough-oracle: check lets a switch follow a Share only"),
        }
    }

    /// The cells a pick or a `MapPickCell` lists.
    fn choices(&self, cells: &[Reference]) -> Choices {
        Choices::of(cells.iter().map(|cell| self.cell(cell)).collect())
    }

    fn outer(&self, reference: &Reference) -> Outer {
        match &self.nodes[top(reference)] {
            Built::Outer(outer) => *outer,
            _ => unreachable!("bough-oracle: check lets a switch read only a cell of tokens"),
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
            NodeType::Lists | NodeType::Tokens(_) | NodeType::Outer(_) | NodeType::Closed => {
                unreachable!("bough-oracle: check refuses {reference} where a scalar is read")
            }
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
            Definition::MapList {
                length,
                element,
                source,
            } => {
                let length = self.resolve(b, length);
                let element = self.resolve(b, element);
                let f: ListFn = Box::new(move |x| {
                    let items = evaluate(&length, &[x]).rem_euclid(4);
                    (0..items).map(|i| evaluate(&element, &[x, i])).collect()
                });
                let chain = self.take(source);
                Built::Lists(chain.map_list(b, f))
            }
            Definition::Split(source) => {
                let lists = self.take_lists(source);
                Built::Stream(Chained::Stream(M::split(b, lists)))
            }
            Definition::Defer(source) => {
                let chain = self.take(source);
                Built::Stream(Chained::Stream(chain.defer(b)))
            }
            Definition::PickStream {
                index,
                streams,
                source,
            } => {
                let index = self.resolve(b, index);
                let streams: Vec<Shared<i64>> = streams.iter().map(|s| self.shared(s)).collect();
                let node = self.take(source).materialized(b);
                let tokens = pick(b, node, picker(index, streams.clone()));
                declare(b, &tokens, &streams);
                Built::Tokens(Tokens::Streams(tokens))
            }
            Definition::PickCell {
                index,
                cells,
                source,
            } => {
                let index = self.resolve(b, index);
                let choices = self.choices(cells);
                let node = self.take(source).materialized(b);
                Built::Tokens(match choices {
                    Choices::Cells(cells) => {
                        let tokens = pick(b, node, picker(index, cells.clone()));
                        declare(b, &tokens, &cells);
                        Tokens::Cells(tokens)
                    }
                    Choices::States(states) => {
                        let tokens = pick(b, node, picker(index, states.clone()));
                        declare(b, &tokens, &states);
                        Tokens::States(tokens)
                    }
                })
            }
            Definition::HoldStream { initial, source } => {
                let initial = self.shared(initial);
                let Tokens::Streams(tokens) = self.take_tokens(source) else {
                    unreachable!("bough-oracle: check holds streams from a pick of streams")
                };
                Built::Outer(Outer::Streams(M::hold_tokens(b, tokens, initial)))
            }
            Definition::HoldCell { initial, source } => {
                let outer = match (self.take_tokens(source), self.cell(initial)) {
                    (Tokens::Cells(tokens), CellToken::Integer(cell)) => {
                        Outer::Cells(M::hold_tokens(b, tokens, cell))
                    }
                    (Tokens::States(tokens), CellToken::IntegerState(state)) => {
                        Outer::States(M::hold_tokens(b, tokens, state))
                    }
                    _ => unreachable!("bough-oracle: check holds cells of one kind"),
                };
                Built::Outer(outer)
            }
            Definition::ConstantStream(stream) => {
                let stream = self.shared(stream);
                Built::Outer(Outer::Streams(M::constant_token(b, stream)))
            }
            Definition::ConstantCell(cell) => Built::Outer(match self.cell(cell) {
                CellToken::Integer(cell) => Outer::Cells(M::constant_token(b, cell)),
                CellToken::IntegerState(state) => Outer::States(M::constant_token(b, state)),
                _ => unreachable!("bough-oracle: check holds cells of integers"),
            }),
            Definition::MapPickCell { index, cells, cell } => {
                let index = self.resolve(b, index);
                let listed = self.choices(cells);
                let outer = match (self.cell(cell), listed.clone()) {
                    (CellToken::Integer(cell), Choices::Cells(choices)) => {
                        Outer::Cells(M::map_cell(
                            b,
                            cell,
                            Box::new(move |v: &i64| picked(&choices, evaluate(&index, &[*v]))),
                        ))
                    }
                    (CellToken::Integer(cell), Choices::States(choices)) => {
                        Outer::States(M::map_cell(
                            b,
                            cell,
                            Box::new(move |v: &i64| picked(&choices, evaluate(&index, &[*v]))),
                        ))
                    }
                    (CellToken::Boolean(cell), Choices::Cells(choices)) => {
                        Outer::Cells(M::map_cell(
                            b,
                            cell,
                            Box::new(move |v: &bool| {
                                picked(&choices, evaluate(&index, &[i64::from(*v)]))
                            }),
                        ))
                    }
                    (CellToken::Boolean(cell), Choices::States(choices)) => {
                        Outer::States(M::map_cell(
                            b,
                            cell,
                            Box::new(move |v: &bool| {
                                picked(&choices, evaluate(&index, &[i64::from(*v)]))
                            }),
                        ))
                    }
                    _ => unreachable!("bough-oracle: check refuses a MapPickCell over a State"),
                };
                match outer {
                    Outer::Cells(outer) => listed.declare(b, &outer),
                    Outer::States(outer) => listed.declare(b, &outer),
                    Outer::Streams(_) => unreachable!("bough-oracle: a MapPickCell lists cells"),
                }
                Built::Outer(outer)
            }
            Definition::SwitchStream(outer) => {
                let Outer::Streams(outer) = self.outer(outer) else {
                    unreachable!("bough-oracle: check switches streams over a cell of streams")
                };
                Built::Stream(Chained::Stream(M::switch_stream(b, outer)))
            }
            Definition::SwitchCell(outer) => Built::Cell(match self.outer(outer) {
                Outer::Cells(outer) => CellToken::Integer(outer.switch_cell(b)),
                Outer::States(outer) => CellToken::IntegerState(outer.switch_cell(b)),
                Outer::Streams(_) => {
                    unreachable!("bough-oracle: check switches cells over a cell of cells")
                }
            }),
            Definition::CellLoop(_) => {
                let index = self.nodes.len();
                let NodeType::Cell { value, state } = self.types[index] else {
                    unreachable!("bough-oracle: check makes a cell loop a cell")
                };
                let (cell, closer) = match (value, state) {
                    (Scalar::Integer, false) => {
                        let (cell, closer) = b.cell_loop::<i64>();
                        (CellToken::Integer(cell), Closer::Integer(closer))
                    }
                    (Scalar::Integer, true) => {
                        let (state, closer) = b.state_loop::<i64>();
                        (CellToken::IntegerState(state), Closer::IntegerState(closer))
                    }
                    (Scalar::Boolean, false) => {
                        let (cell, closer) = b.cell_loop::<bool>();
                        (CellToken::Boolean(cell), Closer::Boolean(closer))
                    }
                    (Scalar::Boolean, true) => {
                        let (state, closer) = b.state_loop::<bool>();
                        (CellToken::BooleanState(state), Closer::BooleanState(closer))
                    }
                };
                self.closers[index] = Some(closer);
                Built::Cell(cell)
            }
            Definition::StreamLoop(_) => {
                let index = self.nodes.len();
                let (stream, closer) = b.stream_loop::<i64>();
                self.closers[index] = Some(Closer::Stream(closer));
                Built::Stream(Chained::Stream(stream))
            }
            Definition::Close {
                forward,
                definition,
            } => {
                let closer = self.closers[*forward]
                    .take()
                    .expect("bough-oracle: check closes a loop once");
                // check makes a cell loop a State exactly when its definition
                // is one, and a state loop may close with either.
                match (closer, self.nodes.get(top(definition))) {
                    (Closer::Stream(closer), _) => {
                        let chain = self.take(definition);
                        chain.close_stream_loop(b, closer);
                    }
                    (Closer::Integer(closer), Some(Built::Cell(CellToken::Integer(cell)))) => {
                        closer.close(b, *cell);
                    }
                    (
                        Closer::IntegerState(closer),
                        Some(Built::Cell(CellToken::IntegerState(state))),
                    ) => closer.close(b, *state),
                    (Closer::IntegerState(closer), Some(Built::Cell(CellToken::Integer(cell)))) => {
                        closer.close(b, *cell);
                    }
                    (Closer::Boolean(closer), Some(Built::Cell(CellToken::Boolean(cell)))) => {
                        closer.close(b, *cell);
                    }
                    (
                        Closer::BooleanState(closer),
                        Some(Built::Cell(CellToken::BooleanState(state))),
                    ) => closer.close(b, *state),
                    (Closer::BooleanState(closer), Some(Built::Cell(CellToken::Boolean(cell)))) => {
                        closer.close(b, *cell);
                    }
                    _ => unreachable!(
                        "bough-oracle: check closes a loop with a node of its type and kind"
                    ),
                }
                Built::Closed
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
            Built::Consumed
            | Built::Lists(_)
            | Built::Tokens(_)
            | Built::Outer(_)
            | Built::Closed => {
                unreachable!("bough-oracle: check refuses observing node {node}")
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
        closers: program.definitions.iter().map(|_| None).collect(),
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

/// What the engine showed for one observed node. A transaction's calls
/// include those of its child transactions, in the order they came.
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
        /// transaction zero and its children.
        registration: Vec<i64>,
        /// `listen_steps`'s calls at registration: none.
        steps_registration: Vec<i64>,
        /// `listen_cell`'s calls in transaction k, at index k - 1.
        values: Vec<Vec<i64>>,
        /// `listen_steps`'s calls in transaction k, at index k - 1.
        steps: Vec<Vec<i64>>,
        /// `graph.sample` after transaction k and its children, at index
        /// k - 1.
        samples: Vec<i64>,
    },
}

/// Which listener of an observed node a call went to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Listened {
    /// `graph.listen` on a stream.
    Stream,
    /// `graph.listen_cell`.
    Cell,
    /// `graph.listen_steps`.
    Steps,
}

impl fmt::Display for Listened {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Listened::Stream => "listen",
            Listened::Cell => "listen_cell",
            Listened::Steps => "listen_steps",
        })
    }
}

/// One listener call in a transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Call {
    /// The observed node's position in the program's `observe`.
    pub observed: usize,
    /// Which of its listeners.
    pub listened: Listened,
    /// The transaction: k - 1 for transaction k.
    pub transaction: usize,
    /// The call's place among that listener's calls in that transaction.
    pub index: usize,
}

/// Everything one run showed, one observation per observed node, in the
/// order the program lists them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineRun {
    /// The observations.
    pub observations: Vec<EngineObservation>,
    /// Every listener call made in a transaction, over every observed node,
    /// in the order the engine made them.
    pub calls: Vec<Call>,
    /// `graph.live_nodes()` after the build: how many nodes the chains
    /// fused into.
    pub live_nodes: usize,
}

/// Every listener's calls, in the order they came: the observed node's
/// position, the listener, and the value.
type Log = Arc<Mutex<Vec<(usize, Listened, i64)>>>;

fn drain(log: &Log) -> Vec<(usize, Listened, i64)> {
    mem::take(&mut *log.lock().unwrap_or_else(PoisonError::into_inner))
}

fn stream_sink(log: &Log, observed: usize) -> StreamSink {
    let log = log.clone();
    Box::new(move |value| {
        log.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((observed, Listened::Stream, value))
    })
}

fn integer_sink(log: &Log, observed: usize, listened: Listened) -> CellSink<i64> {
    let log = log.clone();
    Box::new(move |value: &i64| {
        log.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((observed, listened, *value))
    })
}

fn boolean_sink(log: &Log, observed: usize, listened: Listened) -> CellSink<bool> {
    let log = log.clone();
    Box::new(move |value: &bool| {
        log.lock().unwrap_or_else(PoisonError::into_inner).push((
            observed,
            listened,
            i64::from(*value),
        ))
    })
}

/// What one observed node's listeners saw so far.
enum Recorder {
    Stream {
        events: Vec<Vec<i64>>,
    },
    Cell {
        cell: CellToken,
        observation: EngineObservation,
    },
}

impl Recorder {
    /// Listens to observed node `position`, and files the calls a cell's
    /// listeners make at registration.
    fn attach<M: EngineMode>(
        graph: &mut Graph<M>,
        position: usize,
        observed: Observed,
        log: &Log,
        listeners: &mut Vec<Listener<M>>,
    ) -> Recorder {
        match observed {
            Observed::Stream(stream) => {
                listeners.push(M::listen(graph, stream, stream_sink(log, position)));
                Recorder::Stream { events: Vec::new() }
            }
            Observed::Shared(shared) => {
                listeners.push(M::listen(graph, shared, stream_sink(log, position)));
                Recorder::Stream { events: Vec::new() }
            }
            Observed::Cell(cell) => {
                let (values, steps) = (Listened::Cell, Listened::Steps);
                match cell {
                    CellToken::Integer(c) => {
                        listeners.push(M::listen_cell(
                            graph,
                            c,
                            integer_sink(log, position, values),
                        ));
                        listeners.push(M::listen_steps(
                            graph,
                            c,
                            integer_sink(log, position, steps),
                        ));
                    }
                    CellToken::IntegerState(c) => {
                        listeners.push(M::listen_cell(
                            graph,
                            c,
                            integer_sink(log, position, values),
                        ));
                        listeners.push(M::listen_steps(
                            graph,
                            c,
                            integer_sink(log, position, steps),
                        ));
                    }
                    CellToken::Boolean(c) => {
                        listeners.push(M::listen_cell(
                            graph,
                            c,
                            boolean_sink(log, position, values),
                        ));
                        listeners.push(M::listen_steps(
                            graph,
                            c,
                            boolean_sink(log, position, steps),
                        ));
                    }
                    CellToken::BooleanState(c) => {
                        listeners.push(M::listen_cell(
                            graph,
                            c,
                            boolean_sink(log, position, values),
                        ));
                        listeners.push(M::listen_steps(
                            graph,
                            c,
                            boolean_sink(log, position, steps),
                        ));
                    }
                }
                let (mut registration, mut steps_registration) = (Vec::new(), Vec::new());
                for (_, listened, value) in drain(log) {
                    match listened {
                        Listened::Steps => steps_registration.push(value),
                        _ => registration.push(value),
                    }
                }
                Recorder::Cell {
                    cell,
                    observation: EngineObservation::Cell {
                        registration,
                        steps_registration,
                        values: Vec::new(),
                        steps: Vec::new(),
                        samples: Vec::new(),
                    },
                }
            }
        }
    }

    /// Opens the lists of a new transaction.
    fn begin_transaction(&mut self) {
        match self {
            Recorder::Stream { events } => events.push(Vec::new()),
            Recorder::Cell {
                observation: EngineObservation::Cell { values, steps, .. },
                ..
            } => {
                values.push(Vec::new());
                steps.push(Vec::new());
            }
            Recorder::Cell { .. } => unreachable!("bough-oracle: a cell records a cell"),
        }
    }

    /// Files a call of this transaction, and returns its place among that
    /// listener's calls in it.
    fn file(&mut self, listened: Listened, value: i64) -> usize {
        let list = match (self, listened) {
            (Recorder::Stream { events }, Listened::Stream) => events.last_mut(),
            (
                Recorder::Cell {
                    observation: EngineObservation::Cell { values, .. },
                    ..
                },
                Listened::Cell,
            ) => values.last_mut(),
            (
                Recorder::Cell {
                    observation: EngineObservation::Cell { steps, .. },
                    ..
                },
                Listened::Steps,
            ) => steps.last_mut(),
            _ => None,
        }
        .expect("bough-oracle: a call goes to a listener of its node, in a transaction");
        list.push(value);
        list.len() - 1
    }

    /// Samples a cell after the transaction and its children.
    fn end_transaction<M: EngineMode>(&mut self, graph: &Graph<M>) {
        if let Recorder::Cell {
            cell,
            observation: EngineObservation::Cell { samples, .. },
        } = self
        {
            samples.push(cell.sample_graph(graph));
        }
    }

    fn finish(self) -> EngineObservation {
        match self {
            Recorder::Stream { events } => EngineObservation::Stream { events },
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
    let log = Log::default();
    let mut listeners = Vec::new();
    let mut recorders: Vec<Recorder> = edge
        .observed
        .into_iter()
        .enumerate()
        .map(|(position, observed)| {
            Recorder::attach(&mut graph, position, observed, &log, &mut listeners)
        })
        .collect();
    let mut calls = Vec::new();
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
            recorder.begin_transaction();
        }
        for (observed, listened, value) in drain(&log) {
            let index = recorders[observed].file(listened, value);
            calls.push(Call {
                observed,
                listened,
                transaction: k,
                index,
            });
        }
        for recorder in &mut recorders {
            recorder.end_transaction(&graph);
        }
    }
    drop(listeners);
    Ok(EngineRun {
        observations: recorders.into_iter().map(Recorder::finish).collect(),
        calls,
        live_nodes,
    })
}
