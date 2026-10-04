//! The REPL layer: parses a command, checks it against [`Type`] tags and
//! the binding namespace, and only then touches the graph. It never handles
//! a binding's value; a literal in a command is the one value it carries,
//! into the graph.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use bough::Listener;

use crate::graph::{Arg, Command, Def, Graph, Made};
use crate::registry;
use crate::ty::{Literal, Node, Type};

/// A name with a fixed [`Type`] and a [`Node`].
struct Binding {
    name: String,
    ty: Type,
    made: Made,
    /// The definition as typed, for `graph`.
    shown: String,
}

impl Binding {
    fn node(&self) -> Node {
        match &self.made {
            Made::Input(made) => made.0,
            Made::Defined(node) => **node,
        }
    }
}

/// The REPL: one running graph, its bindings, and what it has printed.
pub struct Repl {
    graph: Graph,
    /// In definition order, for `graph`.
    bindings: Vec<Binding>,
    by_name: HashMap<String, usize>,
    watching: Vec<(String, Listener)>,
    out: Rc<RefCell<Vec<String>>>,
}

type Checked<T> = Result<T, String>;

impl Repl {
    /// A REPL with an empty graph.
    pub fn new() -> Repl {
        Repl {
            graph: Graph::new(),
            bindings: Vec::new(),
            by_name: HashMap::new(),
            watching: Vec::new(),
            out: Rc::default(),
        }
    }

    /// Runs one line, and returns what it printed, watch lines included.
    pub fn run(&mut self, line: &str) -> Vec<String> {
        let words: Vec<&str> = line.split_whitespace().collect();
        let result = match words.as_slice() {
            [] => Ok(()),
            [first, ..] if first.starts_with('#') => Ok(()),
            ["input", name, literal] => self.input(name, literal),
            ["def", name, definition @ ..] if !definition.is_empty() => self.def(name, definition),
            ["set", name, literal] => self.set(name, literal),
            ["watch", name] => self.watch(name),
            ["graph"] => {
                self.graph();
                Ok(())
            }
            _ => Err(format!("cannot read `{}`", line.trim())),
        };
        if let Err(message) = result {
            self.say(format!("error: {message}"));
        }
        self.out.take()
    }

    /// The number of live nodes after a collection: how a test shows that a
    /// rejected command left the graph unchanged.
    pub fn live_nodes(&mut self) -> usize {
        let runtime = self.graph.runtime();
        runtime.collect_garbage();
        runtime.live_nodes()
    }

    fn say(&self, line: String) {
        self.out.borrow_mut().push(line);
    }

    /// `input x 0`: a new input whose type is the literal's.
    fn input(&mut self, name: &str, literal: &str) -> Checked<()> {
        self.fresh(name)?;
        let literal = parse_literal(literal)?;
        let shown = format!("input {literal}");
        let ty = literal.ty();
        let made = self.graph.make(Command::Input(literal));
        self.bind(name, ty, made, shown);
        Ok(())
    }

    /// `def y f a b` or `def y x`.
    fn def(&mut self, name: &str, definition: &[&str]) -> Checked<()> {
        self.fresh(name)?;
        let (ty, def) = self.check(definition)?;
        let made = self.graph.make(Command::Define(def));
        self.bind(name, ty, made, definition.join(" "));
        Ok(())
    }

    /// `set x 5`: a send to an input.
    fn set(&mut self, name: &str, literal: &str) -> Checked<()> {
        let binding = self.lookup(name)?;
        let Made::Input(made) = &binding.made else {
            return Err(format!("{name} is not an input"));
        };
        let input = made.1;
        let ty = binding.ty;
        let literal = parse_literal(literal)?;
        if literal.ty() != ty {
            return Err(format!("{name} is {ty}, and {literal} is {}", literal.ty()));
        }
        input.send(self.graph.runtime(), literal);
        Ok(())
    }

    /// `watch y`: prints `y = value` now and at every step.
    fn watch(&mut self, name: &str) -> Checked<()> {
        if self.watching.iter().any(|(watched, _)| watched == name) {
            return Err(format!("already watching {name}"));
        }
        let node = self.lookup(name)?.node();
        let out = self.out.clone();
        let label = name.to_string();
        let runtime = self.graph.runtime();
        let listener = match node {
            Node::IntCell(cell) => runtime.listen_cell(cell, move |n: &i64| {
                out.borrow_mut().push(format!("{label} = {n}"))
            }),
            Node::BoolCell(cell) => runtime.listen_cell(cell, move |p: &bool| {
                out.borrow_mut().push(format!("{label} = {p}"))
            }),
        };
        self.watching.push((name.to_string(), listener));
        Ok(())
    }

    /// `graph`: every binding, its type and its definition.
    fn graph(&self) {
        for binding in &self.bindings {
            self.say(format!(
                "{} : {} = {}",
                binding.name, binding.ty, binding.shown
            ));
        }
    }

    /// Checks a definition against the namespace and the registry, before
    /// anything touches the graph: (1) the function exists and the count of
    /// arguments is its arity, and (2) each argument's type is the one its
    /// signature names.
    fn check(&self, definition: &[&str]) -> Checked<(Type, Def)> {
        match definition {
            [word] => {
                let (ty, arg) = self.arg(word)?;
                Ok((ty, Def::Alias(arg)))
            }
            [function, words @ ..] => {
                let Some(f) = registry::lookup(function) else {
                    return Err(format!("no function named {function}"));
                };
                if words.len() != f.params.len() {
                    return Err(format!(
                        "{function} takes {} arguments, not {}",
                        f.params.len(),
                        words.len()
                    ));
                }
                let mut args = Vec::with_capacity(words.len());
                for (i, (word, param)) in words.iter().zip(f.params).enumerate() {
                    let (ty, arg) = self.arg(word)?;
                    if ty != *param {
                        return Err(format!(
                            "{function} takes {param} as argument {}, and {word} is {ty}",
                            i + 1
                        ));
                    }
                    args.push(arg);
                }
                Ok((f.result, Def::Apply(f.wire, args)))
            }
            [] => unreachable!("run passes a definition of at least one word"),
        }
    }

    /// An argument: a literal, or the name of a binding.
    fn arg(&self, word: &str) -> Checked<(Type, Arg)> {
        if let Some(literal) = Literal::parse(word) {
            return Ok((literal.ty(), Arg::Literal(literal)));
        }
        let binding = self.lookup(word)?;
        Ok((binding.ty, Arg::Binding(binding.node())))
    }

    fn lookup(&self, name: &str) -> Checked<&Binding> {
        match self.by_name.get(name) {
            Some(&i) => Ok(&self.bindings[i]),
            None => Err(format!("no binding named {name}")),
        }
    }

    /// Checks that `name` can name a new binding.
    fn fresh(&self, name: &str) -> Checked<()> {
        let mut chars = name.chars();
        let identifier = chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !identifier || Literal::parse(name).is_some() {
            return Err(format!("{name} cannot be a name"));
        }
        if self.by_name.contains_key(name) {
            return Err(format!("{name} is already bound"));
        }
        Ok(())
    }

    fn bind(&mut self, name: &str, ty: Type, made: Made, shown: String) {
        self.by_name.insert(name.to_string(), self.bindings.len());
        self.bindings.push(Binding {
            name: name.to_string(),
            ty,
            made,
            shown,
        });
    }
}

impl Default for Repl {
    fn default() -> Repl {
        Repl::new()
    }
}

fn parse_literal(word: &str) -> Checked<Literal> {
    Literal::parse(word).ok_or_else(|| format!("{word} is not a literal"))
}
