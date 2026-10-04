//! The Bough side: the one running graph, and the `construct` that every
//! command which adds nodes goes through.
//!
//! Nothing adds nodes after `Runtime::build` except a `construct` closure,
//! so the build makes one input of [`Command`]s and a construct over it.
//! A command travels into the graph as an event, the closure builds what it
//! says, and what it built comes back out as an event, anchored, which a
//! listener leaves for [`Graph::make`] to pick up once `send` returns.

use std::cell::RefCell;
use std::rc::Rc;

use bough::{Anchored, Build, Input, Runtime, Source};

use crate::registry::Wire;
use crate::ty::{InputToken, Literal, Node};

/// An argument of a definition: a binding's node, or a literal to make a
/// constant of.
#[derive(Debug)]
pub enum Arg {
    /// An existing binding.
    Binding(Node),
    /// A literal.
    Literal(Literal),
}

impl Arg {
    fn node(self, b: &mut Build) -> Node {
        match self {
            Arg::Binding(node) => node,
            Arg::Literal(literal) => literal.constant(b),
        }
    }
}

/// What a `def` asks the graph to build.
#[derive(Debug)]
pub enum Def {
    /// A registry function applied to arguments.
    Apply(Wire, Vec<Arg>),
    /// Another binding, or a literal: no function.
    Alias(Arg),
}

impl Def {
    /// Builds the definition's cell. An alias of a binding builds nothing.
    pub fn build(self, b: &mut Build) -> Node {
        match self {
            Def::Apply(wire, args) => {
                let args: Vec<Node> = args.into_iter().map(|arg| arg.node(b)).collect();
                wire(b, &args)
            }
            Def::Alias(arg) => arg.node(b),
        }
    }
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
    /// A defined binding's cell.
    Defined(Anchored<Node>),
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
        let (mut runtime, edge) = Runtime::build(|b| {
            let (commands, commands_in) = b.input::<Command>();
            let made = commands.construct(b, make);
            (commands_in, made)
        });
        let (commands, made_events) = edge.keep();
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
fn make(b: &mut Build, command: Command) -> Made {
    match command {
        Command::Input(literal) => {
            let made = literal.input(b);
            Made::Input(b.anchor(made))
        }
        Command::Define(def) => {
            let node = def.build(b);
            Made::Defined(b.anchor(node))
        }
    }
}
