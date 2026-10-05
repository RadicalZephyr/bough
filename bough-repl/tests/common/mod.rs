//! The scripted-input harness the completion tests share.

#![allow(dead_code)]

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use bough::{Input, RemoteIo};
use bough_repl::Repl;
use bough_repl::clock::Clock;
use bough_repl::graph::{Arg, Command, Def, Graph, Made};
use bough_repl::registry;
use bough_repl::ty::{InputToken, Literal, Node};

/// A script step that fires every timer once and pumps, in place of a
/// command.
pub const TICK: &str = "<tick>";

/// Runs each command and checks what it printed. Lines are compared as
/// sorted lists: two watches that step in one transaction print in the
/// order their listeners run, which RFD 2 says not to rely on.
pub fn transcript(repl: &mut Repl, steps: &[(&str, &[&str])]) {
    ticking(repl, &Ticks::default(), steps);
}

/// [`transcript`], where a [`TICK`] step fires the timers of `ticks`.
pub fn ticking(repl: &mut Repl, ticks: &Ticks, steps: &[(&str, &[&str])]) {
    for (command, expected) in steps {
        let mut printed = if *command == TICK {
            ticks.fire();
            repl.pump()
        } else {
            repl.run(command)
        };
        let mut expected: Vec<&str> = expected.to_vec();
        printed.sort();
        expected.sort();
        assert_eq!(printed, expected, "after `{command}`");
    }
}

/// The `graph` listing, in definition order.
pub fn listing(repl: &mut Repl) -> Vec<String> {
    repl.run("graph")
}

/// A REPL whose timers fire when the test says.
pub fn with_manual_clock() -> (Repl, Ticks) {
    let ticks = Ticks::default();
    (Repl::with_clock(ManualClock(ticks.0.clone())), ticks)
}

struct Timer {
    step: Sender<()>,
    sent: Receiver<()>,
}

/// A clock whose timers are threads that send the next count through
/// their remote when the test fires them, and say when they have.
struct ManualClock(Rc<RefCell<Vec<Timer>>>);

impl Clock for ManualClock {
    fn start(&mut self, _period: Duration, remote: RemoteIo, input: Input<i64>) {
        let (step, steps) = mpsc::channel();
        let (sent_by, sent) = mpsc::channel();
        thread::spawn(move || {
            for count in 1.. {
                if steps.recv().is_err() {
                    break;
                }
                remote.send(input, count).expect("the runtime is there");
                if sent_by.send(()).is_err() {
                    break;
                }
            }
        });
        self.0.borrow_mut().push(Timer { step, sent });
    }
}

/// The test's side of the manual clock.
#[derive(Default)]
pub struct Ticks(Rc<RefCell<Vec<Timer>>>);

impl Ticks {
    /// Fires every timer once, and returns when each has sent; the sends
    /// wait for the REPL's pump.
    pub fn fire(&self) {
        for timer in self.0.borrow().iter() {
            timer.step.send(()).expect("the timer is running");
            timer.sent.recv().expect("the timer sent");
        }
    }

    /// How many timers have been started.
    pub fn timers(&self) -> usize {
        self.0.borrow().len()
    }
}

/// A registry function over arguments, for driving the graph API past the
/// REPL.
pub fn apply(function: &str, args: Vec<Arg>) -> Def {
    Def::Apply(registry::named(function).next().unwrap().wire, args)
}

/// A new defined binding, kept for the graph's life.
pub fn define(graph: &mut Graph, def: Def) -> (Node, Input<Def>) {
    match graph.make(Command::Define(def)) {
        Made::Defined(made) => made.keep(),
        Made::Input(_) => unreachable!(),
    }
}

/// A new `Int` input, kept for the graph's life.
pub fn int_input(graph: &mut Graph, n: i64) -> (Node, Input<i64>) {
    match graph.make(Command::Input(Literal::Int(n))) {
        Made::Input(made) => match made.keep() {
            (node, InputToken::Int(input)) => (node, input),
            _ => unreachable!(),
        },
        Made::Defined(_) => unreachable!(),
    }
}

/// The message of the panic `f` makes; a panic is expected.
pub fn panic_message(f: impl FnOnce()) -> String {
    let payload = catch_unwind(AssertUnwindSafe(f)).expect_err("a panic");
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => payload.downcast::<&str>().unwrap().to_string(),
    }
}

/// What the watches of [`watch`] print, `name = value`, in the order
/// their listeners ran.
pub type Log = Rc<RefCell<Vec<String>>>;

/// Watches an `Int` binding as the REPL does, printing into `log`: its
/// value now, and at every step.
pub fn watch(graph: &mut Graph, log: &Log, name: &str, node: Node) {
    let Node::IntCell(cell) = node else {
        unreachable!("the probe watches Int bindings")
    };
    let (log, name) = (log.clone(), name.to_string());
    graph
        .runtime()
        .listen_cell(cell, move |n| {
            log.borrow_mut().push(format!("{name} = {n}"))
        })
        .keep();
}

/// An `Int` binding's value now.
pub fn sample_int(graph: &mut Graph, node: Node) -> i64 {
    let Node::IntCell(cell) = node else {
        unreachable!("the probe reads Int bindings")
    };
    *graph.runtime().sample(cell)
}

/// What a refused transaction must leave as it was: every live node's
/// structure, the live count, and the values of some bindings.
#[cfg(any(feature = "undo", feature = "stage"))]
pub struct Before {
    topology: bough::Topology,
    live: usize,
    values: Vec<i64>,
}

/// A graph for the rollback probe: rollback on, and collection only when a
/// test asks, so a live count compared across a refusal shows that what
/// the refused transaction made was freed at once.
#[cfg(any(feature = "undo", feature = "stage"))]
pub fn rollback_graph() -> Graph {
    let mut graph = Graph::new();
    let runtime = graph.runtime();
    runtime.set_rollback(true);
    runtime.set_collection_policy(bough::CollectionPolicy::Manual);
    graph
}

/// Collects, and records what [`Before`] holds.
#[cfg(any(feature = "undo", feature = "stage"))]
pub fn before(graph: &mut Graph, nodes: &[Node]) -> Before {
    graph.runtime().collect_garbage();
    let values = nodes.iter().map(|&n| sample_int(graph, n)).collect();
    let runtime = graph.runtime();
    Before {
        topology: runtime.topology(),
        live: runtime.live_nodes(),
        values,
    }
}

/// Asserts the graph is as [`before`] found it, with no collection since.
#[cfg(any(feature = "undo", feature = "stage"))]
pub fn assert_as_before(graph: &mut Graph, nodes: &[Node], before: &Before) {
    let runtime = graph.runtime();
    assert_eq!(runtime.live_nodes(), before.live, "the live count");
    assert_eq!(
        runtime.topology(),
        before.topology,
        "every live node's structure"
    );
    let values: Vec<i64> = nodes.iter().map(|&n| sample_int(graph, n)).collect();
    assert_eq!(values, before.values, "the bindings' values");
}

/// The refusal a send ended in; anything else fails the test.
#[cfg(any(feature = "undo", feature = "stage"))]
pub fn refusal<T: std::fmt::Debug>(sent: Result<T, bough::SendError>) -> bough::Refusal {
    match sent {
        Err(bough::SendError::Refused(refusal)) => refusal,
        other => panic!("expected a refusal, got {other:?}"),
    }
}
