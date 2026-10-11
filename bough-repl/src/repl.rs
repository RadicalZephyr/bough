//! The REPL layer: reads a command, checks it against [`Type`] tags and
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

use crate::clock::{Clock, WallClock};
use crate::graph::{Command, Def, Graph, Made};
use crate::read::{Form, read};
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
        }
    }

    /// Registers the waker a tick's send wakes, so the driver knows to
    /// [`pump`](Repl::pump).
    pub fn set_waker(&mut self, waker: Waker) {
        self.graph.runtime().set_waker(waker);
    }

    /// Runs the ticks sent since the last pump, each a transaction of its
    /// own, and returns what they printed.
    pub fn pump(&mut self) -> Vec<String> {
        self.graph.runtime().pump();
        self.out.take()
    }

    /// RFD 1's order shuffle, for a test that shows nothing printed depends
    /// on the order nodes evaluate in.
    pub fn set_shuffle_seed(&mut self, seed: Option<u64>) {
        self.graph.runtime().set_shuffle_seed(seed);
    }

    /// Runs one line, and returns what it printed, watch lines included.
    pub fn run(&mut self, line: &str) -> Vec<String> {
        let result = match read(line) {
            Ok(None) => Ok(()),
            Ok(Some(form)) => self.command(&form),
            Err(message) => Err(message),
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

    /// One command: a list whose head names it.
    fn command(&mut self, form: &Form) -> Checked<()> {
        let Form::List(forms) = form else {
            return Err(format!("cannot read `{form}`"));
        };
        let Some((Form::Symbol(head), args)) = forms.split_first() else {
            return Err(format!("cannot read `{form}`"));
        };
        match (head.as_str(), args) {
            ("input", [name, literal]) => self.input(name, literal),
            ("def", [name, definition]) => self.def(name, definition),
            ("set", [name, literal]) => self.set(name, literal),
            ("watch", [name]) => self.watch(name),
            ("tick", [name, period]) => self.tick(name, period),
            ("graph", []) => {
                self.graph();
                Ok(())
            }
            _ => Err(format!("cannot read `{form}`")),
        }
    }

    /// `(input x 0)`: a new input whose type is the literal's.
    fn input(&mut self, name: &Form, literal: &Form) -> Checked<()> {
        let name = self.fresh(name)?;
        let literal = literal_of(literal)?;
        let shown = format!("(input {literal})");
        let ty = literal.ty();
        let made = self.graph.make(Command::Input(literal));
        self.bind(name, ty, made, shown, Vec::new());
        Ok(())
    }

    /// `(tick t 1000)`: an `Int` input that a timer sets to 1, 2, 3, ...,
    /// one every 1000 ms, starting at 0.
    fn tick(&mut self, name: &Form, period: &Form) -> Checked<()> {
        let name = self.fresh(name)?;
        let period = match period {
            Form::Literal(Literal::Int(ms)) if *ms > 0 => Duration::from_millis(*ms as u64),
            _ => return Err(format!("{period} is not a period in milliseconds")),
        };
        let made = self.graph.make(Command::Input(Literal::Int(0)));
        let Made::Input(input) = &made else {
            unreachable!("bough-repl: an input command makes an input")
        };
        let InputToken::Int(input) = input.1 else {
            unreachable!("bough-repl: an Int literal makes an Int input")
        };
        let remote = self.graph.runtime().remote_io();
        self.clock.start(period, remote, input);
        let shown = format!("(tick {})", period.as_millis());
        self.bind(name, Type::Int, made, shown, Vec::new());
        self.bindings.last_mut().expect("just bound").ticks = true;
        Ok(())
    }

    /// `(def y (f a b))` or `(def y x)`, of a new name or an existing one.
    fn def(&mut self, name: &Form, definition: &Form) -> Checked<()> {
        let shown = definition.to_string();
        let bound = match name {
            Form::Symbol(name) => self.by_name.get(name).copied(),
            _ => None,
        };
        let Some(i) = bound else {
            let name = self.fresh(name)?;
            let (ty, def) = self.check(definition)?;
            let uses = uses(definition);
            let made = self.graph.make(Command::Define(def));
            self.bind(name, ty, made, shown, uses);
            return Ok(());
        };
        let binding = &self.bindings[i];
        let Made::Defined(made) = &binding.made else {
            return Err(format!("{name} is an input, and an input is not redefined"));
        };
        let (redefine, ty) = (made.1, binding.ty);
        let (new_ty, def) = self.check(definition)?;
        let uses = uses(definition);
        // 3: no cycle, through the definitions the bindings have now.
        let name = &binding.name;
        if let Some(cycle) = self.cycle(name, &uses) {
            return Err(format!(
                "(def {name} {shown}) would make a cycle: {}",
                cycle.join(" -> ")
            ));
        }
        // 4: the type stays, since dependents were checked against it.
        if new_ty != ty {
            return Err(format!("{name} is {ty}, and {shown} is {new_ty}"));
        }
        self.graph.redefine(redefine, def);
        let binding = &mut self.bindings[i];
        binding.shown = shown;
        binding.uses = uses;
        Ok(())
    }

    /// `(set x 5)`: a send to an input.
    fn set(&mut self, name: &Form, literal: &Form) -> Checked<()> {
        let binding = self.lookup(name)?;
        let name = &binding.name;
        let Made::Input(made) = &binding.made else {
            return Err(format!("{name} is not an input"));
        };
        if binding.ticks {
            return Err(format!("{name} is a tick, which its timer sets"));
        }
        let input = made.1;
        let ty = binding.ty;
        let literal = literal_of(literal)?;
        if literal.ty() != ty {
            return Err(format!("{name} is {ty}, and {literal} is {}", literal.ty()));
        }
        input.send(self.graph.runtime(), literal);
        Ok(())
    }

    /// `(watch y)`: prints `y = value` now and at every step.
    fn watch(&mut self, name: &Form) -> Checked<()> {
        let binding = self.lookup(name)?;
        let (name, node) = (binding.name.clone(), binding.node());
        if self.watching.iter().any(|(watched, _)| *watched == name) {
            return Err(format!("already watching {name}"));
        }
        let print = Printer {
            out: self.out.clone(),
            label: name.clone(),
        };
        let runtime = self.graph.runtime();
        let listener = match node {
            Node::IntCell(cell) => runtime.listen_cell(cell, move |n| print.line(n)),
            Node::BoolCell(cell) => runtime.listen_cell(cell, move |p| print.line(p)),
            Node::StrCell(cell) => runtime.listen_cell(cell, move |text| print.line(text)),
        };
        self.watching.push((name, listener));
        Ok(())
    }

    /// `(graph)`: every binding, its type and its definition.
    fn graph(&self) {
        for binding in &self.bindings {
            self.say(format!(
                "{} : {} = {}",
                binding.name, binding.ty, binding.shown
            ));
        }
    }

    /// Checks a definition against the namespace and the registry, before
    /// anything touches the graph: a literal, the name of a binding, or an
    /// application, of which (1) the function exists and the count of
    /// arguments is its arity, and (2) each argument checks, and its type is
    /// the one the signature names, for one of the signatures its name has.
    /// Returns the result's type and the definition.
    fn check(&self, definition: &Form) -> Checked<(Type, Def)> {
        let forms = match definition {
            Form::Literal(literal) => return Ok((literal.ty(), Def::Literal(literal.clone()))),
            Form::Symbol(_) => {
                let binding = self.lookup(definition)?;
                return Ok((binding.ty, Def::Binding(binding.node())));
            }
            Form::List(forms) => forms,
        };
        let Some((Form::Symbol(function), args)) = forms.split_first() else {
            return Err(format!(
                "{definition} does not start with a function's name"
            ));
        };
        let candidates: Vec<&Function> = registry::named(function).collect();
        let Some(first) = candidates.first() else {
            return Err(format!("no function named {function}"));
        };
        // The signatures of one name share an arity.
        if args.len() != first.params.len() {
            return Err(format!(
                "{function} takes {} arguments, not {}",
                first.params.len(),
                args.len()
            ));
        }
        let mut types = Vec::with_capacity(args.len());
        let mut checked = Vec::with_capacity(args.len());
        for form in args {
            let (ty, arg) = self.check(form)?;
            types.push(ty);
            checked.push(arg);
        }
        if let Some(f) = candidates.iter().find(|f| f.params == types) {
            return Ok((f.result, Def::Apply(f.wire, checked)));
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
                    args[i],
                    types[i]
                )
            }
            _ => {
                let takes: Vec<String> = candidates.iter().map(|f| signature(f.params)).collect();
                let args: Vec<String> = args.iter().map(Form::to_string).collect();
                format!(
                    "{function} takes {}, and {} is {}",
                    takes.join(" or "),
                    args.join(" "),
                    signature(&types)
                )
            }
        })
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

    /// The binding a form names.
    fn lookup(&self, name: &Form) -> Checked<&Binding> {
        let found = match name {
            Form::Symbol(name) => self.by_name.get(name),
            _ => None,
        };
        match found {
            Some(&i) => Ok(&self.bindings[i]),
            None => Err(format!("no binding named {name}")),
        }
    }

    /// Checks that a form can name a new binding, and returns the name.
    fn fresh<'f>(&self, name: &'f Form) -> Checked<&'f str> {
        let Form::Symbol(name) = name else {
            return Err(format!("{name} cannot be a name"));
        };
        let mut chars = name.chars();
        let identifier = chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !identifier {
            return Err(format!("{name} cannot be a name"));
        }
        if self.by_name.contains_key(name) {
            return Err(format!("{name} is already bound"));
        }
        Ok(name)
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

fn literal_of(form: &Form) -> Checked<Literal> {
    match form {
        Form::Literal(literal) => Ok(literal.clone()),
        _ => Err(format!("{form} is not a literal")),
    }
}

/// The bindings a checked definition names, at any depth, for the cycle
/// check.
fn uses(definition: &Form) -> Vec<String> {
    let mut names = Vec::new();
    named(definition, &mut names);
    names
}

fn named(form: &Form, names: &mut Vec<String>) {
    match form {
        Form::Symbol(name) => names.push(name.clone()),
        // The head of a checked list is a function's name, not a binding's.
        Form::List(forms) => forms.iter().skip(1).for_each(|form| named(form, names)),
        Form::Literal(_) => {}
    }
}
