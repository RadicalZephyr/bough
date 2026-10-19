//! The Bough side: the one running graph, and the `construct` that every
//! command which adds nodes goes through.
//!
//! Every command that adds nodes does so in a `construct` closure: the
//! build makes one input of [`Command`]s and a construct over it.
//! A command travels into the graph as an event, the closure builds what it
//! says, and what it built comes back out as an event, anchored, which a
//! listener leaves for [`Graph::make`] to pick up once `send` returns.
//!
//! A defined binding is rebindable: its node is a `switch_cell` over a hold
//! of its current definition, a cell of cells. The hold is fed by a
//! construct of its own, over an input of [`Def`]s, so a redefinition is one
//! send to that input: the construct builds the new definition, the hold
//! takes it, and the switch moves to it at commit, in the one transaction.
//! Dependents name the switch, never the definition behind it.

use std::cell::RefCell;
use std::rc::Rc;

use bough::{Anchored, BuildContext, Builder, Cell, Input, Runtime, Source};

use crate::registry::Wire;
use crate::ty::{InputToken, Literal, Node};

/// What a `def` asks the graph to build: a tree whose leaves are bindings,
/// literals and parameters, and whose inner nodes are registry functions
/// and `defgraph` calls.
#[derive(Debug)]
pub enum Def {
    /// A registry function applied to arguments, each a definition of its
    /// own.
    Apply(Wire, Vec<Def>),
    /// A `defgraph` called with arguments: each is built once, and the
    /// body's [`Param`](Def::Param)s share it.
    Call(Vec<Def>, Box<Def>),
    /// An argument of the innermost call, by position.
    Param(usize),
    /// An existing binding.
    Binding(Node),
    /// A literal, to make a constant of.
    Literal(Literal),
}

impl Def {
    /// Builds the definition's cell: a cell for each application and each
    /// literal in it, and none for a binding it names. The cells under the
    /// top one are plain, with no binding's switch: nothing names them, so
    /// nothing redefines them.
    pub fn build(self, b: &mut BuildContext) -> Node {
        self.build_with(b, &[])
    }

    /// [`build`](Def::build), inside a call whose arguments are `params`.
    fn build_with(self, b: &mut BuildContext, params: &[Node]) -> Node {
        match self {
            Def::Apply(wire, args) => {
                let args = build_all(b, args, params);
                wire(b, &args)
            }
            Def::Call(args, body) => {
                let args = build_all(b, args, params);
                body.build_with(b, &args)
            }
            Def::Param(i) => params[i],
            Def::Binding(node) => node,
            Def::Literal(literal) => literal.constant(b),
        }
    }
}

fn build_all(b: &mut BuildContext, defs: Vec<Def>, params: &[Node]) -> Vec<Node> {
    defs.into_iter()
        .map(|def| def.build_with(b, params))
        .collect()
}

/// An event of the root construct: each makes a new binding.
#[derive(Debug)]
pub enum Command {
    /// An input cell starting at the literal.
    Input(Literal),
    /// A defined binding.
    Define(Def),
}

/// What the root construct made, anchored, so that it survives the
/// collection after its unit.
pub enum Made {
    /// An input's cell and its token.
    Input(Anchored<(Node, InputToken)>),
    /// A defined binding's cell, the switch, and the input its
    /// redefinitions go to.
    Defined(Anchored<(Node, Input<Def>)>),
}

/// The running graph.
pub struct Graph {
    runtime: Runtime,
    commands: Input<Command>,
    made: Rc<RefCell<Option<Made>>>,
}

impl Graph {
    /// Builds the graph: an input of commands and the construct over it.
    pub fn new() -> Graph {
        let mut runtime = Runtime::local();
        let (commands, made_events) = runtime.build_here(|b| {
            let (commands, commands_in) = b.input::<Command>();
            let made = commands.construct(b, make);
            (commands_in, made)
        });
        let made = Rc::new(RefCell::new(None));
        let slot = made.clone();
        runtime
            .listen(made_events, move |m| *slot.borrow_mut() = Some(m))
            .keep();
        Graph {
            runtime,
            commands,
            made,
        }
    }

    /// Runs one command as a transaction of its own, and returns what it
    /// made.
    pub fn make(&mut self, command: Command) -> Made {
        self.runtime.send(self.commands, command);
        self.made
            .borrow_mut()
            .take()
            .expect("bough-repl: the construct fires once per command")
    }

    /// Redefines a binding: one send, so one transaction.
    pub fn redefine(&mut self, binding: Input<Def>, def: Def) {
        self.runtime.send(binding, def);
    }

    /// The runtime, for sends and listeners.
    pub fn runtime(&mut self) -> &mut Runtime {
        &mut self.runtime
    }
}

impl Default for Graph {
    fn default() -> Graph {
        Graph::new()
    }
}

/// The root construct's closure.
fn make(b: &mut BuildContext, command: Command) -> Made {
    match command {
        Command::Input(literal) => {
            let made = literal.input(b);
            Made::Input(b.anchor(made))
        }
        Command::Define(def) => {
            let first = def.build(b);
            let binding = bind_node(b, first);
            Made::Defined(b.anchor(binding))
        }
    }
}

/// A rebindable binding over its first definition: [`bind`] for the cell
/// type of the definition's arm.
fn bind_node(b: &mut BuildContext, first: Node) -> (Node, Input<Def>) {
    match first {
        Node::IntCell(cell) => {
            let (cell, redefine) = bind(b, cell, Node::int);
            (Node::IntCell(cell), redefine)
        }
        Node::BoolCell(cell) => {
            let (cell, redefine) = bind(b, cell, Node::bool);
            (Node::BoolCell(cell), redefine)
        }
        Node::StrCell(cell) => {
            let (cell, redefine) = bind(b, cell, Node::str);
            (Node::StrCell(cell), redefine)
        }
    }
}

/// The cell of cells under a binding: an input of definitions, a construct
/// that builds each, a hold of the current one, and the switch dependents
/// name. Four nodes, besides the definition's own. `cell` recovers the
/// concrete cell from a definition's [`Node`]: the hold is typed, so every
/// definition the construct builds must be of the binding's type.
fn bind<A: 'static>(
    b: &mut BuildContext,
    first: Cell<A>,
    cell: fn(Node) -> Cell<A>,
) -> (Cell<A>, Input<Def>) {
    let (redefinitions, redefine) = b.input::<Def>();
    let definitions = redefinitions.construct(b, move |b, def| cell(def.build(b)));
    let current = definitions.hold(b, first);
    (current.switch_cell(b), redefine)
}
