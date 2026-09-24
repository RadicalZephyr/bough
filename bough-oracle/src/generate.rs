//! Random well-typed programs in the subset of stages 1 to 5, and a reducer
//! that shrinks a failing program further than proptest can.
//!
//! [`programs`] draws a recipe and interprets it into a [`Program`]: one to
//! four inputs, of integers or booleans, some coalescing; five to forty
//! steps, each adding a definition or a small pattern of them, up to
//! [`MAX_DEFINITIONS`]; the observed nodes; and one to ten transactions,
//! each sending to a random subset of the inputs, a non-coalescing input at
//! most once and a coalescing one up to three times. Proptest shrinks the
//! recipe: fewer steps, fewer sends, smaller values, simpler choices.
//!
//! Each step picks its operands among what exists, most recent first, and
//! adds a source when nothing fits, so every step makes progress and every
//! program is well typed and linear: a linear stream is consumed once, and
//! only a `Share` is consumed more often. The steps make diamonds on
//! purpose: two paths from one shared stream that meet in a merge, an
//! `or_else` or a lift of two holds, and two read-through cells of one cell
//! that meet in a lift. Merge and coalescing functions favour ones that are
//! not commutative, so `f(left, right)` shows which side is which, and
//! filters pass some events and drop others. Steps views never read a
//! `State`, which has none.
//!
//! # Loops
//!
//! A step may declare a cell loop, of integers or booleans, meant to close
//! with a `Cell` or with a `State`, or a stream loop; any later step may use
//! the forward like any node; a `Close` step closes an open loop; and the
//! loops still open when the steps run out are closed at the end. Only
//! well-founded loops are made, by construction. The generator keeps, for
//! every node, the open loops it depends on ([`Reach`]): every edge counts
//! but a snapshot's or a gate's read of a cell and an expression's
//! `Sample`, which read a value from before the instant. A loop closes only
//! with a definition that does not depend on it. So a forward is read,
//! upstream of its own definition, only as the cell of a snapshot or a
//! gate, and after its `Close` anything may use it. The edges of a split and
//! a defer count too, though the engine's rule does not count them: a loop
//! through one ends only if something ends it (finding F22), so only the
//! patterns below make one, each with a guard that does.
//!
//! Patterns make the shapes RFD 1 asks for from the first version: a
//! counter, sometimes capped by its own value; two loops that read each
//! other through snapshots, lifted or merged downstream; a loop cell lifted
//! with a cell upstream of itself, the sodium-rust#52 shape; a stream loop
//! read through a hold and a snapshot; and loops through children, a stream
//! loop through a defer, a cell loop through a defer of its steps, and a
//! stream loop through a split. Their guard keeps `(x mod m) - k`, with
//! `k >= 1`, and only while it is positive, so every value around the loop
//! is below the one before, and the loop ends. [`well_founded`] checks that
//! shape wherever a cycle passes through a split or a defer.
//!
//! # Child transactions
//!
//! A split step maps a stream to lists of length 0 to 3 and splits them; a
//! defer step defers a stream; and a diamond of children shares a stream
//! between two splits, or a split and a defer, which fire in one instant and
//! share child indices, and merges them downstream.
//!
//! # Switches
//!
//! A switch step makes the tokens to switch among, an outer that selects
//! among them, and the switch. A `switch_stream` switches among two to four
//! shared streams, a `switch_cell` among two to four cells of integers, or
//! `State`s, which gives a `State`. The outer is mostly a hold of a pick,
//! whose selector is a stream the step takes, now and then deferred or
//! split so that the switch moves in child instants; a map_cell of a cell,
//! which steps whenever that cell does; or a constant, which never
//! switches. One switch step in four takes its selector and its tokens from
//! one shared stream, so that it switches in an instant in which both the
//! new inner and the old one fire. Nested switches switch among switches: a
//! `switch_cell` of `switch_cell`s, and a `switch_stream` among shared
//! `switch_stream`s. A switch loop is a stream loop through a
//! `switch_stream`'s selection, which is legal because the selection is
//! read before the instant (finding F14), or a cell loop read by a snapshot
//! that selects for a switch whose output closes it. Later steps use a
//! switch's output like any node: a steps view of a `switch_cell`, a share
//! of a `switch_stream`, which a later switch may follow.
//!
//! A `switch_cell` depends on its outer; a `switch_stream`'s outer is only
//! watched (finding F14), so a loop through its selection is legal. A
//! switch depends on the cell or stream it follows, which is known only at
//! run time, so for [`Reach`] and [`well_founded`] every token its outer
//! may select is a dependency of the switch. A token downstream of the
//! switch through a loop would be a same-instant cycle when selected, and
//! no generated program has one. This is conservative: two switches that
//! could each select a cell depending on the other, but never both at once,
//! make a legal program (finding F46), which the generator never makes.
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
//! program it keeps passes [`build::check`], and, if the one it started
//! from is [`well_founded`], is well founded too.

use proptest::array::uniform6;
use proptest::collection::vec;
use proptest::prelude::*;

use crate::build::{self, NodeType, Scalar};
use crate::program::{Definition, Expression, Input, Program, Reference, Type, Value, Window};

/// At most this many definitions.
pub const MAX_DEFINITIONS: usize = 64;

/// How many definitions closing a loop may add, at most: a source, a
/// snapshot, a filter, a cell, a conversion to booleans, and the `Close`.
/// A step is taken only if every loop it leaves open can still close.
const CLOSE_ROOM: usize = 6;

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
    // Loops.
    CellLoop,
    StreamLoop,
    Close,
    Counter,
    LoopDiamond,
    Sodium52,
    RunningTotal,
    // Child transactions, and loops through them.
    Split,
    Defer,
    ChildDiamond,
    Countdown,
    CellCountdown,
    SplitLoop,
    // Switches.
    SwitchStream,
    SwitchCell,
    SwitchState,
    NestedSwitch,
    SwitchLoop,
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
    /// A program of stages 1 and 2 alone: no loop and no child transaction.
    plain: bool,
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
        3 => Just(Kind::CellLoop),
        2 => Just(Kind::StreamLoop),
        5 => Just(Kind::Close),
        3 => Just(Kind::Counter),
        3 => Just(Kind::LoopDiamond),
        2 => Just(Kind::Sodium52),
        2 => Just(Kind::RunningTotal),
        5 => Just(Kind::Split),
        4 => Just(Kind::Defer),
        3 => Just(Kind::ChildDiamond),
        3 => Just(Kind::Countdown),
        2 => Just(Kind::CellCountdown),
        2 => Just(Kind::SplitLoop),
        7 => Just(Kind::SwitchStream),
        7 => Just(Kind::SwitchCell),
        4 => Just(Kind::SwitchState),
        4 => Just(Kind::NestedSwitch),
        4 => Just(Kind::SwitchLoop),
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

/// Random programs of the subset, window `FromFirstTransaction`. About one
/// in seven is a program of stages 1 and 2 alone, and most of the others
/// have loops and child transactions.
pub fn programs() -> impl Strategy<Value = Program> {
    (
        vec(input_recipe(), 1..=4),
        vec(step(), 5..=40),
        vec(prop::bool::weighted(0.7), OBSERVE_CHOICES),
        vec(vec((0_usize..8, -9_i64..=9), 0..=6), 1..=10),
        prop::bool::weighted(0.15),
    )
        .prop_map(|(inputs, steps, observe, schedule, plain)| {
            Recipe {
                inputs,
                steps,
                observe,
                schedule,
                plain,
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

/// A guard's map, before a defer: the event `mod m`, less `k`.
pub fn guard_map(modulus: i64, k: i64) -> Expression {
    Argument.modulo(modulus) - literal(k)
}

/// A guard's list element i, before a split: the event `mod m`, less `k`,
/// less i.
pub fn guard_element(modulus: i64, k: i64) -> Expression {
    Argument.modulo(modulus) - literal(k) - SecondArgument
}

/// A guard's filter: keeps a positive event.
pub fn guard_filter() -> Expression {
    literal(0).less_than(Argument)
}

/// `Sub (Mod Arg m) (Lit k)` with `k >= 1`, and its `m`.
fn guard_map_modulus(expression: &Expression) -> Option<i64> {
    match expression {
        Expression::Subtract(value, k) => match (&**value, &**k) {
            (Expression::Modulo(argument, modulus), Expression::Literal(k))
                if **argument == Argument && *k >= 1 =>
            {
                Some(*modulus)
            }
            _ => None,
        },
        _ => None,
    }
}

fn is_guard_map(expression: &Expression) -> bool {
    guard_map_modulus(expression).is_some()
}

fn is_guard_element(expression: &Expression) -> bool {
    match expression {
        Expression::Subtract(value, index) => {
            **index == SecondArgument && guard_map_modulus(value).is_some()
        }
        _ => false,
    }
}

fn is_guard_filter(expression: &Expression) -> bool {
    *expression == guard_filter()
}

/// The kinds of stages 1 and 2, which a plain program's steps take instead
/// of a loop or a child transaction.
const PLAIN: [Kind; 26] = [
    Kind::Input,
    Kind::InputCell,
    Kind::Constant,
    Kind::Never,
    Kind::Share,
    Kind::Node,
    Kind::Map,
    Kind::Filter,
    Kind::FilterMap,
    Kind::MapTo,
    Kind::Snapshot,
    Kind::Gate,
    Kind::Once,
    Kind::Merge,
    Kind::OrElse,
    Kind::Hold,
    Kind::Accumulate,
    Kind::AccumulateMut,
    Kind::Scan,
    Kind::Steps,
    Kind::StepsWithCurrent,
    Kind::MapCell,
    Kind::ToBoolean,
    Kind::Lift,
    Kind::StreamDiamond,
    Kind::CellDiamond,
];

impl Step {
    fn pick(&self, index: usize) -> usize {
        self.picks[index % 6] as usize
    }

    /// The step as a plain program takes it: a kind of stages 1 and 2 in
    /// place of a loop or a child transaction.
    fn plain(&self) -> Step {
        let mut step = self.clone();
        if !PLAIN.contains(&step.kind) {
            step.kind = PLAIN[self.template as usize % PLAIN.len()];
        }
        step
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

    /// A list's length, which the semantics take `mod` 4: zero to three
    /// elements. `which` tells two lists of one step apart.
    fn length(&self, which: usize) -> Expression {
        match (self.pick(3 + which) >> 4) % 4 {
            0 => Argument.modulo(4),
            1 => literal((self.pick(3 + which) % 4) as i64),
            2 => Argument,
            _ => specialize(&self.tree, 1),
        }
    }

    /// A list's element i, from the event and i.
    fn element(&self, which: usize) -> Expression {
        match (self.pick(4 + which) >> 4) % 4 {
            0 => Argument * literal(10) + SecondArgument,
            1 => Argument + SecondArgument,
            2 => SecondArgument - Argument,
            _ => both(specialize(&self.tree, 2)),
        }
    }

    /// A guard's modulus and decrement: 3 to 7, and 1 or 2.
    fn guard(&self) -> (i64, i64) {
        (
            3 + (self.pick(4) % 5) as i64,
            1 + ((self.pick(5) >> 8) % 2) as i64,
        )
    }

    /// A pick's index, which the semantics take `mod` the number of
    /// choices: mostly the event itself, which the inputs' small values make
    /// visit every choice, or the random tree made to read the event.
    fn index(&self) -> Expression {
        match (self.pick(5) >> 12) % 6 {
            0..=2 => Argument,
            3 => Argument + literal(self.literal),
            4 => literal(self.literal) - Argument * literal(2),
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

    /// The same step with its choices moved, for one of several parts of a
    /// pattern that each take a step's choices.
    fn varied(&self, part: u32) -> Step {
        let mut step = self.clone();
        step.picks.rotate_left(part as usize % 6);
        for pick in &mut step.picks {
            *pick = pick.rotate_left(7 * part);
        }
        step.template = step.template.rotate_left(3 * part);
        step.literal = (step.literal + i64::from(part)).rem_euclid(10) - 3;
        step
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
    /// A `MapList`'s lists, consumed at once by the split made with it.
    Lists,
    Cell {
        scalar: Scalar,
        state: bool,
    },
    /// A pick's tokens, consumed at once by the hold made with it.
    Tokens,
    /// A cell of tokens, which only a switch reads.
    Outer,
    /// A `Close`.
    Closed,
}

/// What a node has of the open loops: a bit per loop, by the loop's node.
#[derive(Clone, Copy, Debug, Default)]
struct Reach {
    /// The open loops the node depends on. A dependency is any edge but a
    /// read of a cell's value from before the instant: a snapshot's or a
    /// gate's cell, and an expression's `Sample`. A split's and a defer's
    /// edges count, as they do not for the engine, except a pattern's
    /// guarded one. A loop closes only with a definition that does not
    /// depend on it.
    depends: u128,
    /// The open loops whose values reach the node by any edge: a
    /// definition that reads its own loop is one worth closing it with.
    reads: u128,
}

/// A program under construction.
#[derive(Clone)]
struct Draft {
    inputs: Vec<Input>,
    scalars: Vec<Scalar>,
    definitions: Vec<Definition>,
    slots: Vec<Slot>,
    reach: Vec<Reach>,
    /// The loops declared and not closed yet, oldest first.
    open: Vec<usize>,
}

fn top(node: usize) -> Reference {
    Reference::TopLevel(node)
}

fn bit(node: usize) -> u128 {
    1 << node
}

fn type_of(scalar: Scalar) -> Type {
    match scalar {
        Scalar::Integer => Type::Integer,
        Scalar::Boolean => Type::Boolean,
    }
}

/// The top-level nodes a definition depends on, as [`Reach`] counts them:
/// what it consumes and the cells it reads through, but not a snapshot's
/// or a gate's cell or a `Sample`. A `switch_cell` depends on its outer, and
/// a switch on every token its outer may select, among the `definitions`
/// before it; a `switch_stream`'s outer is read before the instant, and is
/// no dependency.
fn dependencies(definitions: &[Definition], definition: &Definition) -> Vec<usize> {
    let mut nodes = Vec::new();
    let mut add = |reference: &Reference| {
        if let Reference::TopLevel(node) = reference {
            nodes.push(*node);
        }
    };
    let candidates = |outer: &Reference| match outer {
        Reference::TopLevel(outer) => build::switch_candidates(definitions, *outer),
        Reference::Local(_) => Vec::new(),
    };
    match definition {
        Definition::PickStream { source, .. }
        | Definition::PickCell { source, .. }
        | Definition::HoldStream { source, .. }
        | Definition::HoldCell { source, .. } => add(source),
        Definition::MapPickCell { cell, .. } => add(cell),
        Definition::SwitchCell(outer) => {
            add(outer);
            candidates(outer).iter().for_each(add);
        }
        Definition::SwitchStream(outer) => candidates(outer).iter().for_each(add),
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
        | Definition::MapList { source, .. } => add(source),
        Definition::Once(source)
        | Definition::Node(source)
        | Definition::Share(source)
        | Definition::Split(source)
        | Definition::Defer(source)
        | Definition::Steps(source)
        | Definition::StepsWithCurrent(source)
        | Definition::ToBoolean(source) => add(source),
        Definition::MapCell { cell, .. } => add(cell),
        Definition::Merge { left, right, .. } | Definition::OrElse { left, right } => {
            add(left);
            add(right);
        }
        Definition::Lift { cells, .. } => cells.iter().for_each(add),
        _ => {}
    }
    nodes
}

impl Draft {
    /// Whether the draft keeps room for `definitions` more and for closing
    /// every open loop.
    fn room(&self, definitions: usize) -> bool {
        self.definitions.len() + definitions + CLOSE_ROOM * self.open.len() <= MAX_DEFINITIONS
    }

    /// Adds a definition, consuming the linear streams it consumes.
    fn push(&mut self, definition: Definition, slot: Slot) -> usize {
        let index = self.definitions.len();
        let reach = if build::is_loop(&definition) {
            Reach {
                depends: bit(index),
                reads: bit(index),
            }
        } else {
            let mut reach = Reach::default();
            for node in dependencies(&self.definitions, &definition) {
                reach.depends |= self.reach[node].depends;
            }
            for node in references(&definition) {
                reach.reads |= self.reach[node].reads;
            }
            reach
        };
        for reference in build::consumed_streams(&definition) {
            if let Reference::TopLevel(node) = reference {
                self.consume(node);
            }
        }
        self.definitions.push(definition);
        self.slots.push(slot);
        self.reach.push(reach);
        index
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
            Slot::Lists | Slot::Tokens | Slot::Outer | Slot::Closed => {
                unreachable!("bough-oracle: node {node} has no scalar")
            }
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
                _ => false,
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
                _ => false,
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

    /// A stream to consume, most recent first, that depends on none of the
    /// loops in `avoid`; an input's stream when none is left. Marks it
    /// consumed.
    fn stream_avoiding(&mut self, choice: usize, scalar: Option<Scalar>, avoid: u128) -> usize {
        let candidates: Vec<usize> = self
            .streams(scalar)
            .into_iter()
            .filter(|&node| self.reach[node].depends & avoid == 0)
            .collect();
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

    /// A stream to consume, most recent first; an input's stream when none
    /// is left. Marks it consumed.
    fn stream(&mut self, choice: usize, scalar: Option<Scalar>) -> usize {
        self.stream_avoiding(choice, scalar, 0)
    }

    /// A stream of one scalar to consume: [`Draft::stream_avoiding`],
    /// converted when its fallback input carries the other scalar.
    fn stream_of_avoiding(&mut self, choice: usize, scalar: Scalar, avoid: u128) -> usize {
        let node = self.stream_avoiding(choice, Some(scalar), avoid);
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

    fn stream_of(&mut self, choice: usize, scalar: Scalar) -> usize {
        self.stream_of_avoiding(choice, scalar, 0)
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
    /// cell, read in the build closure. Only a cell that depends on no open
    /// loop is sampled, since a forward has no value before its `Close`.
    fn initial(&self, step: &Step) -> Expression {
        let cells: Vec<usize> = self
            .cells(None, true)
            .into_iter()
            .filter(|&cell| self.reach[cell].depends == 0)
            .collect();
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
                self.push_stream(Definition::Never(type_of(scalar)), scalar);
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
            Kind::CellLoop => {
                let scalar = if step.pick(0) % 4 == 0 {
                    Scalar::Boolean
                } else {
                    Scalar::Integer
                };
                self.declare_cell_loop(scalar, step.pick(1) % 3 == 0);
            }
            Kind::StreamLoop => {
                self.declare_stream_loop();
            }
            Kind::Close => match recent(&self.open, step.pick(0)) {
                Some(forward) => self.close_loop(forward, step),
                None => self.counter(step),
            },
            Kind::Counter => self.counter(step),
            Kind::LoopDiamond => self.loop_diamond(step),
            Kind::Sodium52 => self.sodium_52(step),
            Kind::RunningTotal => self.running_total(step),
            Kind::Split => {
                let source = self.stream(step.pick(0), None);
                self.split(step, 0, source);
            }
            Kind::Defer => {
                let source = self.stream(step.pick(0), None);
                let scalar = self.scalar(source);
                self.push_stream(Definition::Defer(top(source)), scalar);
            }
            Kind::ChildDiamond => self.child_diamond(step),
            Kind::Countdown => self.countdown(step),
            Kind::CellCountdown => self.cell_countdown(step),
            Kind::SplitLoop => self.split_loop(step),
            Kind::SwitchStream => {
                let switch = self.switch_stream(step, 0);
                if step.pick(4) % 3 == 0 {
                    self.push_stream(Definition::Share(top(switch)), self.scalar(switch));
                }
            }
            Kind::SwitchCell | Kind::SwitchState => {
                let state = step.kind == Kind::SwitchState;
                let switch = self.switch_cell(step, state, 0);
                if !state {
                    match step.pick(4) % 5 {
                        0 => {
                            self.push_stream(Definition::Steps(top(switch)), Scalar::Integer);
                        }
                        1 => {
                            self.push_stream(
                                Definition::StepsWithCurrent(top(switch)),
                                Scalar::Integer,
                            );
                        }
                        _ => {}
                    }
                }
            }
            Kind::NestedSwitch => self.nested_switch(step),
            Kind::SwitchLoop => self.switch_loop(step),
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
        let shared = self.shared(source);
        let first = self.adapter(PATHS[step.pick(1) % PATHS.len()], step, shared);
        let mut second = self.adapter(PATHS[step.pick(2) % PATHS.len()], step, shared);
        if step.pick(3) % 3 == 0 {
            second = self.adapter(PATHS[step.pick(4) % PATHS.len()], step, second);
        }
        self.join(step, step.pick(3) % 5, first, second);
    }

    /// The stream itself if it is shared, or a share of it.
    fn shared(&mut self, source: usize) -> usize {
        match self.slots[source] {
            Slot::Stream { shared: true, .. } => source,
            _ => self.push_stream(Definition::Share(top(source)), self.scalar(source)),
        }
    }

    /// Joins two streams: a merge (`join` 0 or 1), an `or_else` (2), or a
    /// lift of a hold of one and an accumulator of the other (3 and up). A
    /// boolean stream is read as integers where the other is not boolean.
    fn join(&mut self, step: &Step, join: usize, first: usize, second: usize) {
        if join < 3 && self.scalar(first) != self.scalar(second) {
            // A merge needs one type: the boolean path reads as integers.
            if self.scalar(first) == Scalar::Boolean {
                let mapped = self.adapter(Kind::Map, step, first);
                return self.join(step, join, mapped, second);
            }
            let mapped = self.adapter(Kind::Map, step, second);
            return self.join(step, join, first, mapped);
        }
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

    // ----- loops -----

    /// Declares a cell loop, meant to close with a `State` or not.
    fn declare_cell_loop(&mut self, scalar: Scalar, state: bool) -> usize {
        let node = self.push_cell(Definition::CellLoop(type_of(scalar)), scalar, state);
        self.open.push(node);
        node
    }

    /// Declares a stream loop of integers.
    fn declare_stream_loop(&mut self) -> usize {
        let node = self.push_stream(Definition::StreamLoop(Type::Integer), Scalar::Integer);
        self.open.push(node);
        node
    }

    /// Closes a loop with a definition that does not depend on it. Every
    /// node that depended on the loop now depends on what the definition
    /// depends on, and so for what reads it.
    fn close(&mut self, forward: usize, definition: usize) {
        let closing = bit(forward);
        let reach = self.reach[definition];
        assert!(
            reach.depends & closing == 0,
            "bough-oracle: a loop closes only with a definition that does not depend on it"
        );
        if let (Slot::Cell { state: meant, .. }, Slot::Cell { state, .. }) =
            (self.slots[forward], self.slots[definition])
        {
            assert_eq!(
                meant, state,
                "bough-oracle: a cell loop closes with a definition of the kind it was meant to be"
            );
        }
        self.push(
            Definition::Close {
                forward,
                definition: top(definition),
            },
            Slot::Closed,
        );
        for r in &mut self.reach {
            if r.depends & closing != 0 {
                r.depends = (r.depends & !closing) | reach.depends;
            }
            if r.reads & closing != 0 {
                r.reads = (r.reads & !closing) | reach.reads;
            }
        }
        self.open.retain(|&node| node != forward);
    }

    /// A split or a defer made by a pattern with a guard: its output no
    /// longer depends on the loop the guard ends.
    fn guarded(&mut self, node: usize, forward: usize) {
        self.reach[node].depends &= !bit(forward);
    }

    /// Closes an open loop: with the most recent node that fits it, reads
    /// it and does not depend on it, or with a definition made for it. A
    /// cell loop meant to be a `State` closes with a `State`, and one meant
    /// to be a `Cell` with a `Cell`, so that what the draft says it is holds
    /// from its declaration: a switch lists the loop among `State`s or
    /// among `Cell`s before it closes.
    fn close_loop(&mut self, forward: usize, step: &Step) {
        let closing = bit(forward);
        let (want, cell) = match self.slots[forward] {
            Slot::Cell { scalar, state } => ((scalar, state), true),
            Slot::Stream { scalar, .. } => ((scalar, false), false),
            _ => unreachable!("bough-oracle: node {forward} is a loop"),
        };
        let candidates: Vec<usize> = (forward + 1..self.slots.len())
            .filter(|&node| {
                let fits = match self.slots[node] {
                    Slot::Cell { scalar, state } => cell && scalar == want.0 && want.1 == state,
                    Slot::Stream {
                        scalar,
                        shared,
                        consumed,
                    } => !cell && scalar == want.0 && (shared || !consumed),
                    _ => false,
                };
                fits && self.reach[node].depends & closing == 0
                    && self.reach[node].reads & closing != 0
            })
            .collect();
        let definition = match recent(&candidates, step.pick(2)) {
            Some(node) => node,
            None if cell => self.cell_definition(step, forward),
            None => self.stream_definition(step, forward),
        };
        self.close(forward, definition);
    }

    /// A definition for a cell loop that reads the loop through a snapshot,
    /// and now and then through a filter that caps it: a hold or an
    /// accumulator of a stream that does not depend on the loop, a `State`
    /// for a loop meant to be one, converted for a loop of booleans.
    fn cell_definition(&mut self, step: &Step, forward: usize) -> usize {
        let (scalar, state) = match self.slots[forward] {
            Slot::Cell { scalar, state } => (scalar, state),
            _ => unreachable!("bough-oracle: node {forward} is a cell loop"),
        };
        let initial = self.initial(step);
        let source = self.stream_avoiding(step.pick(3), None, bit(forward));
        let read = self.push_stream(
            Definition::Snapshot {
                function: step.binary(),
                source: top(source),
                cell: top(forward),
            },
            Scalar::Integer,
        );
        let chain = if step.pick(4) % 3 == 0 {
            // The capped counter: the loop's own value stops it.
            self.push_stream(
                Definition::Filter {
                    predicate: Argument.less_than(literal(5 + step.literal)),
                    source: top(read),
                },
                Scalar::Integer,
            )
        } else {
            read
        };
        let cell = if state {
            self.push_cell(
                Definition::AccumulateMut {
                    initial,
                    function: step.accumulator(),
                    source: top(chain),
                },
                Scalar::Integer,
                true,
            )
        } else if step.pick(5) % 2 == 0 {
            self.push_cell(
                Definition::Hold {
                    initial,
                    source: top(chain),
                },
                Scalar::Integer,
                false,
            )
        } else {
            self.push_cell(
                Definition::Accumulate {
                    initial,
                    function: step.accumulator(),
                    source: top(chain),
                },
                Scalar::Integer,
                false,
            )
        };
        match scalar {
            Scalar::Boolean => self.convert_cell(cell, Scalar::Boolean),
            Scalar::Integer => cell,
        }
    }

    /// A definition for a stream loop that reads the loop before the
    /// instant: a snapshot, of a stream that does not depend on the loop,
    /// of a cell the loop's events reach, a hold of the forward when nothing
    /// consumed it yet; or, when no cell reads the loop, just that stream.
    /// Now and then a forward nothing consumed is left so, for a listener.
    fn stream_definition(&mut self, step: &Step, forward: usize) -> usize {
        let closing = bit(forward);
        let cell = match self.slots[forward] {
            Slot::Stream {
                consumed: false, ..
            } if step.pick(2) % 3 == 0 => None,
            Slot::Stream {
                consumed: false, ..
            } => {
                let initial = self.initial(step);
                Some(self.push_cell(
                    Definition::Hold {
                        initial,
                        source: top(forward),
                    },
                    Scalar::Integer,
                    false,
                ))
            }
            _ => {
                let cells: Vec<usize> = (forward + 1..self.slots.len())
                    .filter(|&node| {
                        matches!(self.slots[node], Slot::Cell { .. })
                            && self.reach[node].reads & closing != 0
                    })
                    .collect();
                recent(&cells, step.pick(2))
            }
        };
        let source = self.stream_of_avoiding(step.pick(3), Scalar::Integer, closing);
        match cell {
            Some(cell) => self.push_stream(
                Definition::Snapshot {
                    function: step.binary(),
                    source: top(source),
                    cell: top(cell),
                },
                Scalar::Integer,
            ),
            None => source,
        }
    }

    /// A counter: a cell loop closed at once with a definition that reads
    /// it through a snapshot, now and then capped by its own value. After
    /// its `Close` anything may use the forward: now and then a steps view
    /// or a read-through cell of it.
    fn counter(&mut self, step: &Step) {
        let scalar = if step.pick(0) % 4 == 0 {
            Scalar::Boolean
        } else {
            Scalar::Integer
        };
        let state = step.pick(1) % 4 == 0;
        let forward = self.declare_cell_loop(scalar, state);
        let definition = self.cell_definition(step, forward);
        self.close(forward, definition);
        match step.pick(2) % 4 {
            0 if !state => {
                self.push_stream(Definition::Steps(top(forward)), scalar);
            }
            1 => {
                self.push_cell(
                    Definition::MapCell {
                        function: step.unary(),
                        cell: top(forward),
                    },
                    Scalar::Integer,
                    state,
                );
            }
            _ => {}
        }
    }

    /// Two loops that read each other through snapshots on one shared
    /// stream, so both step in one instant, each reading the other's value
    /// from before it; then joined downstream: a lift of the two forwards,
    /// a merge of their steps, or a lift of the definitions with a hold of
    /// the shared stream, upstream of both.
    fn loop_diamond(&mut self, step: &Step) {
        let a = self.declare_cell_loop(Scalar::Integer, false);
        let b_state = step.pick(0) % 4 == 0;
        let b = self.declare_cell_loop(Scalar::Integer, b_state);
        let source = self.stream(step.pick(1), None);
        let shared = self.shared(source);
        let initial = self.initial(step);
        let reads_b = self.push_stream(
            Definition::Snapshot {
                function: step.binary(),
                source: top(shared),
                cell: top(b),
            },
            Scalar::Integer,
        );
        let da = self.push_cell(
            Definition::Hold {
                initial,
                source: top(reads_b),
            },
            Scalar::Integer,
            false,
        );
        let reads_a = self.push_stream(
            Definition::Snapshot {
                function: step.combine(),
                source: top(shared),
                cell: top(a),
            },
            Scalar::Integer,
        );
        let accumulate = if b_state {
            Definition::AccumulateMut {
                initial: literal(step.literal),
                function: step.accumulator(),
                source: top(reads_a),
            }
        } else {
            Definition::Accumulate {
                initial: literal(step.literal),
                function: step.accumulator(),
                source: top(reads_a),
            }
        };
        let db = self.push_cell(accumulate, Scalar::Integer, b_state);
        self.close(a, da);
        self.close(b, db);
        match step.pick(2) % 3 {
            0 => {
                self.lift(step, &[a, b]);
            }
            1 if !b_state => {
                let steps_a = self.push_stream(Definition::Steps(top(a)), Scalar::Integer);
                let steps_b = self.push_stream(Definition::Steps(top(b)), Scalar::Integer);
                self.push_stream(
                    Definition::Merge {
                        function: step.combine(),
                        left: top(steps_a),
                        right: top(steps_b),
                    },
                    Scalar::Integer,
                );
            }
            _ => {
                let scalar = self.scalar(shared);
                let level = self.push_cell(
                    Definition::Hold {
                        initial: literal(1),
                        source: top(shared),
                    },
                    scalar,
                    false,
                );
                self.lift(step, &[da, db, level]);
            }
        }
    }

    /// The sodium-rust#52 shape: health, a loop, clamps its own value plus
    /// a merge of two streams by a maximum read before the instant, a cell
    /// upstream of health, itself a loop now and then; and a lift reads
    /// health and the maximum together.
    fn sodium_52(&mut self, step: &Step) {
        let maximum = if step.pick(0) % 2 == 0 {
            let forward = self.declare_cell_loop(Scalar::Integer, false);
            let level = self.stream(step.pick(1), None);
            let raised = self.push_stream(
                Definition::Snapshot {
                    function: SecondArgument + Argument,
                    source: top(level),
                    cell: top(forward),
                },
                Scalar::Integer,
            );
            let held = self.push_cell(
                Definition::Hold {
                    initial: literal(100),
                    source: top(raised),
                },
                Scalar::Integer,
                false,
            );
            self.close(forward, held);
            if step.pick(1) % 2 == 0 { forward } else { held }
        } else {
            self.cell(step.pick(1), Some(Scalar::Integer), true)
        };
        let health = self.declare_cell_loop(Scalar::Integer, false);
        let heal = self.stream(step.pick(2), None);
        let scalar = self.scalar(heal);
        let damage = self.stream_of(step.pick(3), scalar);
        let delta = self.push_stream(
            Definition::Merge {
                function: step.combine(),
                left: top(heal),
                right: top(damage),
            },
            scalar,
        );
        let moved = self.push_stream(
            Definition::Snapshot {
                function: Argument + SecondArgument,
                source: top(delta),
                cell: top(health),
            },
            Scalar::Integer,
        );
        let clamped = self.push_stream(
            Definition::Snapshot {
                function: Argument.minimum(SecondArgument).maximum(literal(0)),
                source: top(moved),
                cell: top(maximum),
            },
            Scalar::Integer,
        );
        let held = self.push_cell(
            Definition::Hold {
                initial: literal(60),
                source: top(clamped),
            },
            Scalar::Integer,
            false,
        );
        self.close(health, held);
        let reading = if step.pick(4) % 2 == 0 { health } else { held };
        if step.pick(5) % 2 == 0 {
            self.lift(step, &[maximum, reading]);
        } else {
            self.lift(step, &[reading, maximum]);
        }
    }

    /// A stream loop read through a hold or an accumulator of its forward
    /// and a snapshot of that: a running total, which needs no children.
    fn running_total(&mut self, step: &Step) {
        let forward = self.declare_stream_loop();
        let initial = self.initial(step);
        let total = if step.pick(0) % 2 == 0 {
            self.push_cell(
                Definition::Hold {
                    initial,
                    source: top(forward),
                },
                Scalar::Integer,
                false,
            )
        } else {
            self.push_cell(
                Definition::Accumulate {
                    initial,
                    function: step.accumulator(),
                    source: top(forward),
                },
                Scalar::Integer,
                false,
            )
        };
        let numbers = self.stream(step.pick(1), None);
        let sums = self.push_stream(
            Definition::Snapshot {
                function: step.binary(),
                source: top(numbers),
                cell: top(total),
            },
            Scalar::Integer,
        );
        let definition = if step.pick(2) % 2 == 0 {
            self.push_stream(Definition::Share(top(sums)), Scalar::Integer)
        } else {
            sums
        };
        self.close(forward, definition);
    }

    // ----- child transactions -----

    /// A split of a stream: a map to lists of zero to three elements, and
    /// the split of it. Consumes the stream; returns the split.
    fn split(&mut self, step: &Step, which: usize, source: usize) -> usize {
        let lists = self.push(
            Definition::MapList {
                length: step.length(which),
                element: step.element(which),
                source: top(source),
            },
            Slot::Lists,
        );
        self.push_stream(Definition::Split(top(lists)), Scalar::Integer)
    }

    /// Two splits, or a split and a defer, of one shared stream, now and
    /// then through an adapter: they fire in one instant and share child
    /// indices. Merged, or joined in a lift of holds, downstream.
    fn child_diamond(&mut self, step: &Step) {
        let source = self.stream(step.pick(0), None);
        let shared = self.shared(source);
        let first = self.split(step, 0, shared);
        let path = if step.pick(2) % 2 == 0 {
            shared
        } else {
            self.adapter(PATHS[step.pick(4) % PATHS.len()], step, shared)
        };
        let second = if step.pick(1) % 2 == 0 {
            self.split(step, 1, path)
        } else {
            let scalar = self.scalar(path);
            self.push_stream(Definition::Defer(top(path)), scalar)
        };
        self.join(step, step.pick(3) % 4, first, second);
    }

    /// The other stream a loop through children merges with its own: one
    /// that does not depend on the loop, taken before the guard is built.
    /// Then the join: an `or_else` or a merge, the loop's side on either.
    fn join_other(&mut self, step: &Step, other: usize, looped: usize) -> usize {
        let definition = match step.pick(2) % 3 {
            0 => Definition::OrElse {
                left: top(other),
                right: top(looped),
            },
            1 => Definition::Merge {
                function: step.combine(),
                left: top(other),
                right: top(looped),
            },
            _ => Definition::Merge {
                function: step.combine(),
                left: top(looped),
                right: top(other),
            },
        };
        self.push_stream(definition, Scalar::Integer)
    }

    /// The guard before a defer, over `source`: `(x mod m) - k`, kept while
    /// positive. Returns the defer.
    fn guarded_defer(&mut self, step: &Step, source: usize, forward: usize) -> usize {
        let (modulus, k) = step.guard();
        let less = self.push_stream(
            Definition::Map {
                function: guard_map(modulus, k),
                source: top(source),
            },
            Scalar::Integer,
        );
        let kept = self.push_stream(
            Definition::Filter {
                predicate: guard_filter(),
                source: top(less),
            },
            Scalar::Integer,
        );
        let later = self.push_stream(Definition::Defer(top(kept)), Scalar::Integer);
        self.guarded(later, forward);
        later
    }

    /// A countdown: a stream loop through a defer, whose guard ends it,
    /// merged with a stream from outside.
    fn countdown(&mut self, step: &Step) {
        let forward = self.declare_stream_loop();
        let other = self.stream_of_avoiding(step.pick(1), Scalar::Integer, bit(forward));
        let later = self.guarded_defer(step, forward, forward);
        let joined = self.join_other(step, other, later);
        let shared = self.push_stream(Definition::Share(top(joined)), Scalar::Integer);
        self.close(forward, shared);
    }

    /// A cell loop through a defer of its own steps view, whose guard ends
    /// it: legal where the same loop without the defer is F3's cycle.
    fn cell_countdown(&mut self, step: &Step) {
        let forward = self.declare_cell_loop(Scalar::Integer, false);
        let other = self.stream_of_avoiding(step.pick(1), Scalar::Integer, bit(forward));
        let steps = self.push_stream(Definition::Steps(top(forward)), Scalar::Integer);
        let later = self.guarded_defer(step, steps, forward);
        let joined = self.join_other(step, other, later);
        let held = self.push_cell(
            Definition::Hold {
                initial: literal(step.literal),
                source: top(joined),
            },
            Scalar::Integer,
            false,
        );
        self.close(forward, held);
    }

    /// A stream loop through a split: each event makes up to three smaller
    /// ones in its children, `(x mod m) - k - i`, kept while positive, so
    /// the split fires inside its own children (F7), and ends.
    fn split_loop(&mut self, step: &Step) {
        let forward = self.declare_stream_loop();
        let other = self.stream_of_avoiding(step.pick(1), Scalar::Integer, bit(forward));
        let (modulus, k) = step.guard();
        let lists = self.push(
            Definition::MapList {
                length: step.length(0),
                element: guard_element(modulus, k),
                source: top(forward),
            },
            Slot::Lists,
        );
        let items = self.push_stream(Definition::Split(top(lists)), Scalar::Integer);
        self.guarded(items, forward);
        let kept = self.push_stream(
            Definition::Filter {
                predicate: guard_filter(),
                source: top(items),
            },
            Scalar::Integer,
        );
        let joined = self.join_other(step, other, kept);
        let shared = self.push_stream(Definition::Share(top(joined)), Scalar::Integer);
        self.close(forward, shared);
    }

    // ----- switches -----

    /// A new stream of an input of the scalar, converted when no input
    /// carries it.
    fn input_of(&mut self, choice: usize, scalar: Scalar) -> usize {
        let matching: Vec<usize> = (0..self.inputs.len())
            .filter(|&k| self.scalars[k] == scalar)
            .collect();
        let node = match recent(&matching, choice) {
            Some(k) => self.push_stream(Definition::Input(k), scalar),
            None => self.input_stream(choice),
        };
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
        self.push_stream(converted, scalar)
    }

    /// A new share, of a linear stream of the scalar that nothing consumed
    /// and that depends on none of the loops in `avoid`, most recent first,
    /// or of an input's stream.
    fn new_share(&mut self, choice: usize, scalar: Scalar, avoid: u128) -> usize {
        let linear: Vec<usize> = (0..self.slots.len())
            .filter(|&node| {
                matches!(
                    self.slots[node],
                    Slot::Stream {
                        scalar: s,
                        shared: false,
                        consumed: false,
                    } if s == scalar
                ) && self.reach[node].depends & avoid == 0
            })
            .collect();
        let source = match recent(&linear, choice) {
            Some(node) => node,
            None => self.input_of(choice, scalar),
        };
        self.push_stream(Definition::Share(top(source)), scalar)
    }

    /// The shared streams, of one scalar or any, that depend on none of the
    /// loops in `avoid`.
    fn shares(&self, scalar: Option<Scalar>, avoid: u128) -> Vec<usize> {
        (0..self.slots.len())
            .filter(|&node| {
                matches!(
                    self.slots[node],
                    Slot::Stream { scalar: s, shared: true, .. }
                        if scalar.is_none_or(|want| want == s)
                ) && self.reach[node].depends & avoid == 0
            })
            .collect()
    }

    /// Two to four shared streams of one scalar, the given one or any, for
    /// a `switch_stream` to switch among, that depend on none of the loops
    /// in `avoid`: shares made before, most recent first, and now and then,
    /// or when too few are left, new ones.
    fn stream_candidates(
        &mut self,
        step: &Step,
        scalar: Option<Scalar>,
        avoid: u128,
    ) -> Vec<usize> {
        let count = 2 + step.pick(5) % 3;
        let first = match recent(&self.shares(scalar, avoid), step.pick(1)) {
            Some(node) if step.pick(1) % 4 != 0 => node,
            _ => self.new_share(step.pick(1), scalar.unwrap_or(Scalar::Integer), avoid),
        };
        let scalar = self.scalar(first);
        let mut candidates = vec![first];
        for k in 1..count {
            let choice = step.pick(1 + k) >> 4;
            let left: Vec<usize> = self
                .shares(Some(scalar), avoid)
                .into_iter()
                .filter(|node| !candidates.contains(node))
                .collect();
            let next = match recent(&left, choice) {
                Some(node) if choice % 4 != 0 => node,
                _ => self.new_share(choice, scalar, avoid),
            };
            candidates.push(next);
        }
        candidates
    }

    /// A new cell of integers, a `State` when `state`, that depends on none
    /// of the loops in `avoid`: a constant, an input cell or a hold of a
    /// stream; or an accumulator in place of a stream.
    fn new_cell(&mut self, step: &Step, choice: usize, state: bool, avoid: u128) -> usize {
        let value = literal((choice % 10) as i64 - 3);
        if state {
            let source = self.stream_avoiding(choice, None, avoid);
            return self.push_cell(
                Definition::AccumulateMut {
                    initial: value,
                    function: step.accumulator(),
                    source: top(source),
                },
                Scalar::Integer,
                true,
            );
        }
        let cell = match choice % 3 {
            0 => self.push_cell(Definition::Constant(value), Scalar::Integer, false),
            1 => self.input_cell(choice, value),
            _ => {
                let source = self.stream_avoiding(choice, None, avoid);
                let scalar = self.scalar(source);
                self.push_cell(
                    Definition::Hold {
                        initial: value,
                        source: top(source),
                    },
                    scalar,
                    false,
                )
            }
        };
        match self.scalar(cell) {
            Scalar::Integer => cell,
            Scalar::Boolean => self.convert_cell(cell, Scalar::Integer),
        }
    }

    /// Two to four cells of integers, `State`s when `state`, for a
    /// `switch_cell` to switch among, that depend on none of the loops in
    /// `avoid`: cells made before, most recent first, and now and then, or
    /// when too few are left, new ones.
    fn cell_candidates(&mut self, step: &Step, state: bool, avoid: u128) -> Vec<usize> {
        let count = 2 + step.pick(5) % 3;
        let mut candidates: Vec<usize> = Vec::new();
        for k in 0..count {
            let choice = step.pick(1 + k) >> 4;
            let left: Vec<usize> = (0..self.slots.len())
                .filter(|&node| {
                    matches!(
                        self.slots[node],
                        Slot::Cell { scalar: Scalar::Integer, state: s } if s == state
                    ) && self.reach[node].depends & avoid == 0
                        && !candidates.contains(&node)
                })
                .collect();
            let next = match recent(&left, choice) {
                Some(node) if choice % 4 != 0 => node,
                _ => self.new_cell(step, choice, state, avoid),
            };
            candidates.push(next);
        }
        candidates
    }

    /// Two to four shares of maps of a shared stream, which fire exactly
    /// when it does: `x * 10 + k` for the k-th.
    fn echo_streams(&mut self, step: &Step, shared: usize) -> Vec<usize> {
        let count = 2 + step.pick(5) % 3;
        (0..count)
            .map(|k| {
                let mapped = self.push_stream(
                    Definition::Map {
                        function: Argument * literal(10) + literal(k as i64),
                        source: top(shared),
                    },
                    Scalar::Integer,
                );
                self.push_stream(Definition::Share(top(mapped)), Scalar::Integer)
            })
            .collect()
    }

    /// Two to four cells that step exactly when a shared stream fires:
    /// holds of maps of it, or accumulators in place of it for `State`s.
    fn echo_cells(&mut self, step: &Step, shared: usize, state: bool) -> Vec<usize> {
        let count = 2 + step.pick(5) % 3;
        (0..count)
            .map(|k| {
                let k = k as i64;
                if state {
                    self.push_cell(
                        Definition::AccumulateMut {
                            initial: literal(k),
                            function: (SecondArgument + Argument * literal(10) + literal(k))
                                .modulo(1000),
                            source: top(shared),
                        },
                        Scalar::Integer,
                        true,
                    )
                } else {
                    let mapped = self.push_stream(
                        Definition::Map {
                            function: Argument * literal(10) + literal(k),
                            source: top(shared),
                        },
                        Scalar::Integer,
                    );
                    self.push_cell(
                        Definition::Hold {
                            initial: literal(k),
                            source: top(mapped),
                        },
                        Scalar::Integer,
                        false,
                    )
                }
            })
            .collect()
    }

    /// The stream a pick reads: a stream to consume that depends on none of
    /// the loops in `avoid`, now and then deferred or split, so that the
    /// switch moves in child instants.
    fn selector(&mut self, step: &Step, avoid: u128) -> usize {
        let source = self.stream_avoiding(step.pick(3) >> 8, None, avoid);
        match (step.pick(4) >> 8) % 6 {
            0 => {
                let scalar = self.scalar(source);
                self.push_stream(Definition::Defer(top(source)), scalar)
            }
            1 => self.split(step, 0, source),
            _ => source,
        }
    }

    /// The initial token of a hold of tokens: one of the candidates, or now
    /// and then one of none, `other`, which the switch starts from and
    /// never selects again.
    fn initial_token(step: &Step, candidates: &[usize], other: impl FnOnce() -> usize) -> usize {
        match (step.pick(3) >> 16) % 6 {
            0 => other(),
            choice => candidates[choice % candidates.len()],
        }
    }

    /// A `switch_stream` among the candidates, over a hold of a pick of
    /// them whose selector is `selector`, or one made for it; or now and
    /// then, when no selector is given, over a constant of one. Returns the
    /// switch.
    fn switch_stream_over(
        &mut self,
        step: &Step,
        candidates: &[usize],
        selector: Option<usize>,
        avoid: u128,
    ) -> usize {
        let scalar = self.scalar(candidates[0]);
        let outer = if selector.is_none() && step.pick(2) % 8 == 0 {
            self.push(Definition::ConstantStream(top(candidates[0])), Slot::Outer)
        } else {
            let selector = match selector {
                Some(selector) => selector,
                None => self.selector(step, avoid),
            };
            let pick = self.push(
                Definition::PickStream {
                    index: step.index(),
                    streams: candidates.iter().map(|&c| top(c)).collect(),
                    source: top(selector),
                },
                Slot::Tokens,
            );
            let choice = step.pick(0) >> 8;
            let initial =
                Draft::initial_token(step, candidates, || self.new_share(choice, scalar, avoid));
            self.push(
                Definition::HoldStream {
                    initial: top(initial),
                    source: top(pick),
                },
                Slot::Outer,
            )
        };
        self.push_stream(Definition::SwitchStream(top(outer)), scalar)
    }

    /// A `switch_cell` among the candidates, `State`s when `state`: over a
    /// hold of a pick of them whose selector is `selector`, or one made for
    /// it; or now and then, when no selector is given, over a map_cell of a
    /// cell to them, which steps whenever that cell does, or over a
    /// constant of one. Returns the switch.
    fn switch_cell_over(
        &mut self,
        step: &Step,
        candidates: &[usize],
        state: bool,
        selector: Option<usize>,
        avoid: u128,
    ) -> usize {
        let cells: Vec<Reference> = candidates.iter().map(|&c| top(c)).collect();
        let outer = match (selector, step.pick(2) % 8) {
            (None, 0) => self.push(Definition::ConstantCell(cells[0]), Slot::Outer),
            (None, 1 | 2) => {
                let readable: Vec<usize> = self
                    .cells(None, false)
                    .into_iter()
                    .filter(|&cell| self.reach[cell].depends & avoid == 0)
                    .collect();
                let cell = match recent(&readable, step.pick(3) >> 8) {
                    Some(cell) => cell,
                    None => self.input_cell(step.pick(3), literal(0)),
                };
                self.push(
                    Definition::MapPickCell {
                        index: step.index(),
                        cells,
                        cell: top(cell),
                    },
                    Slot::Outer,
                )
            }
            (selector, _) => {
                let selector = match selector {
                    Some(selector) => selector,
                    None => self.selector(step, avoid),
                };
                let pick = self.push(
                    Definition::PickCell {
                        index: step.index(),
                        cells,
                        source: top(selector),
                    },
                    Slot::Tokens,
                );
                let choice = step.pick(0) >> 8;
                let initial = Draft::initial_token(step, candidates, || {
                    self.new_cell(step, choice, state, avoid)
                });
                self.push(
                    Definition::HoldCell {
                        initial: top(initial),
                        source: top(pick),
                    },
                    Slot::Outer,
                )
            }
        };
        self.push_cell(Definition::SwitchCell(top(outer)), Scalar::Integer, state)
    }

    /// A `switch_stream` among two to four shared streams that depend on
    /// none of the loops in `avoid`. One in four takes its streams and its
    /// selector from one shared stream, so that it switches at an instant
    /// at which the old stream and the new one both fire. Returns the
    /// switch.
    fn switch_stream(&mut self, step: &Step, avoid: u128) -> usize {
        if step.pick(0) % 4 == 0 {
            let source = self.stream_avoiding(step.pick(1), None, avoid);
            let shared = self.shared(source);
            let candidates = self.echo_streams(step, shared);
            let selector = if step.pick(2) % 2 == 0 {
                shared
            } else {
                self.adapter(Kind::Map, step, shared)
            };
            self.switch_stream_over(step, &candidates, Some(selector), avoid)
        } else {
            let candidates = self.stream_candidates(step, None, avoid);
            self.switch_stream_over(step, &candidates, None, avoid)
        }
    }

    /// A `switch_cell` among two to four cells of integers, `State`s when
    /// `state`, that depend on none of the loops in `avoid`. One in four
    /// takes its cells and its selector from one shared stream, so that it
    /// switches at an instant at which the old cell and the new one both
    /// step. Returns the switch.
    fn switch_cell(&mut self, step: &Step, state: bool, avoid: u128) -> usize {
        if step.pick(0) % 4 == 0 {
            let source = self.stream_avoiding(step.pick(1), None, avoid);
            let shared = self.shared(source);
            let candidates = self.echo_cells(step, shared, state);
            let selector = if step.pick(2) % 2 == 0 {
                shared
            } else {
                self.adapter(Kind::Map, step, shared)
            };
            self.switch_cell_over(step, &candidates, state, Some(selector), avoid)
        } else {
            let candidates = self.cell_candidates(step, state, avoid);
            self.switch_cell_over(step, &candidates, state, None, avoid)
        }
    }

    /// Nested switches: a `switch_cell` among two or three `switch_cell`s,
    /// or a `switch_stream` among shares of two or three `switch_stream`s,
    /// each inner switch with choices of its own.
    fn nested_switch(&mut self, step: &Step) {
        let count = 2 + step.pick(1) % 2;
        let top_step = step.varied(7);
        if step.pick(0) % 2 == 0 {
            let state = step.pick(2) % 4 == 0;
            let inner: Vec<usize> = (1..=count)
                .map(|part| self.switch_cell(&step.varied(part as u32), state, 0))
                .collect();
            let switch = self.switch_cell_over(&top_step, &inner, state, None, 0);
            if !state && step.pick(3) % 2 == 0 {
                self.push_stream(Definition::Steps(top(switch)), Scalar::Integer);
            }
        } else {
            let mut inner: Vec<usize> = Vec::new();
            for part in 1..=count {
                let switch = self.switch_stream(&step.varied(part as u32), 0);
                let scalar = self.scalar(switch);
                inner.push(self.push_stream(Definition::Share(top(switch)), scalar));
            }
            let scalar = self.scalar(inner[0]);
            inner.retain(|&shared| self.scalar(shared) == scalar);
            if inner.len() < 2 {
                inner.push(self.new_share(step.pick(4), scalar, 0));
            }
            self.switch_stream_over(&top_step, &inner, None, 0);
        }
    }

    /// A loop through a switch's selection: a stream loop whose events
    /// select, through a hold, the stream a `switch_stream` follows from the
    /// next instant on, the switch's events closing the loop, which is
    /// legal because the selection is read before the instant (finding
    /// F14); or a cell loop that a snapshot reads to select for a
    /// `switch_cell`, which closes it, or for a `switch_stream`, a hold of
    /// whose events closes it.
    fn switch_loop(&mut self, step: &Step) {
        match step.pick(0) % 3 {
            0 => {
                let forward = self.declare_stream_loop();
                let closing = bit(forward);
                let candidates = if step.pick(1) % 2 == 0 {
                    let ticks = self.stream_avoiding(step.pick(2), None, closing);
                    let ticks = self.shared(ticks);
                    self.echo_streams(step, ticks)
                } else {
                    self.stream_candidates(step, Some(Scalar::Integer), closing)
                };
                let out = self.push_stream(Definition::Share(top(forward)), Scalar::Integer);
                let selector = if step.pick(3) % 2 == 0 {
                    out
                } else {
                    self.adapter(Kind::Map, step, out)
                };
                let switch = self.switch_stream_over(step, &candidates, Some(selector), closing);
                self.close(forward, switch);
            }
            1 => {
                let forward = self.declare_cell_loop(Scalar::Integer, false);
                let closing = bit(forward);
                let candidates = self.cell_candidates(step, false, closing);
                let ticks = self.stream_avoiding(step.pick(2), None, closing);
                let selector = self.push_stream(
                    Definition::Snapshot {
                        function: step.binary(),
                        source: top(ticks),
                        cell: top(forward),
                    },
                    Scalar::Integer,
                );
                let switch =
                    self.switch_cell_over(step, &candidates, false, Some(selector), closing);
                self.close(forward, switch);
            }
            _ => {
                let forward = self.declare_cell_loop(Scalar::Integer, false);
                let closing = bit(forward);
                let candidates = self.stream_candidates(step, Some(Scalar::Integer), closing);
                let ticks = self.stream_avoiding(step.pick(2), None, closing);
                let selector = self.push_stream(
                    Definition::Snapshot {
                        function: step.binary(),
                        source: top(ticks),
                        cell: top(forward),
                    },
                    Scalar::Integer,
                );
                let switch = self.switch_stream_over(step, &candidates, Some(selector), closing);
                let held = self.push_cell(
                    Definition::Hold {
                        initial: literal(step.literal),
                        source: top(switch),
                    },
                    Scalar::Integer,
                    false,
                );
                self.close(forward, held);
            }
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
                Slot::Lists | Slot::Tokens | Slot::Outer | Slot::Closed => false,
            })
            .collect()
    }
}

/// The paths of a diamond from a shared stream.
const PATHS: [Kind; 7] = [
    Kind::Map,
    Kind::Filter,
    Kind::Snapshot,
    Kind::Gate,
    Kind::Once,
    Kind::FilterMap,
    Kind::Map,
];

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
            reach: Vec::new(),
            open: Vec::new(),
        };
        for step in &self.steps {
            let plain;
            let step = if self.plain {
                plain = step.plain();
                &plain
            } else {
                step
            };
            let mut attempt = draft.clone();
            attempt.step(step);
            if attempt.room(0) {
                draft = attempt;
            } else if draft.room(1) {
                draft.input_stream(step.pick(0));
            }
        }
        // Every loop still open closes, each with a step's choices.
        let mut k = 0;
        while let Some(&forward) = draft.open.first() {
            draft.close_loop(forward, &self.steps[k % self.steps.len()]);
            k += 1;
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

// ----- loops that end -----

/// Whether a split or a defer is guarded, as the generator's patterns guard
/// the loops through them: a defer of `filter(0 < x)` of
/// `map((x mod m) - k)`, `k >= 1`; a split of a `MapList` whose element i
/// is `(x mod m) - k - i`, `k >= 1`, read by `filter(0 < x)` alone. Every
/// event that comes out of either is positive and below the one that went
/// in, so a loop through it that carries the value unchanged back to the
/// guard ends.
fn is_guarded(program: &Program, node: usize) -> bool {
    let definition = |reference: &Reference| match reference {
        Reference::TopLevel(n) if *n < node => program.definitions.get(*n),
        _ => None,
    };
    match &program.definitions[node] {
        Definition::Defer(source) => match definition(source) {
            Some(Definition::Filter { predicate, source }) if is_guard_filter(predicate) => {
                matches!(definition(source), Some(Definition::Map { function, .. }) if is_guard_map(function))
            }
            _ => false,
        },
        Definition::Split(source) => {
            let element = matches!(definition(source), Some(Definition::MapList { element, .. }) if is_guard_element(element));
            let readers: Vec<&Definition> = program
                .definitions
                .iter()
                .filter(|reader| {
                    build::consumed_streams(reader).contains(&Reference::TopLevel(node))
                })
                .collect();
            let filtered = matches!(readers.as_slice(), [Definition::Filter { predicate, .. }] if is_guard_filter(predicate));
            element && filtered && !program.observe.contains(&node)
        }
        _ => false,
    }
}

/// Whether every loop of a program ends, as the generator's do. First, with
/// the edge into each guarded split or defer cut, the graph of every edge
/// but a read of a cell from before the instant, where a loop's forward
/// depends on its definition, has no cycle: so the engine's dependency
/// graph has none, and every cycle through a split or a defer passes
/// through a guard. Second, everything on a cycle through a guard carries
/// the value unchanged, or drops it, or is another guard: a loop's forward,
/// a share, a node, a hold, a steps view, a filter, a gate, a once, or a
/// merge or an `or_else` with only one input on the cycle, whose other
/// input fires finitely often. So each time round, the value is below the
/// last, and the guard's filter ends it.
pub fn well_founded(program: &Program) -> bool {
    let n = program.definitions.len();
    let mut next: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (index, definition) in program.definitions.iter().enumerate() {
        match definition {
            Definition::Close {
                forward,
                definition: Reference::TopLevel(node),
            } if *node < n && *forward < n => next[*node].push(*forward),
            _ => {
                for node in dependencies(&program.definitions, definition) {
                    if node < n {
                        next[node].push(index);
                    }
                }
            }
        }
    }
    let guarded: Vec<bool> = (0..n).map(|node| is_guarded(program, node)).collect();
    if has_cycle(&next, |_, to| guarded[to]) {
        return false;
    }
    let mut previous: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (from, targets) in next.iter().enumerate() {
        for &to in targets {
            previous[to].push(from);
        }
    }
    for guard in (0..n).filter(|&node| guarded[node]) {
        let source = match &program.definitions[guard] {
            Definition::Split(Reference::TopLevel(source))
            | Definition::Defer(Reference::TopLevel(source)) => *source,
            _ => continue,
        };
        let after = reachable(&next, guard);
        let before = reachable(&previous, source);
        let on_cycle: Vec<bool> = (0..n).map(|node| after[node] && before[node]).collect();
        for node in (0..n).filter(|&node| on_cycle[node]) {
            let on = |reference: &Reference| match reference {
                Reference::TopLevel(input) => on_cycle.get(*input).copied().unwrap_or(false),
                Reference::Local(_) => false,
            };
            let keeps = match &program.definitions[node] {
                Definition::StreamLoop(_)
                | Definition::CellLoop(_)
                | Definition::Share(_)
                | Definition::Node(_)
                | Definition::Hold { .. }
                | Definition::Steps(_)
                | Definition::StepsWithCurrent(_)
                | Definition::Filter { .. }
                | Definition::Gate { .. }
                | Definition::Once(_) => true,
                Definition::Map { function, .. } => is_guard_map(function),
                Definition::MapList { element, .. } => is_guard_element(element),
                Definition::Split(_) | Definition::Defer(_) => guarded[node],
                Definition::Merge { left, right, .. } | Definition::OrElse { left, right } => {
                    on(left) != on(right)
                }
                _ => false,
            };
            if !keeps {
                return false;
            }
        }
    }
    true
}

/// Whether a graph has a cycle, the edges `skip` names left out.
fn has_cycle(next: &[Vec<usize>], skip: impl Fn(usize, usize) -> bool) -> bool {
    // 0: not seen; 1: on the stack; 2: done.
    let mut state = vec![0_u8; next.len()];
    for start in 0..next.len() {
        if state[start] != 0 {
            continue;
        }
        let mut stack = vec![(start, 0_usize)];
        state[start] = 1;
        while let Some((node, k)) = stack.last_mut() {
            let node = *node;
            if let Some(&to) = next[node].get(*k) {
                *k += 1;
                if skip(node, to) {
                    continue;
                }
                match state[to] {
                    0 => {
                        state[to] = 1;
                        stack.push((to, 0));
                    }
                    1 => return true,
                    _ => {}
                }
            } else {
                state[node] = 2;
                stack.pop();
            }
        }
    }
    false
}

/// The nodes reachable from `start` over `next`, `start` included.
fn reachable(next: &[Vec<usize>], start: usize) -> Vec<bool> {
    let mut seen = vec![false; next.len()];
    let mut stack = vec![start];
    seen[start] = true;
    while let Some(node) = stack.pop() {
        for &to in &next[node] {
            if !seen[to] {
                seen[to] = true;
                stack.push(to);
            }
        }
    }
    seen
}

/// The program with the definition of one of its cell loops of integers
/// that are not a `State` made to depend on the loop's own forward in the
/// same instant, for the test that the engine refuses such a loop at its
/// close. `which` picks the loop and the way: a lift of the definition and
/// the forward; a lift of the definition and a `map_cell` of the forward;
/// or a lift of the definition and a hold of the forward's `steps`, which
/// RFD 2's path rule accepts because the path passes through a hold
/// (finding F3). The new nodes go just before the loop's `Close`, which
/// names the lift. `None` if the program has no such loop.
pub fn with_same_instant_cycle(program: &Program, which: usize) -> Option<Program> {
    let types = build::check(program).ok()?;
    let integers = NodeType::Cell {
        value: Scalar::Integer,
        state: false,
    };
    let loops: Vec<(usize, usize, usize)> = program
        .definitions
        .iter()
        .enumerate()
        .filter_map(|(close, definition)| match definition {
            Definition::Close {
                forward,
                definition: Reference::TopLevel(node),
            } if types[*forward] == integers => Some((*forward, close, *node)),
            _ => None,
        })
        .collect();
    if loops.is_empty() {
        return None;
    }
    let (forward, close, definition) = loops[which % loops.len()];
    let (f, d, at) = (top(forward), top(definition), close);
    let lift = |cell: usize| Definition::Lift {
        function: Expression::ArgumentAt(0) + Expression::ArgumentAt(1),
        cells: vec![d, top(cell)],
    };
    let inserted = match (which / loops.len()) % 3 {
        0 => vec![Definition::Lift {
            function: Expression::ArgumentAt(0) - Expression::ArgumentAt(1),
            cells: vec![d, f],
        }],
        1 => vec![
            Definition::MapCell {
                function: Argument + literal(1),
                cell: f,
            },
            lift(at),
        ],
        _ => vec![
            Definition::Steps(f),
            Definition::Map {
                function: Argument + literal(1),
                source: top(at),
            },
            Definition::Hold {
                initial: literal(0),
                source: top(at + 1),
            },
            lift(at + 2),
        ],
    };
    let shift = inserted.len();
    let map = |node: usize| if node >= close { node + shift } else { node };
    let mut definitions = program.definitions[..close].to_vec();
    definitions.extend(inserted);
    definitions.push(Definition::Close {
        forward,
        definition: top(close + shift - 1),
    });
    definitions.extend(
        program.definitions[close + 1..]
            .iter()
            .map(|definition| rename(definition, &map)),
    );
    Some(Program {
        definitions,
        observe: program.observe.iter().map(|&node| map(node)).collect(),
        ..program.clone()
    })
}

// ----- reducing a failing program -----

/// Every top-level node a definition reads: its streams, its cells, the
/// cells its expressions sample, and for a `Close`, its loop and its
/// definition.
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
        | Definition::Share(source)
        | Definition::Split(source)
        | Definition::Defer(source) => add(source),
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
        Definition::MapList {
            length,
            element,
            source,
        } => {
            add(source);
            expressions.extend([length, element]);
        }
        Definition::Close {
            forward,
            definition,
        } => {
            add(&Reference::TopLevel(*forward));
            add(definition);
        }
        Definition::PickStream {
            index,
            streams: listed,
            source,
        }
        | Definition::PickCell {
            index,
            cells: listed,
            source,
        } => {
            listed.iter().for_each(&mut add);
            add(source);
            expressions.push(index);
        }
        Definition::HoldStream { initial, source } | Definition::HoldCell { initial, source } => {
            add(initial);
            add(source);
        }
        Definition::MapPickCell { index, cells, cell } => {
            cells.iter().for_each(&mut add);
            add(cell);
            expressions.push(index);
        }
        Definition::ConstantStream(token)
        | Definition::ConstantCell(token)
        | Definition::SwitchStream(token)
        | Definition::SwitchCell(token) => add(token),
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

/// Renames every top-level reference, in definitions and expressions, and
/// the loop a `Close` closes.
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
        Definition::Split(source) => Definition::Split(r(source)),
        Definition::Defer(source) => Definition::Defer(r(source)),
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
        Definition::MapList {
            length,
            element,
            source,
        } => Definition::MapList {
            length: e(length),
            element: e(element),
            source: r(source),
        },
        Definition::Close {
            forward,
            definition,
        } => Definition::Close {
            forward: map(*forward),
            definition: r(definition),
        },
        Definition::PickStream {
            index,
            streams,
            source,
        } => Definition::PickStream {
            index: e(index),
            streams: streams.iter().map(r).collect(),
            source: r(source),
        },
        Definition::PickCell {
            index,
            cells,
            source,
        } => Definition::PickCell {
            index: e(index),
            cells: cells.iter().map(r).collect(),
            source: r(source),
        },
        Definition::HoldStream { initial, source } => Definition::HoldStream {
            initial: r(initial),
            source: r(source),
        },
        Definition::HoldCell { initial, source } => Definition::HoldCell {
            initial: r(initial),
            source: r(source),
        },
        Definition::ConstantStream(stream) => Definition::ConstantStream(r(stream)),
        Definition::ConstantCell(cell) => Definition::ConstantCell(r(cell)),
        Definition::MapPickCell { index, cells, cell } => Definition::MapPickCell {
            index: e(index),
            cells: cells.iter().map(r).collect(),
            cell: r(cell),
        },
        Definition::SwitchStream(outer) => Definition::SwitchStream(r(outer)),
        Definition::SwitchCell(outer) => Definition::SwitchCell(r(outer)),
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
        | Definition::Defer(Reference::TopLevel(source))
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

/// What a node reads: its references, and for a loop, its `Close`, which
/// names what the loop becomes.
fn inputs(program: &Program, node: usize) -> Vec<usize> {
    let mut nodes = references(&program.definitions[node]);
    if build::is_loop(&program.definitions[node]) {
        nodes.extend(
            program
                .definitions
                .iter()
                .enumerate()
                .filter(|(_, definition)| {
                    matches!(definition, Definition::Close { forward, .. } if *forward == node)
                })
                .map(|(close, _)| close),
        );
    }
    nodes
}

/// Every node `node` reads, directly or through others.
fn ancestors(program: &Program, node: usize) -> Vec<usize> {
    let mut seen = vec![false; program.definitions.len()];
    let mut stack = inputs(program, node);
    let mut found = Vec::new();
    while let Some(next) = stack.pop() {
        if !seen[next] {
            seen[next] = true;
            found.push(next);
            stack.extend(inputs(program, next));
        }
    }
    found.sort_unstable();
    found
}

/// Both streams of one scalar, or both cells of one.
fn same_type(a: NodeType, b: NodeType) -> bool {
    match (a, b) {
        (NodeType::Stream(a), NodeType::Stream(b)) => a == b,
        (NodeType::Cell { value: a, .. }, NodeType::Cell { value: b, .. }) => a == b,
        (NodeType::Tokens(a), NodeType::Tokens(b)) | (NodeType::Outer(a), NodeType::Outer(b)) => {
            a == b
        }
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
        Definition::MapList {
            length, element, ..
        } => vec![length, element],
        Definition::PickStream { index, .. }
        | Definition::PickCell { index, .. }
        | Definition::MapPickCell { index, .. } => vec![index],
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
    // Loops cut open: the forward made a constant, or a stream that never
    // fires, and its Close dropped, so what the loop fed back no longer
    // comes back.
    for (close, definition) in program.definitions.iter().enumerate() {
        if let Definition::Close { forward, .. } = definition {
            let opened = match &program.definitions[*forward] {
                Definition::CellLoop(Type::Integer) => Definition::Constant(literal(0)),
                Definition::StreamLoop(event_type) => Definition::Never(event_type.clone()),
                _ => continue,
            };
            let mut p = program.clone();
            p.definitions[*forward] = opened;
            smaller.extend(without(&p, &[close]));
        }
    }
    // Identity-like nodes bypassed.
    for node in 0..program.definitions.len() {
        smaller.extend(bypassed(program, node));
    }
    // References moved upstream, to an ancestor of the same type defined
    // before the node referred to, which strands what was between. A loop's
    // ancestors include its definition, which comes later, and a move there
    // could be undone by a move back; so a move only goes to an earlier
    // node. A `Close` keeps its loop.
    if let Ok(types) = build::check(program) {
        for node in 0..program.definitions.len() {
            let mut targets = references(&program.definitions[node]);
            if let Definition::Close { forward, .. } = &program.definitions[node] {
                targets.retain(|target| target != forward);
            }
            targets.sort_unstable();
            targets.dedup();
            for target in targets {
                for ancestor in ancestors(program, target) {
                    if ancestor < target && same_type(types[ancestor], types[target]) {
                        let mut p = program.clone();
                        let map = |n: usize| if n == target { ancestor } else { n };
                        p.definitions[node] = rename(&program.definitions[node], &map);
                        smaller.push(self::live(&p));
                    }
                }
            }
        }
        // A node replaced by a source of its type, and what is then dead
        // dropped: a stream by an input's stream, a cell by an input cell,
        // on a new input where none carries its scalar.
        for (node, made) in types.iter().enumerate() {
            let (scalar, cell) = match made {
                NodeType::Stream(scalar) => (*scalar, false),
                NodeType::Cell { value, .. } => (*value, true),
                NodeType::Lists | NodeType::Tokens(_) | NodeType::Outer(_) | NodeType::Closed => {
                    continue;
                }
            };
            if matches!(
                program.definitions[node],
                Definition::Input(_)
                    | Definition::InputCell { .. }
                    | Definition::Share(_)
                    | Definition::CellLoop(_)
                    | Definition::StreamLoop(_)
            ) {
                continue;
            }
            let mut p = program.clone();
            let input_type = type_of(scalar);
            let k = match p
                .inputs
                .iter()
                .position(|input| input.event_type == input_type)
            {
                Some(k) => k,
                None => {
                    p.inputs.push(Input::new(input_type));
                    p.inputs.len() - 1
                }
            };
            p.definitions[node] = if cell {
                Definition::InputCell {
                    input: k,
                    initial: literal(0),
                }
            } else {
                Definition::Input(k)
            };
            smaller.push(self::live(&p));
        }
    }
    // An observation moved to an earlier ancestor, and what is then dead
    // dropped.
    for position in 0..program.observe.len() {
        let observed = program.observe[position];
        for ancestor in ancestors(program, observed) {
            if ancestor >= observed {
                continue;
            }
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
/// simplifying keeps it failing, and returns the smallest found. A program
/// that is [`well_founded`] shrinks only to programs that are, so that a
/// cut never turns it into a loop that does not end, which the oracle would
/// take its whole time limit to answer.
pub fn reduce(program: &Program, mut fails: impl FnMut(&Program) -> bool) -> Program {
    let founded = well_founded(program);
    let mut best = program.clone();
    'shrink: loop {
        for candidate in candidates(&best) {
            if build::check(&candidate).is_ok()
                && (!founded || well_founded(&candidate))
                && fails(&candidate)
            {
                best = candidate;
                continue 'shrink;
            }
        }
        return best;
    }
}
