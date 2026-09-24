//! Random well-typed programs in the subset of stages 1 and 2, and a reducer
//! that shrinks a failing program further than proptest can.
//!
//! [`programs`] draws a recipe and interprets it into a [`Program`]: one to
//! four inputs, of integers or booleans, some coalescing; five to forty
//! definitions; the observed nodes; and one to ten transactions, each
//! sending to a random subset of the inputs, a non-coalescing input at most
//! once and a coalescing one up to three times. Proptest shrinks the
//! recipe: fewer steps, fewer sends, smaller values, simpler choices.
//!
//! Each step of the recipe adds a definition, or a few. It picks its
//! operands among what exists, most recent first, and adds a source when
//! nothing fits, so every step makes progress and every program is well
//! typed and linear: a linear stream is consumed once, and only a `Share`
//! is consumed more often. The steps make diamonds on purpose: two paths
//! from one shared stream that meet in a merge, an `or_else` or a lift of
//! two holds, and two read-through cells of one cell that meet in a lift.
//! Merge and coalescing functions favour ones that are not commutative, so
//! `f(left, right)` shows which side is which, and filters pass some events
//! and drop others. Steps views never read a `State`, which has none.
//!
//! The recipe observes a random subset of the nodes that can be observed:
//! every cell, every shared stream, and every linear stream nothing
//! consumes; the builder gives a chain a `node` to listen to.
//!
//! [`reduce`] takes a failing program and a test for failure, and cuts what
//! it can while the test still fails: dead definitions, transactions,
//! definitions with everything that reads them, inputs nothing reads,
//! observations, sends and coalescing functions. It bypasses identity
//! nodes, moves a reference or an observation to an ancestor, which strands
//! what was between, and simplifies expressions and sent values. Every
//! program it keeps passes [`build::check`].

use proptest::array::uniform6;
use proptest::collection::vec;
use proptest::prelude::*;

use crate::build::{self, Scalar};
use crate::program::{Definition, Expression, Input, Program, Reference, Type, Value, Window};

/// At most this many definitions.
pub const MAX_DEFINITIONS: usize = 40;

/// How many observation choices a recipe carries; candidates past this
/// reuse them.
const OBSERVE_CHOICES: usize = 48;

/// The kinds of step, each adding a definition or a small pattern of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Input,
    InputCell,
    Constant,
    Never,
    Share,
    Node,
    Map,
    Filter,
    FilterMap,
    MapTo,
    Snapshot,
    Gate,
    Once,
    Merge,
    OrElse,
    Hold,
    Accumulate,
    AccumulateMut,
    Scan,
    Steps,
    StepsWithCurrent,
    MapCell,
    ToBoolean,
    Lift,
    StreamDiamond,
    CellDiamond,
}

/// One step of a recipe: its kind and the choices it makes.
#[derive(Clone, Debug)]
struct Step {
    kind: Kind,
    /// Operands and small choices, each taken modulo what it chooses among.
    picks: [u32; 6],
    /// Which template a function follows.
    template: u8,
    /// A random expression, for the functions that use one.
    tree: Expression,
    /// A small literal, for initial values and constants.
    literal: i64,
}

#[derive(Clone, Debug)]
struct InputRecipe {
    boolean: bool,
    coalesce: Option<(u8, Expression)>,
}

#[derive(Clone, Debug)]
struct Recipe {
    inputs: Vec<InputRecipe>,
    steps: Vec<Step>,
    observe: Vec<bool>,
    schedule: Vec<Vec<(usize, i64)>>,
}

fn kind() -> impl Strategy<Value = Kind> {
    prop_oneof![
        10 => Just(Kind::Input),
        6 => Just(Kind::InputCell),
        2 => Just(Kind::Constant),
        1 => Just(Kind::Never),
        8 => Just(Kind::Share),
        3 => Just(Kind::Node),
        6 => Just(Kind::Map),
        5 => Just(Kind::Filter),
        3 => Just(Kind::FilterMap),
        2 => Just(Kind::MapTo),
        5 => Just(Kind::Snapshot),
        4 => Just(Kind::Gate),
        3 => Just(Kind::Once),
        6 => Just(Kind::Merge),
        3 => Just(Kind::OrElse),
        5 => Just(Kind::Hold),
        3 => Just(Kind::Accumulate),
        3 => Just(Kind::AccumulateMut),
        3 => Just(Kind::Scan),
        3 => Just(Kind::Steps),
        3 => Just(Kind::StepsWithCurrent),
        4 => Just(Kind::MapCell),
        2 => Just(Kind::ToBoolean),
        5 => Just(Kind::Lift),
        4 => Just(Kind::StreamDiamond),
        3 => Just(Kind::CellDiamond),
    ]
}

/// A random expression over arguments 0 to 5 and small literals, at most
/// three deep. [`specialize`] fits it to an arity.
fn tree() -> impl Strategy<Value = Expression> {
    let leaf = prop_oneof![
        3 => (0_usize..6).prop_map(Expression::ArgumentAt),
        2 => (-3_i64..=9).prop_map(Expression::Literal),
    ];
    leaf.prop_recursive(3, 16, 3, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| a + b),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| a - b),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| a * b),
            (inner.clone(), 1_i64..=7).prop_map(|(a, divisor)| a.modulo(divisor)),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| a.maximum(b)),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| a.minimum(b)),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| a.equal(b)),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| a.less_than(b)),
            inner.clone().prop_map(|a| !a),
            (inner.clone(), inner.clone(), inner)
                .prop_map(|(c, a, b)| Expression::if_then_else(c, a, b)),
        ]
    })
}

fn step() -> impl Strategy<Value = Step> {
    (
        kind(),
        uniform6(any::<u32>()),
        any::<u8>(),
        tree(),
        -3_i64..=6,
    )
        .prop_map(|(kind, picks, template, tree, literal)| Step {
            kind,
            picks,
            template,
            tree,
            literal,
        })
}

fn input_recipe() -> impl Strategy<Value = InputRecipe> {
    (
        prop::bool::weighted(0.2),
        prop::option::weighted(0.35, (any::<u8>(), tree())),
    )
        .prop_map(|(boolean, coalesce)| InputRecipe { boolean, coalesce })
}

/// Random programs of the subset, window `FromFirstTransaction`.
pub fn programs() -> impl Strategy<Value = Program> {
    (
        vec(input_recipe(), 1..=4),
        vec(step(), 5..=MAX_DEFINITIONS),
        vec(prop::bool::weighted(0.7), OBSERVE_CHOICES),
        vec(vec((0_usize..8, -9_i64..=9), 0..=6), 1..=10),
    )
        .prop_map(|(inputs, steps, observe, schedule)| {
            Recipe {
                inputs,
                steps,
                observe,
                schedule,
            }
            .program()
        })
}

// ----- expressions -----

/// Fits a random expression to an arity: argument i becomes the only
/// argument, alternately the first and the second, argument i modulo the
/// arity, or the literal i where there are none.
fn specialize(expression: &Expression, arity: usize) -> Expression {
    let go = |e: &Expression| Box::new(specialize(e, arity));
    match expression {
        Expression::ArgumentAt(index) => match arity {
            0 => Expression::Literal(*index as i64),
            1 => Expression::Argument,
            2 if index % 2 == 0 => Expression::Argument,
            2 => Expression::SecondArgument,
            n => Expression::ArgumentAt(index % n),
        },
        Expression::Add(a, b) => Expression::Add(go(a), go(b)),
        Expression::Subtract(a, b) => Expression::Subtract(go(a), go(b)),
        Expression::Multiply(a, b) => Expression::Multiply(go(a), go(b)),
        Expression::Modulo(a, divisor) => Expression::Modulo(go(a), *divisor),
        Expression::Maximum(a, b) => Expression::Maximum(go(a), go(b)),
        Expression::Minimum(a, b) => Expression::Minimum(go(a), go(b)),
        Expression::Equal(a, b) => Expression::Equal(go(a), go(b)),
        Expression::LessThan(a, b) => Expression::LessThan(go(a), go(b)),
        Expression::Not(a) => Expression::Not(go(a)),
        Expression::If(c, a, b) => Expression::If(go(c), go(a), go(b)),
        leaf => leaf.clone(),
    }
}

/// The direct sub-expressions.
fn children(expression: &Expression) -> Vec<&Expression> {
    match expression {
        Expression::Add(a, b)
        | Expression::Subtract(a, b)
        | Expression::Multiply(a, b)
        | Expression::Maximum(a, b)
        | Expression::Minimum(a, b)
        | Expression::Equal(a, b)
        | Expression::LessThan(a, b) => vec![a, b],
        Expression::Modulo(a, _) | Expression::Not(a) => vec![a],
        Expression::If(c, a, b) => vec![c, a, b],
        _ => Vec::new(),
    }
}

fn mentions(expression: &Expression, leaf: &Expression) -> bool {
    expression == leaf || children(expression).into_iter().any(|e| mentions(e, leaf))
}

fn multiplies(expression: &Expression) -> bool {
    matches!(expression, Expression::Multiply(..))
        || children(expression).into_iter().any(multiplies)
}

fn literal(value: i64) -> Expression {
    Expression::Literal(value)
}

use Expression::{Argument, SecondArgument};

impl Step {
    fn pick(&self, index: usize) -> usize {
        self.picks[index % 6] as usize
    }

    /// A function of the event: a template, or the random tree made to
    /// read the event.
    fn unary(&self) -> Expression {
        let k = (self.pick(5) % 4) as i64 + 2;
        match self.template % 5 {
            0 => Argument + literal(self.literal),
            1 => Argument * literal(k),
            2 => Argument.modulo(k),
            3 => literal(self.literal) - Argument,
            _ => {
                let e = specialize(&self.tree, 1);
                if mentions(&e, &Argument) {
                    e
                } else {
                    Argument + e
                }
            }
        }
    }

    /// A predicate that passes some events and drops others.
    fn predicate(&self) -> Expression {
        let k = (self.pick(5) % 3) as i64 + 2;
        let j = (self.pick(4) % k as usize) as i64;
        match self.template % 4 {
            0 => Argument.modulo(k).less_than(literal(j + 1)),
            1 => Argument.less_than(literal(self.literal)),
            2 => !Argument.modulo(2).equal(literal(0)),
            _ => specialize(&self.tree, 1),
        }
    }

    /// A function of two arguments that reads both.
    fn binary(&self) -> Expression {
        match self.template % 4 {
            0 => Argument + SecondArgument,
            1 => Argument * literal(10) + SecondArgument,
            2 => SecondArgument - Argument,
            _ => both(specialize(&self.tree, 2)),
        }
    }

    /// A merge or coalescing function, mostly not commutative.
    fn combine(&self) -> Expression {
        combine(self.template, &self.tree)
    }

    /// An accumulator's step, the event first and the state second, kept
    /// small where it multiplies.
    fn accumulator(&self) -> Expression {
        let e = match (self.template / 4) % 5 {
            0 => SecondArgument + Argument,
            1 => (SecondArgument * literal(3) + Argument).modulo(1000),
            2 => SecondArgument - Argument,
            3 => SecondArgument.maximum(Argument),
            _ => both(specialize(&self.tree, 2)),
        };
        if multiplies(&e) && !matches!(e, Expression::Modulo(..)) {
            e.modulo(1000)
        } else {
            e
        }
    }

    /// A lift's function: the arguments folded left, each with an operation
    /// of its own, so it reads every cell.
    fn lift(&self, arity: usize) -> Expression {
        let mut e = Expression::ArgumentAt(0);
        for index in 1..arity {
            let argument = Expression::ArgumentAt(index);
            e = match (self.picks[5] >> (3 * index)) % 5 {
                0 => e + argument,
                1 => e - argument,
                2 => e * literal(10) + argument,
                3 => e.maximum(argument),
                _ => e.minimum(argument.clone()) * literal(2) + argument,
            };
        }
        e
    }
}

/// A function of two arguments, made to read both.
fn both(e: Expression) -> Expression {
    let e = if mentions(&e, &Argument) {
        e
    } else {
        Argument + e
    };
    if mentions(&e, &SecondArgument) {
        e
    } else {
        e + SecondArgument * literal(100)
    }
}

/// A merge or coalescing function: the left argument first.
fn combine(template: u8, tree: &Expression) -> Expression {
    match template % 4 {
        0 => Argument - SecondArgument,
        1 => Argument * literal(10) + SecondArgument,
        2 => Expression::if_then_else(
            Argument.less_than(SecondArgument),
            SecondArgument - Argument,
            Argument * literal(2),
        ),
        _ => both(specialize(tree, 2)),
    }
}

// ----- interpreting a recipe -----

/// What a node of a program under construction is.
#[derive(Clone, Copy, Debug)]
enum Slot {
    Stream {
        scalar: Scalar,
        shared: bool,
        consumed: bool,
    },
    Cell {
        scalar: Scalar,
        state: bool,
    },
}

/// A program under construction.
#[derive(Clone)]
struct Draft {
    inputs: Vec<Input>,
    scalars: Vec<Scalar>,
    definitions: Vec<Definition>,
    slots: Vec<Slot>,
}

fn top(node: usize) -> Reference {
    Reference::TopLevel(node)
}

impl Draft {
    fn room(&self, definitions: usize) -> bool {
        self.definitions.len() + definitions <= MAX_DEFINITIONS
    }

    fn push(&mut self, definition: Definition, slot: Slot) -> usize {
        self.definitions.push(definition);
        self.slots.push(slot);
        self.definitions.len() - 1
    }

    fn push_stream(&mut self, definition: Definition, scalar: Scalar) -> usize {
        let shared = matches!(definition, Definition::Share(_));
        self.push(
            definition,
            Slot::Stream {
                scalar,
                shared,
                consumed: false,
            },
        )
    }

    fn push_cell(&mut self, definition: Definition, scalar: Scalar, state: bool) -> usize {
        self.push(definition, Slot::Cell { scalar, state })
    }

    fn scalar(&self, node: usize) -> Scalar {
        match self.slots[node] {
            Slot::Stream { scalar, .. } | Slot::Cell { scalar, .. } => scalar,
        }
    }

    fn state(&self, node: usize) -> bool {
        matches!(self.slots[node], Slot::Cell { state: true, .. })
    }

    /// The streams a definition may consume, of one scalar or any.
    fn streams(&self, scalar: Option<Scalar>) -> Vec<usize> {
        (0..self.slots.len())
            .filter(|&node| match self.slots[node] {
                Slot::Stream {
                    scalar: s,
                    shared,
                    consumed,
                } => (shared || !consumed) && scalar.is_none_or(|want| want == s),
                Slot::Cell { .. } => false,
            })
            .collect()
    }

    fn cells(&self, scalar: Option<Scalar>, state: bool) -> Vec<usize> {
        (0..self.slots.len())
            .filter(|&node| match self.slots[node] {
                Slot::Cell {
                    scalar: s,
                    state: st,
                } => scalar.is_none_or(|want| want == s) && (state || !st),
                Slot::Stream { .. } => false,
            })
            .collect()
    }

    /// Marks a linear stream consumed.
    fn consume(&mut self, node: usize) {
        if let Slot::Stream {
            shared: false,
            consumed,
            ..
        } = &mut self.slots[node]
        {
            *consumed = true;
        }
    }

    /// The stream of an input.
    fn input_stream(&mut self, choice: usize) -> usize {
        let k = choice % self.inputs.len();
        self.push_stream(Definition::Input(k), self.scalars[k])
    }

    fn input_cell(&mut self, choice: usize, initial: Expression) -> usize {
        let k = choice % self.inputs.len();
        self.push_cell(
            Definition::InputCell { input: k, initial },
            self.scalars[k],
            false,
        )
    }

    /// A stream to consume, most recent first; an input's stream when none
    /// is left. Marks it consumed.
    fn stream(&mut self, choice: usize, scalar: Option<Scalar>) -> usize {
        let candidates = self.streams(scalar);
        let node = match recent(&candidates, choice) {
            Some(node) => node,
            None => {
                let matching: Vec<usize> = (0..self.inputs.len())
                    .filter(|&k| scalar.is_none_or(|want| want == self.scalars[k]))
                    .collect();
                match recent(&matching, choice) {
                    Some(k) => self.push_stream(Definition::Input(k), self.scalars[k]),
                    None => self.input_stream(choice),
                }
            }
        };
        self.consume(node);
        node
    }

    /// A stream of one scalar to consume: [`Draft::stream`], converted when
    /// its fallback input carries the other scalar.
    fn stream_of(&mut self, choice: usize, scalar: Scalar) -> usize {
        let node = self.stream(choice, Some(scalar));
        if self.scalar(node) == scalar {
            return node;
        }
        let converted = match scalar {
            Scalar::Integer => Definition::Map {
                function: Argument,
                source: top(node),
            },
            Scalar::Boolean => Definition::MapTo {
                value: Value::Boolean(choice % 2 == 0),
                source: top(node),
            },
        };
        let converted = self.push_stream(converted, scalar);
        self.consume(converted);
        converted
    }

    /// A cell to read, most recent first; an input cell when none fits.
    fn cell(&mut self, choice: usize, scalar: Option<Scalar>, state: bool) -> usize {
        let candidates = self.cells(scalar, state);
        match recent(&candidates, choice) {
            Some(node) => node,
            None => {
                let node = self.input_cell(choice, literal(0));
                match scalar {
                    Some(want) if want != self.scalar(node) => self.convert_cell(node, want),
                    _ => node,
                }
            }
        }
    }

    /// A cell of the other scalar over a cell: `ToBoolean`, or `MapCell`.
    fn convert_cell(&mut self, node: usize, scalar: Scalar) -> usize {
        let state = self.state(node);
        match scalar {
            Scalar::Boolean => {
                self.push_cell(Definition::ToBoolean(top(node)), Scalar::Boolean, state)
            }
            Scalar::Integer => self.push_cell(
                Definition::MapCell {
                    function: Argument,
                    cell: top(node),
                },
                Scalar::Integer,
                state,
            ),
        }
    }

    /// An initial value: a literal, and now and then plus a sample of a
    /// cell, read in the build closure.
    fn initial(&self, step: &Step) -> Expression {
        let cells = self.cells(None, true);
        match recent(&cells, step.pick(3)) {
            Some(node) if step.pick(5) % 8 == 0 => {
                Expression::Sample(top(node)) + literal(step.literal)
            }
            _ => literal(step.literal),
        }
    }

    /// One adapter over a stream, as a step or one path of a diamond. It
    /// consumes the stream.
    fn adapter(&mut self, kind: Kind, step: &Step, source: usize) -> usize {
        self.consume(source);
        let scalar = self.scalar(source);
        match kind {
            Kind::Map => self.push_stream(
                Definition::Map {
                    function: step.unary(),
                    source: top(source),
                },
                Scalar::Integer,
            ),
            Kind::Filter => self.push_stream(
                Definition::Filter {
                    predicate: step.predicate(),
                    source: top(source),
                },
                scalar,
            ),
            Kind::FilterMap => self.push_stream(
                Definition::FilterMap {
                    keep: step.predicate(),
                    function: step.unary(),
                    source: top(source),
                },
                Scalar::Integer,
            ),
            Kind::MapTo => {
                let (value, scalar) = if step.pick(1) % 3 == 0 {
                    (Value::Boolean(step.literal % 2 != 0), Scalar::Boolean)
                } else {
                    (Value::Integer(step.literal), Scalar::Integer)
                };
                self.push_stream(
                    Definition::MapTo {
                        value,
                        source: top(source),
                    },
                    scalar,
                )
            }
            Kind::Snapshot => {
                let cell = self.cell(step.pick(1), None, true);
                self.push_stream(
                    Definition::Snapshot {
                        function: step.binary(),
                        source: top(source),
                        cell: top(cell),
                    },
                    Scalar::Integer,
                )
            }
            Kind::Gate => {
                let cell = self.cell(step.pick(1), Some(Scalar::Boolean), true);
                self.push_stream(
                    Definition::Gate {
                        source: top(source),
                        cell: top(cell),
                    },
                    scalar,
                )
            }
            _ => self.push_stream(Definition::Once(top(source)), scalar),
        }
    }

    fn step(&mut self, step: &Step) {
        match step.kind {
            Kind::Input => {
                self.input_stream(step.pick(0));
            }
            Kind::InputCell => {
                let initial = self.initial(step);
                self.input_cell(step.pick(0), initial);
            }
            Kind::Constant => {
                let value = self.initial(step);
                self.push_cell(Definition::Constant(value), Scalar::Integer, false);
            }
            Kind::Never => {
                let scalar = if step.pick(0) % 3 == 0 {
                    Scalar::Boolean
                } else {
                    Scalar::Integer
                };
                let event_type = match scalar {
                    Scalar::Integer => Type::Integer,
                    Scalar::Boolean => Type::Boolean,
                };
                self.push_stream(Definition::Never(event_type), scalar);
            }
            Kind::Share | Kind::Node => {
                let source = self.stream(step.pick(0), None);
                let scalar = self.scalar(source);
                let definition = if step.kind == Kind::Share {
                    Definition::Share(top(source))
                } else {
                    Definition::Node(top(source))
                };
                self.push_stream(definition, scalar);
            }
            Kind::Map
            | Kind::Filter
            | Kind::FilterMap
            | Kind::MapTo
            | Kind::Snapshot
            | Kind::Gate
            | Kind::Once => {
                let source = self.stream(step.pick(0), None);
                self.adapter(step.kind, step, source);
            }
            Kind::Merge | Kind::OrElse => {
                let left = self.stream(step.pick(0), None);
                let scalar = self.scalar(left);
                let right = self.stream_of(step.pick(1), scalar);
                let definition = if step.kind == Kind::Merge {
                    Definition::Merge {
                        function: step.combine(),
                        left: top(left),
                        right: top(right),
                    }
                } else {
                    Definition::OrElse {
                        left: top(left),
                        right: top(right),
                    }
                };
                self.push_stream(definition, scalar);
            }
            Kind::Hold => {
                let initial = self.initial(step);
                let source = self.stream(step.pick(0), None);
                let scalar = self.scalar(source);
                self.push_cell(
                    Definition::Hold {
                        initial,
                        source: top(source),
                    },
                    scalar,
                    false,
                );
            }
            Kind::Accumulate | Kind::AccumulateMut => {
                let initial = self.initial(step);
                let source = self.stream(step.pick(0), None);
                let (initial, function, source) = (initial, step.accumulator(), top(source));
                let state = step.kind == Kind::AccumulateMut;
                let definition = if state {
                    Definition::AccumulateMut {
                        initial,
                        function,
                        source,
                    }
                } else {
                    Definition::Accumulate {
                        initial,
                        function,
                        source,
                    }
                };
                self.push_cell(definition, Scalar::Integer, state);
            }
            Kind::Scan => {
                let initial = self.initial(step);
                let source = self.stream(step.pick(0), None);
                self.push_stream(
                    Definition::Scan {
                        initial,
                        output: step.binary(),
                        state: step.accumulator(),
                        source: top(source),
                    },
                    Scalar::Integer,
                );
            }
            Kind::Steps | Kind::StepsWithCurrent => {
                let cell = self.cell(step.pick(0), None, false);
                let scalar = self.scalar(cell);
                let definition = if step.kind == Kind::Steps {
                    Definition::Steps(top(cell))
                } else {
                    Definition::StepsWithCurrent(top(cell))
                };
                self.push_stream(definition, scalar);
            }
            Kind::MapCell => {
                let cell = self.cell(step.pick(0), None, true);
                let state = self.state(cell);
                self.push_cell(
                    Definition::MapCell {
                        function: step.unary(),
                        cell: top(cell),
                    },
                    Scalar::Integer,
                    state,
                );
            }
            Kind::ToBoolean => {
                let cell = self.cell(step.pick(0), Some(Scalar::Integer), true);
                self.convert_cell(cell, Scalar::Boolean);
            }
            Kind::Lift => {
                let arity = step.template as usize % 5 + 2;
                let cells: Vec<usize> = (0..arity)
                    .map(|position| self.cell(step.pick(position), None, true))
                    .collect();
                self.lift(step, &cells);
            }
            Kind::StreamDiamond => self.stream_diamond(step),
            Kind::CellDiamond => self.cell_diamond(step),
        }
    }

    fn lift(&mut self, step: &Step, cells: &[usize]) -> usize {
        let state = cells.iter().any(|&cell| self.state(cell));
        self.push_cell(
            Definition::Lift {
                function: step.lift(cells.len()),
                cells: cells.iter().map(|&cell| top(cell)).collect(),
            },
            Scalar::Integer,
            state,
        )
    }

    /// Two paths from one shared stream, meeting in a merge, an `or_else`,
    /// or a lift of a hold of each.
    fn stream_diamond(&mut self, step: &Step) {
        let source = self.stream(step.pick(0), None);
        let shared = match self.slots[source] {
            Slot::Stream { shared: true, .. } => source,
            _ => self.push_stream(Definition::Share(top(source)), self.scalar(source)),
        };
        const PATHS: [Kind; 7] = [
            Kind::Map,
            Kind::Filter,
            Kind::Snapshot,
            Kind::Gate,
            Kind::Once,
            Kind::FilterMap,
            Kind::Map,
        ];
        let first = self.adapter(PATHS[step.pick(1) % PATHS.len()], step, shared);
        let mut second = self.adapter(PATHS[step.pick(2) % PATHS.len()], step, shared);
        if step.pick(3) % 3 == 0 {
            second = self.adapter(PATHS[step.pick(4) % PATHS.len()], step, second);
        }
        let join = step.pick(3) % 5;
        if join < 3 && self.scalar(first) != self.scalar(second) {
            // A merge needs one type: the boolean path reads as integers.
            if self.scalar(first) == Scalar::Boolean {
                self.consume(first);
                let mapped = self.adapter(Kind::Map, step, first);
                return self.join(step, join, mapped, second);
            }
            self.consume(second);
            let mapped = self.adapter(Kind::Map, step, second);
            return self.join(step, join, first, mapped);
        }
        self.join(step, join, first, second);
    }

    fn join(&mut self, step: &Step, join: usize, first: usize, second: usize) {
        self.consume(first);
        self.consume(second);
        match join {
            0 | 1 => {
                self.push_stream(
                    Definition::Merge {
                        function: step.combine(),
                        left: top(first),
                        right: top(second),
                    },
                    self.scalar(first),
                );
            }
            2 => {
                self.push_stream(
                    Definition::OrElse {
                        left: top(first),
                        right: top(second),
                    },
                    self.scalar(first),
                );
            }
            _ => {
                let first = self.push_cell(
                    Definition::Hold {
                        initial: literal(step.literal),
                        source: top(first),
                    },
                    self.scalar(first),
                    false,
                );
                let second = self.push_cell(
                    Definition::Accumulate {
                        initial: literal(0),
                        function: step.accumulator(),
                        source: top(second),
                    },
                    Scalar::Integer,
                    false,
                );
                self.lift(step, &[first, second]);
            }
        }
    }

    /// Two read-through cells of one cell, meeting in a lift, now and then
    /// with the cell itself.
    fn cell_diamond(&mut self, step: &Step) {
        let cell = self.cell(step.pick(0), None, true);
        let state = self.state(cell);
        let first = self.push_cell(
            Definition::MapCell {
                function: step.unary(),
                cell: top(cell),
            },
            Scalar::Integer,
            state,
        );
        let second = if step.pick(1) % 2 == 0 {
            self.push_cell(
                Definition::MapCell {
                    function: Argument * literal(3) - literal(1),
                    cell: top(cell),
                },
                Scalar::Integer,
                state,
            )
        } else {
            self.push_cell(Definition::ToBoolean(top(cell)), Scalar::Boolean, state)
        };
        if step.pick(2) % 3 == 0 {
            self.lift(step, &[first, cell, second]);
        } else {
            self.lift(step, &[first, second]);
        }
    }

    /// The nodes that can be observed: every cell, every shared stream, and
    /// every linear stream nothing consumed.
    fn observable(&self) -> Vec<usize> {
        (0..self.slots.len())
            .filter(|&node| match self.slots[node] {
                Slot::Cell { .. } => true,
                Slot::Stream {
                    shared, consumed, ..
                } => shared || !consumed,
            })
            .collect()
    }
}

/// The candidate `choice` counts back from the most recent.
fn recent(candidates: &[usize], choice: usize) -> Option<usize> {
    (!candidates.is_empty()).then(|| candidates[candidates.len() - 1 - choice % candidates.len()])
}

impl Recipe {
    fn program(&self) -> Program {
        let inputs: Vec<Input> = self
            .inputs
            .iter()
            .map(|input| {
                let event_type = if input.boolean {
                    Type::Boolean
                } else {
                    Type::Integer
                };
                match &input.coalesce {
                    None => Input::new(event_type),
                    Some((template, tree)) => {
                        Input::coalescing(event_type, combine(*template, tree))
                    }
                }
            })
            .collect();
        let scalars = self
            .inputs
            .iter()
            .map(|input| {
                if input.boolean {
                    Scalar::Boolean
                } else {
                    Scalar::Integer
                }
            })
            .collect();
        let mut draft = Draft {
            inputs,
            scalars,
            definitions: Vec::new(),
            slots: Vec::new(),
        };
        for step in &self.steps {
            let mut attempt = draft.clone();
            attempt.step(step);
            if attempt.definitions.len() <= MAX_DEFINITIONS {
                draft = attempt;
            } else if draft.room(1) {
                draft.input_stream(step.pick(0));
            }
        }
        let candidates = draft.observable();
        let mut observe: Vec<usize> = candidates
            .iter()
            .enumerate()
            .filter(|(position, _)| self.observe[position % OBSERVE_CHOICES])
            .map(|(_, &node)| node)
            .collect();
        if observe.is_empty() {
            observe.extend(candidates.last());
        }
        let schedule = self
            .schedule
            .iter()
            .map(|sends| {
                let mut counts = vec![0_usize; draft.inputs.len()];
                sends
                    .iter()
                    .filter_map(|&(input, value)| {
                        let k = input % draft.inputs.len();
                        let limit = if draft.inputs[k].coalesce.is_some() {
                            3
                        } else {
                            1
                        };
                        (counts[k] < limit).then(|| {
                            counts[k] += 1;
                            let value = match draft.scalars[k] {
                                Scalar::Integer => Value::Integer(value),
                                Scalar::Boolean => Value::Boolean(value.rem_euclid(2) == 1),
                            };
                            (k, value)
                        })
                    })
                    .collect()
            })
            .collect();
        Program {
            window: Window::FromFirstTransaction,
            inputs: draft.inputs,
            definitions: draft.definitions,
            observe,
            schedule,
        }
    }
}

// ----- reducing a failing program -----

/// Every top-level node a definition reads: its streams, its cells, and
/// the cells its expressions sample.
pub fn references(definition: &Definition) -> Vec<usize> {
    let mut nodes = Vec::new();
    let mut add = |reference: &Reference| {
        if let Reference::TopLevel(node) = reference {
            nodes.push(*node);
        }
    };
    let mut expressions: Vec<&Expression> = Vec::new();
    match definition {
        Definition::InputCell { initial, .. } => expressions.push(initial),
        Definition::Constant(value) => expressions.push(value),
        Definition::Map { function, source } => {
            add(source);
            expressions.push(function);
        }
        Definition::Filter { predicate, source } => {
            add(source);
            expressions.push(predicate);
        }
        Definition::FilterMap {
            keep,
            function,
            source,
        } => {
            add(source);
            expressions.extend([keep, function]);
        }
        Definition::MapTo { source, .. }
        | Definition::Once(source)
        | Definition::Node(source)
        | Definition::Share(source) => add(source),
        Definition::Snapshot {
            function,
            source,
            cell,
        } => {
            add(source);
            add(cell);
            expressions.push(function);
        }
        Definition::Gate { source, cell } => {
            add(source);
            add(cell);
        }
        Definition::Merge {
            function,
            left,
            right,
        } => {
            add(left);
            add(right);
            expressions.push(function);
        }
        Definition::OrElse { left, right } => {
            add(left);
            add(right);
        }
        Definition::Scan {
            initial,
            output,
            state,
            source,
        } => {
            add(source);
            expressions.extend([initial, output, state]);
        }
        Definition::Steps(cell)
        | Definition::StepsWithCurrent(cell)
        | Definition::ToBoolean(cell) => add(cell),
        Definition::Hold { initial, source } => {
            add(source);
            expressions.push(initial);
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
            add(source);
            expressions.extend([initial, function]);
        }
        Definition::MapCell { function, cell } => {
            add(cell);
            expressions.push(function);
        }
        Definition::Lift { function, cells } => {
            cells.iter().for_each(&mut add);
            expressions.push(function);
        }
        _ => {}
    }
    for expression in expressions {
        samples(expression, &mut nodes);
    }
    nodes
}

fn samples(expression: &Expression, nodes: &mut Vec<usize>) {
    if let Expression::Sample(Reference::TopLevel(node)) = expression {
        nodes.push(*node);
    }
    for child in children(expression) {
        samples(child, nodes);
    }
}

/// Renames every top-level reference, in definitions and expressions.
fn rename(definition: &Definition, map: &dyn Fn(usize) -> usize) -> Definition {
    let r = |reference: &Reference| match reference {
        Reference::TopLevel(node) => Reference::TopLevel(map(*node)),
        local => *local,
    };
    let e = |expression: &Expression| rename_expression(expression, map);
    match definition {
        Definition::InputCell { input, initial } => Definition::InputCell {
            input: *input,
            initial: e(initial),
        },
        Definition::Constant(value) => Definition::Constant(e(value)),
        Definition::Map { function, source } => Definition::Map {
            function: e(function),
            source: r(source),
        },
        Definition::Filter { predicate, source } => Definition::Filter {
            predicate: e(predicate),
            source: r(source),
        },
        Definition::FilterMap {
            keep,
            function,
            source,
        } => Definition::FilterMap {
            keep: e(keep),
            function: e(function),
            source: r(source),
        },
        Definition::MapTo { value, source } => Definition::MapTo {
            value: value.clone(),
            source: r(source),
        },
        Definition::Snapshot {
            function,
            source,
            cell,
        } => Definition::Snapshot {
            function: e(function),
            source: r(source),
            cell: r(cell),
        },
        Definition::Gate { source, cell } => Definition::Gate {
            source: r(source),
            cell: r(cell),
        },
        Definition::Once(source) => Definition::Once(r(source)),
        Definition::Node(source) => Definition::Node(r(source)),
        Definition::Share(source) => Definition::Share(r(source)),
        Definition::Merge {
            function,
            left,
            right,
        } => Definition::Merge {
            function: e(function),
            left: r(left),
            right: r(right),
        },
        Definition::OrElse { left, right } => Definition::OrElse {
            left: r(left),
            right: r(right),
        },
        Definition::Scan {
            initial,
            output,
            state,
            source,
        } => Definition::Scan {
            initial: e(initial),
            output: e(output),
            state: e(state),
            source: r(source),
        },
        Definition::Steps(cell) => Definition::Steps(r(cell)),
        Definition::StepsWithCurrent(cell) => Definition::StepsWithCurrent(r(cell)),
        Definition::Hold { initial, source } => Definition::Hold {
            initial: e(initial),
            source: r(source),
        },
        Definition::Accumulate {
            initial,
            function,
            source,
        } => Definition::Accumulate {
            initial: e(initial),
            function: e(function),
            source: r(source),
        },
        Definition::AccumulateMut {
            initial,
            function,
            source,
        } => Definition::AccumulateMut {
            initial: e(initial),
            function: e(function),
            source: r(source),
        },
        Definition::MapCell { function, cell } => Definition::MapCell {
            function: e(function),
            cell: r(cell),
        },
        Definition::ToBoolean(cell) => Definition::ToBoolean(r(cell)),
        Definition::Lift { function, cells } => Definition::Lift {
            function: e(function),
            cells: cells.iter().map(r).collect(),
        },
        other => other.clone(),
    }
}

fn rename_expression(expression: &Expression, map: &dyn Fn(usize) -> usize) -> Expression {
    let go = |e: &Expression| Box::new(rename_expression(e, map));
    match expression {
        Expression::Sample(Reference::TopLevel(node)) => {
            Expression::Sample(Reference::TopLevel(map(*node)))
        }
        Expression::Add(a, b) => Expression::Add(go(a), go(b)),
        Expression::Subtract(a, b) => Expression::Subtract(go(a), go(b)),
        Expression::Multiply(a, b) => Expression::Multiply(go(a), go(b)),
        Expression::Modulo(a, divisor) => Expression::Modulo(go(a), *divisor),
        Expression::Maximum(a, b) => Expression::Maximum(go(a), go(b)),
        Expression::Minimum(a, b) => Expression::Minimum(go(a), go(b)),
        Expression::Equal(a, b) => Expression::Equal(go(a), go(b)),
        Expression::LessThan(a, b) => Expression::LessThan(go(a), go(b)),
        Expression::Not(a) => Expression::Not(go(a)),
        Expression::If(c, a, b) => Expression::If(go(c), go(a), go(b)),
        leaf => leaf.clone(),
    }
}

/// The program without the given definitions and everything that reads
/// them, renumbered; `None` if nothing observed is left.
fn without(program: &Program, remove: &[usize]) -> Option<Program> {
    let mut gone = vec![false; program.definitions.len()];
    for &node in remove {
        gone[node] = true;
    }
    for (index, definition) in program.definitions.iter().enumerate() {
        if references(definition).iter().any(|&node| gone[node]) {
            gone[index] = true;
        }
    }
    let mut new_index = vec![usize::MAX; gone.len()];
    let mut next = 0;
    for (index, removed) in gone.iter().enumerate() {
        if !removed {
            new_index[index] = next;
            next += 1;
        }
    }
    let map = |node: usize| new_index[node];
    let definitions = program
        .definitions
        .iter()
        .enumerate()
        .filter(|(index, _)| !gone[*index])
        .map(|(_, definition)| rename(definition, &map))
        .collect();
    let observe: Vec<usize> = program
        .observe
        .iter()
        .filter(|&&node| !gone[node])
        .map(|&node| new_index[node])
        .collect();
    (!observe.is_empty()).then(|| Program {
        definitions,
        observe,
        ..program.clone()
    })
}

/// The program with the identity-like node `node` bypassed: what read it
/// reads its source.
fn bypassed(program: &Program, node: usize) -> Option<Program> {
    let source = match &program.definitions[node] {
        Definition::Node(Reference::TopLevel(source))
        | Definition::Share(Reference::TopLevel(source))
        | Definition::Once(Reference::TopLevel(source))
        | Definition::Filter {
            source: Reference::TopLevel(source),
            ..
        }
        | Definition::Gate {
            source: Reference::TopLevel(source),
            ..
        } => *source,
        _ => return None,
    };
    let map = |n: usize| if n == node { source } else { n };
    let mut bypassed = program.clone();
    for definition in bypassed.definitions.iter_mut().skip(node + 1) {
        *definition = rename(definition, &map);
    }
    for observed in &mut bypassed.observe {
        *observed = map(*observed);
    }
    bypassed.observe.dedup();
    without(&bypassed, &[node])
}

/// The program without the definitions no observed node reads.
fn live(program: &Program) -> Program {
    let mut read = vec![false; program.definitions.len()];
    for &node in &program.observe {
        read[node] = true;
        for ancestor in ancestors(program, node) {
            read[ancestor] = true;
        }
    }
    let dead: Vec<usize> = (0..read.len()).filter(|&node| !read[node]).collect();
    without(program, &dead).unwrap_or_else(|| program.clone())
}

/// Every node `node` reads, directly or through others.
fn ancestors(program: &Program, node: usize) -> Vec<usize> {
    let mut seen = vec![false; program.definitions.len()];
    let mut stack = references(&program.definitions[node]);
    let mut found = Vec::new();
    while let Some(next) = stack.pop() {
        if !seen[next] {
            seen[next] = true;
            found.push(next);
            stack.extend(references(&program.definitions[next]));
        }
    }
    found.sort_unstable();
    found
}

/// Both streams of one scalar, or both cells of one.
fn same_type(a: build::NodeType, b: build::NodeType) -> bool {
    use build::NodeType::{Cell, Stream};
    match (a, b) {
        (Stream(a), Stream(b)) => a == b,
        (Cell { value: a, .. }, Cell { value: b, .. }) => a == b,
        _ => false,
    }
}

/// The program without input `k`, which nothing reads, and its sends.
fn without_input(program: &Program, k: usize) -> Program {
    let renumber = |i: usize| if i > k { i - 1 } else { i };
    let mut smaller = program.clone();
    smaller.inputs.remove(k);
    for definition in &mut smaller.definitions {
        match definition {
            Definition::Input(i) | Definition::InputCell { input: i, .. } => *i = renumber(*i),
            _ => {}
        }
    }
    for sends in &mut smaller.schedule {
        sends.retain(|(i, _)| *i != k);
        for (i, _) in sends.iter_mut() {
            *i = renumber(*i);
        }
    }
    smaller
}

/// Simpler forms of an expression: each sub-expression, and small literals.
fn simpler(expression: &Expression) -> Vec<Expression> {
    let mut forms: Vec<Expression> = children(expression).into_iter().cloned().collect();
    if !matches!(expression, Expression::Literal(0 | 1)) {
        forms.push(Expression::Literal(0));
        forms.push(Expression::Literal(1));
    }
    forms
}

/// Every expression of a definition, mutably.
fn expressions_mut(definition: &mut Definition) -> Vec<&mut Expression> {
    match definition {
        Definition::InputCell { initial, .. } => vec![initial],
        Definition::Constant(value) => vec![value],
        Definition::Map { function, .. } | Definition::MapCell { function, .. } => vec![function],
        Definition::Filter { predicate, .. } => vec![predicate],
        Definition::FilterMap { keep, function, .. } => vec![keep, function],
        Definition::Snapshot { function, .. }
        | Definition::Merge { function, .. }
        | Definition::Lift { function, .. } => vec![function],
        Definition::Scan {
            initial,
            output,
            state,
            ..
        } => vec![initial, output, state],
        Definition::Hold { initial, .. } => vec![initial],
        Definition::Accumulate {
            initial, function, ..
        }
        | Definition::AccumulateMut {
            initial, function, ..
        } => vec![initial, function],
        _ => Vec::new(),
    }
}

/// Smaller programs than `program`, the biggest cuts first. Each is a valid
/// program if `program` is, except where [`build::check`] says otherwise,
/// which [`reduce`] asks.
fn candidates(program: &Program) -> Vec<Program> {
    let mut smaller = Vec::new();
    // What no observed node reads.
    let live = live(program);
    if live.definitions.len() < program.definitions.len() {
        smaller.push(live);
    }
    // Whole transactions, the last first.
    for k in (0..program.schedule.len()).rev() {
        let mut p = program.clone();
        p.schedule.remove(k);
        smaller.push(p);
    }
    // Definitions and everything that reads them, the last first.
    for node in (0..program.definitions.len()).rev() {
        smaller.extend(without(program, &[node]));
    }
    // Identity-like nodes bypassed.
    for node in 0..program.definitions.len() {
        smaller.extend(bypassed(program, node));
    }
    // References moved upstream, to an ancestor of the same type, which
    // strands what was between.
    if let Ok(types) = build::check(program) {
        for node in 0..program.definitions.len() {
            let mut targets = references(&program.definitions[node]);
            targets.sort_unstable();
            targets.dedup();
            for target in targets {
                for ancestor in ancestors(program, target) {
                    if same_type(types[ancestor], types[target]) {
                        let mut p = program.clone();
                        let map = |n: usize| if n == target { ancestor } else { n };
                        p.definitions[node] = rename(&program.definitions[node], &map);
                        smaller.push(p);
                    }
                }
            }
        }
    }
    // An observation moved to an ancestor, and what is then dead dropped.
    for position in 0..program.observe.len() {
        for ancestor in ancestors(program, program.observe[position]) {
            let mut p = program.clone();
            p.observe[position] = ancestor;
            smaller.push(self::live(&p));
        }
    }
    // Observations.
    if program.observe.len() > 1 {
        for position in 0..program.observe.len() {
            let mut p = program.clone();
            p.observe.remove(position);
            smaller.push(p);
        }
    }
    // Sends.
    for k in 0..program.schedule.len() {
        for position in 0..program.schedule[k].len() {
            let mut p = program.clone();
            p.schedule[k].remove(position);
            smaller.push(p);
        }
    }
    // Inputs no definition reads, with their sends.
    for k in (0..program.inputs.len()).rev() {
        let read = program.definitions.iter().any(|definition| {
            matches!(definition, Definition::Input(i) | Definition::InputCell { input: i, .. } if *i == k)
        });
        if !read {
            smaller.push(without_input(program, k));
        }
    }
    // Coalescing functions dropped where no transaction needs one.
    for (k, input) in program.inputs.iter().enumerate() {
        let needs = program
            .schedule
            .iter()
            .any(|sends| sends.iter().filter(|(i, _)| *i == k).count() > 1);
        if input.coalesce.is_some() && !needs {
            let mut p = program.clone();
            p.inputs[k].coalesce = None;
            smaller.push(p);
        }
    }
    // Parts of expressions.
    for node in 0..program.definitions.len() {
        let count = expressions_mut(&mut program.definitions[node].clone()).len();
        for which in 0..count {
            let mut definition = program.definitions[node].clone();
            let forms = simpler(expressions_mut(&mut definition)[which]);
            for form in forms {
                let mut p = program.clone();
                *expressions_mut(&mut p.definitions[node])[which] = form;
                smaller.push(p);
            }
        }
    }
    // Sent values toward zero.
    for k in 0..program.schedule.len() {
        for position in 0..program.schedule[k].len() {
            if let Value::Integer(value) = program.schedule[k][position].1 {
                if value != 0 {
                    let mut p = program.clone();
                    p.schedule[k][position].1 = Value::Integer(value / 2);
                    smaller.push(p);
                }
            }
        }
    }
    smaller
}

/// Shrinks a program for which `fails` holds, as far as removing and
/// simplifying keeps it failing, and returns the smallest found.
pub fn reduce(program: &Program, mut fails: impl FnMut(&Program) -> bool) -> Program {
    let mut best = program.clone();
    'shrink: loop {
        for candidate in candidates(&best) {
            if build::check(&candidate).is_ok() && fails(&candidate) {
                best = candidate;
                continue 'shrink;
            }
        }
        return best;
    }
}
