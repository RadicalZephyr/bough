//! The REPL layer: parses a command, checks it against [`Type`] tags and
//! the binding namespace, and only then touches the graph. It never handles
//! a binding's value; a literal in a command is the one value it carries,
//! into the graph.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::rc::Rc;
use std::task::Waker;
use std::time::Duration;

use bough::Listener;
#[cfg(any(feature = "undo", feature = "stage"))]
use bough::{PumpError, SendError};

use crate::clock::{Clock, WallClock};
use bough::Input;

use crate::graph::{Arg, Command, Def, Graph, Made};
use crate::registry::{self, Function};
use crate::ty::{InputToken, Literal, Node, Type};

/// A name with a fixed [`Type`] and a [`Node`].
struct Binding {
    name: String,
    ty: Type,
    made: Made,
    /// The definition as typed, for `graph`.
    shown: String,
    /// The bindings the definition names, for the cycle check.
    uses: Vec<String>,
    /// Whether a timer drives it.
    ticks: bool,
}

impl Binding {
    fn node(&self) -> Node {
        match &self.made {
            Made::Input(made) => made.0,
            Made::Defined(made) => made.0,
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
    clock: Box<dyn Clock>,
    /// The rollback probe: the engine refuses a command or a tick that
    /// fails, and the REPL prints the refusal, instead of dying.
    #[cfg(any(feature = "undo", feature = "stage"))]
    rollback: bool,
}

type Checked<T> = Result<T, String>;

impl Repl {
    /// A REPL with an empty graph, whose ticks run on the wall clock.
    pub fn new() -> Repl {
        Repl::with_clock(WallClock)
    }

    /// A REPL whose ticks run on `clock`.
    pub fn with_clock(clock: impl Clock + 'static) -> Repl {
        Repl {
            graph: Graph::new(),
            bindings: Vec::new(),
            by_name: HashMap::new(),
            watching: Vec::new(),
            out: Rc::default(),
            clock: Box::new(clock),
            #[cfg(any(feature = "undo", feature = "stage"))]
            rollback: false,
        }
    }

    /// The rollback probe: with it on, the engine refuses a command or a
    /// tick that fails and leaves the graph as it was, and the REPL prints
    /// the refusal as an error.
    #[cfg(any(feature = "undo", feature = "stage"))]
    pub fn set_rollback(&mut self, on: bool) {
        self.rollback = on;
        self.graph.runtime().set_rollback(on);
    }

    /// Registers the waker a tick's send wakes, so the driver knows to
    /// [`pump`](Repl::pump).
    pub fn set_waker(&mut self, waker: Waker) {
        self.graph.runtime().set_waker(waker);
    }

    /// Runs the ticks sent since the last pump, each a transaction of its
    /// own, and returns what they printed. With rollback on, a tick the
    /// engine refuses prints its refusal once, its event is dropped, and
    /// the ticks after it run.
    pub fn pump(&mut self) -> Vec<String> {
        #[cfg(any(feature = "undo", feature = "stage"))]
        if self.rollback {
            loop {
                match self.graph.runtime().try_pump() {
                    Ok(()) => break,
                    Err(PumpError::Refused(refusal)) => self.say(format!("error: {refusal}")),
                    Err(error) => {
                        self.say(format!("error: {error}"));
                        break;
                    }
                }
            }
            return self.out.take();
        }
        self.graph.runtime().pump();
        self.out.take()
    }

    /// Runs a command in the graph. With rollback on, a refusal is an
    /// error, which `run` prints; without it, a failure in the graph
    /// panics as it always has.
    fn make(&mut self, command: Command) -> Checked<Made> {
        #[cfg(any(feature = "undo", feature = "stage"))]
        if self.rollback {
            return self.graph.try_make(command).map_err(refused);
        }
        Ok(self.graph.make(command))
    }

    /// Sends a redefinition, as [`make`](Repl::make) runs a command.
    fn redefine(&mut self, binding: Input<Def>, def: Def) -> Checked<()> {
        #[cfg(any(feature = "undo", feature = "stage"))]
        if self.rollback {
            return self.graph.try_redefine(binding, def).map_err(refused);
        }
        self.graph.redefine(binding, def);
        Ok(())
    }

    /// Sends a literal to an input, as [`make`](Repl::make) runs a command.
    fn send(&mut self, input: InputToken, literal: Literal) -> Checked<()> {
        #[cfg(any(feature = "undo", feature = "stage"))]
        if self.rollback {
            return input
                .try_send(self.graph.runtime(), literal)
                .map_err(refused);
        }
        input.send(self.graph.runtime(), literal);
        Ok(())
    }

    /// RFD 1's order shuffle, for a test that shows nothing printed depends
    /// on the order nodes evaluate in.
    pub fn set_shuffle_seed(&mut self, seed: Option<u64>) {
        self.graph.runtime().set_shuffle_seed(seed);
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
            ["tick", name, period] => self.tick(name, period),
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
        let made = self.make(Command::Input(literal))?;
        self.bind(name, ty, made, shown, Vec::new());
        Ok(())
    }

    /// `tick t 1000`: an `Int` input that a timer sets to 1, 2, 3, ..., one
    /// every 1000 ms, starting at 0.
    fn tick(&mut self, name: &str, period: &str) -> Checked<()> {
        self.fresh(name)?;
        let period = match period.parse::<u64>() {
            Ok(ms) if ms > 0 => Duration::from_millis(ms),
            _ => return Err(format!("{period} is not a period in milliseconds")),
        };
        let made = self.make(Command::Input(Literal::Int(0)))?;
        let Made::Input(input) = &made else {
            unreachable!("bough-repl: an input command makes an input")
        };
        let InputToken::Int(input) = input.1 else {
            unreachable!("bough-repl: an Int literal makes an Int input")
        };
        let remote = self.graph.runtime().remote_io();
        self.clock.start(period, remote, input);
        let shown = format!("tick {}", period.as_millis());
        self.bind(name, Type::Int, made, shown, Vec::new());
        self.bindings.last_mut().expect("just bound").ticks = true;
        Ok(())
    }

    /// `def y f a b` or `def y x`, of a new name or an existing one.
    fn def(&mut self, name: &str, definition: &[&str]) -> Checked<()> {
        let shown = definition.join(" ");
        let Some(&i) = self.by_name.get(name) else {
            self.fresh(name)?;
            let (ty, def, uses) = self.check(definition)?;
            let made = self.make(Command::Define(def))?;
            self.bind(name, ty, made, shown, uses);
            return Ok(());
        };
        let binding = &self.bindings[i];
        let Made::Defined(made) = &binding.made else {
            return Err(format!("{name} is an input, and an input is not redefined"));
        };
        let (redefine, ty) = (made.1, binding.ty);
        let (new_ty, def, uses) = self.check(definition)?;
        // 3: no cycle, through the definitions the bindings have now.
        if let Some(cycle) = self.cycle(name, &uses) {
            return Err(format!(
                "def {name} {shown} would make a cycle: {}",
                cycle.join(" -> ")
            ));
        }
        // 4: the type stays, since dependents were checked against it.
        if new_ty != ty {
            return Err(format!("{name} is {ty}, and {shown} is {new_ty}"));
        }
        self.redefine(redefine, def)?;
        let binding = &mut self.bindings[i];
        binding.shown = shown;
        binding.uses = uses;
        Ok(())
    }

    /// `set x 5`: a send to an input.
    fn set(&mut self, name: &str, literal: &str) -> Checked<()> {
        let binding = self.lookup(name)?;
        let Made::Input(made) = &binding.made else {
            return Err(format!("{name} is not an input"));
        };
        if binding.ticks {
            return Err(format!("{name} is a tick, which its timer sets"));
        }
        let input = made.1;
        let ty = binding.ty;
        let literal = parse_literal(literal)?;
        if literal.ty() != ty {
            return Err(format!("{name} is {ty}, and {literal} is {}", literal.ty()));
        }
        self.send(input, literal)
    }

    /// `watch y`: prints `y = value` now and at every step.
    fn watch(&mut self, name: &str) -> Checked<()> {
        if self.watching.iter().any(|(watched, _)| watched == name) {
            return Err(format!("already watching {name}"));
        }
        let node = self.lookup(name)?.node();
        let print = Printer {
            out: self.out.clone(),
            label: name.to_string(),
        };
        let runtime = self.graph.runtime();
        let listener = match node {
            Node::IntCell(cell) => runtime.listen_cell(cell, move |n| print.line(n)),
            Node::BoolCell(cell) => runtime.listen_cell(cell, move |p| print.line(p)),
            Node::StrCell(cell) => runtime.listen_cell(cell, move |text| print.line(text)),
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
    /// signature names, for one of the signatures its name has. Returns the
    /// result's type, the definition, and the bindings it names.
    fn check(&self, definition: &[&str]) -> Checked<(Type, Def, Vec<String>)> {
        let uses = definition
            .iter()
            .skip(usize::from(definition.len() > 1))
            .filter(|word| Literal::parse(word).is_none())
            .map(|word| word.to_string())
            .collect();
        match definition {
            [word] => {
                let (ty, arg) = self.arg(word)?;
                Ok((ty, Def::Alias(arg), uses))
            }
            [function, words @ ..] => {
                let candidates: Vec<&Function> = registry::named(function).collect();
                let Some(first) = candidates.first() else {
                    return Err(format!("no function named {function}"));
                };
                // The signatures of one name share an arity.
                if words.len() != first.params.len() {
                    return Err(format!(
                        "{function} takes {} arguments, not {}",
                        first.params.len(),
                        words.len()
                    ));
                }
                let mut types = Vec::with_capacity(words.len());
                let mut args = Vec::with_capacity(words.len());
                for word in words {
                    let (ty, arg) = self.arg(word)?;
                    types.push(ty);
                    args.push(arg);
                }
                if let Some(f) = candidates.iter().find(|f| f.params == types) {
                    return Ok((f.result, Def::Apply(f.wire, args), uses));
                }
                Err(match candidates.as_slice() {
                    [f] => {
                        let i = (0..types.len())
                            .find(|&i| f.params[i] != types[i])
                            .expect("an argument of another type");
                        format!(
                            "{function} takes {} as argument {}, and {} is {}",
                            f.params[i],
                            i + 1,
                            words[i],
                            types[i]
                        )
                    }
                    _ => {
                        let takes: Vec<String> =
                            candidates.iter().map(|f| signature(f.params)).collect();
                        format!(
                            "{function} takes {}, and {} is {}",
                            takes.join(" or "),
                            words.join(" "),
                            signature(&types)
                        )
                    }
                })
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

    /// The path from `name` back to itself, if a definition naming `uses`
    /// would close one.
    fn cycle(&self, name: &str, uses: &[String]) -> Option<Vec<String>> {
        let mut path = vec![name.to_string()];
        let mut seen = HashSet::new();
        self.reaches(uses, name, &mut path, &mut seen)
            .then_some(path)
    }

    fn reaches(
        &self,
        from: &[String],
        target: &str,
        path: &mut Vec<String>,
        seen: &mut HashSet<String>,
    ) -> bool {
        for next in from {
            path.push(next.clone());
            if next == target {
                return true;
            }
            if seen.insert(next.clone()) {
                let uses = &self.bindings[self.by_name[next]].uses;
                if self.reaches(uses, target, path, seen) {
                    return true;
                }
            }
            path.pop();
        }
        false
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

    fn bind(&mut self, name: &str, ty: Type, made: Made, shown: String, uses: Vec<String>) {
        self.by_name.insert(name.to_string(), self.bindings.len());
        self.bindings.push(Binding {
            name: name.to_string(),
            ty,
            made,
            shown,
            uses,
            ticks: false,
        });
    }
}

impl Default for Repl {
    fn default() -> Repl {
        Repl::new()
    }
}

/// What a watch's listener prints with.
struct Printer {
    out: Rc<RefCell<Vec<String>>>,
    label: String,
}

impl Printer {
    fn line(&self, value: &impl fmt::Display) {
        let line = format!("{} = {value}", self.label);
        self.out.borrow_mut().push(line);
    }
}

/// A signature's parameter types, as `(Bool, Int, Int)`.
fn signature(types: &[Type]) -> String {
    let types: Vec<String> = types.iter().map(Type::to_string).collect();
    format!("({})", types.join(", "))
}

/// A send's error as the REPL prints it: a refusal says what failed.
#[cfg(any(feature = "undo", feature = "stage"))]
fn refused(error: SendError) -> String {
    match error {
        SendError::Refused(refusal) => refusal.to_string(),
        other => other.to_string(),
    }
}

fn parse_literal(word: &str) -> Checked<Literal> {
    Literal::parse(word).ok_or_else(|| format!("{word} is not a literal"))
}
