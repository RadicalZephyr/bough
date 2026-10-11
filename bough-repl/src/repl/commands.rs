//! The commands: the table [`Repl::command`] dispatches through, and the
//! help each command and function shows. Help is written as Markdown:
//! parameter names and code in backticks, examples indented four spaces.
//!
//! Every command's name, and every help topic, is reserved: no binding or
//! defgraph can take one, so `(help name)` always means one thing.

use crate::read::Form;
use crate::registry::{self, Function};

use super::{Checked, Repl, signature};

/// A command, its help, and what runs it.
pub(super) struct Spec {
    name: &'static str,
    /// The command with named arguments, such as `(def name expr)`.
    usage: &'static str,
    /// One line, for `(help commands)`.
    summary: &'static str,
    /// The lines of `(help name)`, after the usage.
    detail: &'static [&'static str],
    /// The fewest and the most arguments it takes.
    arity: (usize, usize),
    /// Runs it, with a count of arguments in `arity`.
    run: fn(&mut Repl, &[Form]) -> Checked<()>,
}

const COMMANDS: &[Spec] = &[
    Spec {
        name: "input",
        usage: "(input name literal)",
        summary: "declare an input; `literal` gives its type",
        detail: &[
            "Declares an input named `name`, starting at `literal`. Its type is",
            "the literal's: an integer is `Int`, `true` or `false` is `Bool`, and",
            "`\"text\"` is `Str`. Send it new values with `(set name literal)`. An",
            "input cannot be redefined.",
            "",
            "    (input n 0)",
            "    (input greeting \"hi\")",
        ],
        arity: (2, 2),
        run: |repl, args| repl.input(&args[0], &args[1]),
    },
    Spec {
        name: "tick",
        usage: "(tick name ms)",
        summary: "an `Int` input a timer counts up, one step every `ms`",
        detail: &[
            "Declares an `Int` input named `name` that starts at 0, and that a",
            "timer sets to 1, 2, 3, ..., one every `ms` milliseconds, each step a",
            "transaction of its own. Only its timer sets it.",
            "",
            "    (tick t 1000)",
        ],
        arity: (2, 2),
        run: |repl, args| repl.tick(&args[0], &args[1]),
    },
    Spec {
        name: "set",
        usage: "(set name literal)",
        summary: "send a value to an input",
        detail: &[
            "Sends `literal` to the input `name`, in a transaction of its own.",
            "`literal` must have the input's type, and a tick can't be set.",
            "",
            "    (set n 5)",
        ],
        arity: (2, 2),
        run: |repl, args| repl.set(&args[0], &args[1]),
    },
    Spec {
        name: "def",
        usage: "(def name expr)",
        summary: "bind `name` to an expression, or rebind it",
        detail: &[
            "Binds `name` to `expr`. `expr` can be a literal, a binding's name,",
            "or a call `(f arg ...)`, whose arguments are expressions too. Each",
            "call builds a cell, and only `name` is a binding.",
            "",
            "Redefining a name swaps its definition in place, under anything",
            "running, and everything downstream follows. The type must stay the",
            "same, and the new definition must not make a cycle.",
            "",
            "    (def y (add x 1))",
            "    (def label (if (eq (mod x 3) 0) \"Fizz\" (str x)))",
        ],
        arity: (2, 2),
        run: |repl, args| repl.def(&args[0], &args[1]),
    },
    Spec {
        name: "defgraph",
        usage: "(defgraph name [param ...] expr)",
        summary: "name a piece of graph, built afresh at each call",
        detail: &[
            "Names `expr`, which a call `(name arg ...)` builds afresh, with each",
            "argument for its parameter. The body names only its parameters; its",
            "types are checked at each call. Each argument is built once, however",
            "often the body names it. A defgraph cannot be redefined, since what was",
            "built from it would not follow.",
            "",
            "    (defgraph word-for-mod-n [x n word] (if (eq (mod x n) 0) word \"\"))",
            "    (def fizz (word-for-mod-n x 3 \"Fizz\"))",
        ],
        arity: (3, 3),
        run: |repl, args| repl.defgraph(&args[0], &args[1], &args[2]),
    },
    Spec {
        name: "watch",
        usage: "(watch name)",
        summary: "print a binding's value now and at every step",
        detail: &[
            "Prints `name = value` now, and again at every step of `name`.",
            "",
            "    (watch label)",
        ],
        arity: (1, 1),
        run: |repl, args| repl.watch(&args[0]),
    },
    Spec {
        name: "graph",
        usage: "(graph)",
        summary: "list every defgraph and binding",
        detail: &[
            "Lists every defgraph as written, then every binding as",
            "`name : Type = definition`, in the order they were made.",
        ],
        arity: (0, 0),
        run: |repl, _| {
            repl.graph();
            Ok(())
        },
    },
    Spec {
        name: "help",
        usage: "(help topic)",
        summary: "this help",
        detail: &[
            "`(help)` lists the topics. `(help commands)` and `(help fns)` list",
            "the commands and the functions, `(help all)` both, and `(help name)`",
            "shows one in detail. The topic names and the command names are",
            "reserved, so no binding or defgraph can take them.",
        ],
        arity: (0, 1),
        run: |repl, args| repl.help(args.first()),
    },
];

/// The topics of `help` besides a command's or a function's name.
const TOPICS: [&str; 3] = ["commands", "fns", "all"];

/// What a bare `(help)` prints.
const INDEX: [[&str; 2]; 4] = [
    ["(help commands)", "every command, one line each"],
    [
        "(help fns)",
        "every function: the registry's, and your defgraphs",
    ],
    ["(help all)", "both lists"],
    ["(help name)", "one command or function in detail"],
];

/// Why no binding or defgraph can take `name`, if none can.
pub(super) fn reserved(name: &str) -> Option<&'static str> {
    if COMMANDS.iter().any(|command| command.name == name) {
        Some("a command")
    } else if TOPICS.contains(&name) {
        Some("a help topic")
    } else {
        None
    }
}

impl Repl {
    /// One command: a list whose head names it, with the count of
    /// arguments it takes.
    pub(super) fn command(&mut self, form: &Form) -> Checked<()> {
        let cannot = || format!("cannot read `{form}`: a command is a list, such as `(help)`");
        let Form::List(forms) = form else {
            return Err(cannot());
        };
        let Some((Form::Symbol(head), args)) = forms.split_first() else {
            return Err(cannot());
        };
        let Some(command) = COMMANDS.iter().find(|command| command.name == head) else {
            return Err(format!(
                "no command named {head}; `(help commands)` lists them"
            ));
        };
        let (fewest, most) = command.arity;
        if !(fewest..=most).contains(&args.len()) {
            let takes = if fewest == most {
                fewest.to_string()
            } else {
                format!("{fewest} or {most}")
            };
            return Err(format!(
                "{head} takes {takes} arguments, not {}: {}",
                args.len(),
                command.usage
            ));
        }
        (command.run)(self, args)
    }

    /// `(help)`, `(help commands)`, `(help fns)`, `(help all)` or
    /// `(help name)`.
    fn help(&mut self, topic: Option<&Form>) -> Checked<()> {
        let Some(topic) = topic else {
            self.say_all(table(INDEX.map(|row| row.map(String::from))));
            return Ok(());
        };
        let Form::Symbol(name) = topic else {
            return Err(format!("{topic} is not a topic; `(help)` lists them"));
        };
        let lines = match name.as_str() {
            "commands" => commands(),
            "fns" => self.fns(),
            "all" => {
                let mut lines = commands();
                lines.push(String::new());
                lines.extend(self.fns());
                lines
            }
            name => self.detail(name).ok_or_else(|| {
                format!("no command or function named {name}; `(help)` lists the topics")
            })?,
        };
        self.say_all(lines);
        Ok(())
    }

    /// The registry's functions, then the defgraphs, apart.
    fn fns(&self) -> Vec<String> {
        let mut rows = Vec::new();
        let mut functions = registry::all().iter().peekable();
        while let Some(first) = functions.next() {
            rows.push(vec![
                first.doc.usage.to_string(),
                typed(first),
                first.doc.summary.to_string(),
            ]);
            while let Some(overload) = functions.next_if(|f| f.name == first.name) {
                rows.push(vec![String::new(), typed(overload), String::new()]);
            }
        }
        let mut lines = table(rows);
        if !self.graphdefs.is_empty() {
            lines.push(String::new());
            lines.extend(table(
                self.graphdefs
                    .iter()
                    .map(|graph| vec![graph.usage(), "defgraph".to_string()]),
            ));
        }
        lines
    }

    /// `(help name)` of a command, a function, or a defgraph.
    fn detail(&self, name: &str) -> Option<Vec<String>> {
        if let Some(command) = COMMANDS.iter().find(|command| command.name == name) {
            let mut lines = vec![command.usage.to_string(), String::new()];
            lines.extend(command.detail.iter().map(|line| line.to_string()));
            return Some(lines);
        }
        let mut functions = registry::named(name).peekable();
        if let Some(first) = functions.peek() {
            let mut lines = vec![first.doc.usage.to_string()];
            let detail = first.doc.detail;
            lines.extend(functions.map(|f| format!("    {}", typed(f))));
            lines.push(String::new());
            lines.extend(detail.iter().map(|line| line.to_string()));
            return Some(lines);
        }
        let graph = self.graphdef(name)?;
        Some(vec![
            graph.usage(),
            String::new(),
            format!("    {}", graph.shown),
        ])
    }

    fn say_all(&self, lines: Vec<String>) {
        for line in lines {
            self.say(line);
        }
    }
}

/// The space between two columns.
const GAP: &str = "   ";

/// `(help commands)`.
fn commands() -> Vec<String> {
    table(
        COMMANDS
            .iter()
            .map(|command| vec![command.usage.to_string(), command.summary.to_string()]),
    )
}

/// A signature with its result, as `(Int, Int) -> Bool`.
fn typed(f: &Function) -> String {
    format!("{} -> {}", signature(f.params), f.result)
}

/// Rows of cells, each column but the last padded to its widest cell.
fn table<R: IntoIterator<Item = String>>(rows: impl IntoIterator<Item = R>) -> Vec<String> {
    let rows: Vec<Vec<String>> = rows
        .into_iter()
        .map(|row| row.into_iter().collect())
        .collect();
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..columns.saturating_sub(1))
        .map(|i| rows.iter().map(|row| row[i].len()).max().unwrap_or(0))
        .collect();
    rows.iter()
        .map(|row| {
            let mut line = String::new();
            for (i, cell) in row.iter().enumerate() {
                match widths.get(i) {
                    Some(width) => line.push_str(&format!("{cell:<width$}{GAP}")),
                    None => line.push_str(cell),
                }
            }
            line.trim_end().to_string()
        })
        .collect()
}
