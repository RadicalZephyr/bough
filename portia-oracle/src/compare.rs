//! The engine against the oracle, transaction by transaction.
//!
//! [`expected`] reads the oracle's answer to a program, window
//! `FromFirstTransaction`, as what each listener should see: a stream's
//! events in transaction k, and a cell's value after transaction zero, its
//! steps in transaction k, and its value after it. Transaction k is the
//! instant `[k]` and its child transactions, `[k, 0]`, `[k, 0, 0]`, `[k, 1]`
//! and so on, so its events are all those whose time starts with k, in time
//! order.
//!
//! [`compare`] holds one engine run to it. For a cell that is `listen_cell`'s
//! one call at registration, its calls during transaction k, which are the
//! steps, a step to an equal value included; `listen_steps`'s calls, the
//! same steps and nothing at registration; and `graph.sample` after
//! transaction k and its children, the value after the last of them. For a
//! stream it is the events of transaction k.
//!
//! # What child transactions leave unchecked
//!
//! The engine cannot tell I/O code which child instant a listener call came
//! from: RFD 1's affordances expose no child index. So the times of the
//! oracle's events are sorted, and then dropped: per observed node and per
//! external transaction, what is compared is the ordered list of events, or
//! of steps. Child indices are not compared. That leaves unchecked, for any
//! one node, which child instant each event or step fell in, as long as
//! their order is right: a node whose events the oracle puts at `[1, 0]`
//! and `[1, 1]` agrees with an engine that fired them at `[1, 0]` and
//! `[1, 0, 0]`, or at two instants the semantics do not have. It also
//! leaves unchecked whether two nodes' events are simultaneous, except
//! through a node that combines them, such as a merge, which the
//! comparison sees.
//!
//! One thing about times is checked without child indices: the order of the
//! calls across nodes. The engine runs each child instant's listeners after
//! its commit and the child instants in time order, so a listener call for
//! an event at an earlier time must come before one for an event at a later
//! time, whatever nodes they are on. [`compare`] reads each call's time
//! from the oracle's answer, where the per-node lists agree, and reports a
//! call that comes after a call for a later time. Calls for one time may
//! come in any order.
//!
//! [`check_program`] asks the oracle once and runs the engine every way it
//! is given. A failure is a [`Report`]: the program as Rust and as the
//! Haskell line, its schedule, and every observed node, the engine beside
//! the oracle, a row per transaction, with the rows that differ marked.
//!
//! [`check_fed`] holds RFD 7's fold law to the oracle the same way: one
//! program input is fed through an input slot, by writes that pumps cut
//! into runs, and the other inputs through `graph.transaction` between
//! them. The oracle answers the same program with one transaction per run,
//! whose sends are the run's writes to an input that coalesces with the
//! slot's fold, as an expression; so the engine's slot must fold each run
//! left, the first write on the left, as the semantics fold simultaneous
//! sends.
//!
//! # What the switches did
//!
//! A switch whose outer never selects another inner tests little, and the
//! answer does not say what a switch followed: a cell of tokens cannot be
//! observed. [`watch_switches`] makes a second program for the oracle, the
//! same nodes and more, which observes each switch's selections, as the
//! index its pick or map_cell takes, and every token its outer may select.
//! [`SwitchWatch::count`] reads the answer: which switches the comparison
//! sees moved to another inner, and at how many instants the new inner or
//! the old one fired or stepped at the instant of the move, which is where
//! a switch is hardest to get right.

use std::fmt::{self, Write as _};
use std::ops::AddAssign;
use std::panic::{self, AssertUnwindSafe};

use crate::answer::{Answer, Datum, Observation};
use crate::build::{self, BuildError, Call, Drive, EngineObservation, EngineRun, Feed, RunOptions};
use crate::generate;
use crate::ghc::Oracle;
use crate::program::{BodyResult, Definition, Expression, Program, Reference, Time, Value, Window};

/// What the oracle says one observed node shows, transaction k at index
/// k - 1, each event or step with its time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expected {
    /// A stream's events.
    Stream {
        /// The events of each transaction and its children, in time order.
        events: Vec<Vec<(Time, i64)>>,
    },
    /// A cell.
    Cell {
        /// The value after transaction zero and its child transactions.
        initial: i64,
        /// The steps of each transaction and its children, in time order:
        /// none or one per instant.
        steps: Vec<Vec<(Time, i64)>>,
        /// The value after each transaction and its children.
        values: Vec<i64>,
    },
}

impl Expected {
    /// The time of a listener call's event or step, if the answer has one
    /// at that place.
    fn time(&self, call: &Call) -> Option<&Time> {
        let lists = match self {
            Expected::Stream { events } => events,
            Expected::Cell { steps, .. } => steps,
        };
        lists
            .get(call.transaction)
            .and_then(|list| list.get(call.index))
            .map(|(time, _)| time)
    }
}

/// A value of the subset: an integer, or a boolean as 0 or 1.
fn integer(datum: &Datum) -> Result<i64, String> {
    match datum {
        Datum::Integer(value) => Ok(*value),
        Datum::List(values) => Err(format!(
            "the oracle answered the list {values:?}, which the comparison does not observe"
        )),
    }
}

/// Events by external transaction: an event at `[k]` or at any child time
/// of it, `[k, …]`, belongs to transaction k. Each transaction's events are
/// sorted by time; one node has at most one event at a time.
fn by_transaction(
    events: &[(Time, Datum)],
    transactions: usize,
) -> Result<Vec<Vec<(Time, i64)>>, String> {
    let mut buckets = vec![Vec::new(); transactions];
    for (time, datum) in events {
        let bucket = match time.first() {
            Some(&k) if k >= 1 && (k as usize) <= transactions => (k - 1) as usize,
            _ => {
                return Err(format!(
                    "the oracle answered an event at {time:?}, which is in no transaction of \
                     the schedule"
                ));
            }
        };
        buckets[bucket].push((time.clone(), integer(datum)?));
    }
    for bucket in &mut buckets {
        bucket.sort_by(|a, b| a.0.cmp(&b.0));
        if let Some(pair) = bucket.windows(2).find(|pair| pair[0].0 == pair[1].0) {
            return Err(format!(
                "the oracle answered two events of one node at {:?}",
                pair[0].0
            ));
        }
    }
    Ok(buckets)
}

/// What each observed node should show, from the oracle's answer to the
/// program with the window `FromFirstTransaction`.
pub fn expected(program: &Program, answer: &Answer) -> Result<Vec<Expected>, String> {
    let observations = match answer {
        Answer::Observed(observations) => observations,
        Answer::Error(message) => return Err(format!("the oracle answered ERR {message}")),
        Answer::Timeout => return Err("the oracle answered TIMEOUT".to_owned()),
    };
    if observations.len() != program.observe.len() {
        return Err(format!(
            "the oracle answered {} observations for {} observed nodes",
            observations.len(),
            program.observe.len()
        ));
    }
    let transactions = program.schedule.len();
    observations
        .iter()
        .map(|observation| match observation {
            Observation::Stream { events } => Ok(Expected::Stream {
                events: by_transaction(events, transactions)?,
            }),
            Observation::Cell { initial, steps } => {
                let initial = integer(initial)?;
                let steps = by_transaction(steps, transactions)?;
                let mut value = initial;
                let values = steps
                    .iter()
                    .map(|steps| {
                        if let Some((_, last)) = steps.last() {
                            value = *last;
                        }
                        value
                    })
                    .collect();
                Ok(Expected::Cell {
                    initial,
                    steps,
                    values,
                })
            }
        })
        .collect()
}

/// One row of a node's table: a label, the engine's side, the oracle's
/// side, and whether they differ.
struct Row {
    label: String,
    engine: String,
    oracle: String,
    differs: bool,
}

fn list(values: &[i64]) -> String {
    format!("{values:?}")
}

/// The values of a list of timed events, what the comparison compares.
fn values(events: &[(Time, i64)]) -> Vec<i64> {
    events.iter().map(|(_, value)| *value).collect()
}

/// The oracle's side of a row: the values, and their times when any is a
/// child transaction's.
fn timed(events: &[(Time, i64)]) -> String {
    let mut text = list(&values(events));
    if events.iter().any(|(time, _)| time.len() > 1) {
        let times: Vec<String> = events
            .iter()
            .map(|(time, _)| format!("{time:?}").replace(' ', ""))
            .collect();
        let _ = write!(text, " at {}", times.join(" "));
    }
    text
}

/// The rows of one observed node: engine beside oracle, per transaction.
fn rows(engine: &EngineObservation, expected: &Expected) -> Vec<Row> {
    match (engine, expected) {
        (EngineObservation::Stream { events }, Expected::Stream { events: oracle }) => events
            .iter()
            .zip(oracle)
            .enumerate()
            .map(|(k, (engine, oracle))| Row {
                label: format!("[{}]", k + 1),
                engine: list(engine),
                oracle: timed(oracle),
                differs: *engine != values(oracle),
            })
            .collect(),
        (
            EngineObservation::Cell {
                registration,
                steps_registration,
                values: engine_values,
                steps,
                samples,
            },
            Expected::Cell {
                initial,
                steps: oracle_steps,
                values: oracle_values,
            },
        ) => {
            let mut rows = vec![Row {
                label: "initial".to_owned(),
                engine: format!(
                    "listen_cell {} listen_steps {}",
                    list(registration),
                    list(steps_registration)
                ),
                oracle: format!("value {initial}"),
                differs: registration.as_slice() != [*initial] || !steps_registration.is_empty(),
            }];
            for k in 0..samples.len() {
                let oracle = values(&oracle_steps[k]);
                rows.push(Row {
                    label: format!("[{}]", k + 1),
                    engine: format!(
                        "listen_cell {} listen_steps {} sample {}",
                        list(&engine_values[k]),
                        list(&steps[k]),
                        samples[k]
                    ),
                    oracle: format!(
                        "steps {} value {}",
                        timed(&oracle_steps[k]),
                        oracle_values[k]
                    ),
                    differs: engine_values[k] != oracle
                        || steps[k] != oracle
                        || samples[k] != oracle_values[k],
                });
            }
            rows
        }
        (engine, _) => vec![Row {
            label: "kind".to_owned(),
            engine: format!("{engine:?}"),
            oracle: "the other kind of node".to_owned(),
            differs: true,
        }],
    }
}

/// Where the engine's order of listener calls across nodes goes against
/// the oracle's times: a call that comes after a call for a later time.
/// Read only where every node's lists agree, so each call has a time.
fn order(program: &Program, expected: &[Expected], calls: &[Call]) -> Option<String> {
    let describe = |call: &Call, time: &Time| {
        let node = program.observe[call.observed];
        format!(
            "observed {} (node {node}, {})'s {} for {time:?}",
            call.observed,
            build::name(&program.definitions[node]),
            call.listened
        )
    };
    let mut latest: Option<(&Call, &Time)> = None;
    for call in calls {
        let Some(time) = expected.get(call.observed).and_then(|e| e.time(call)) else {
            continue;
        };
        match latest {
            Some((before, later)) if time < later => {
                return Some(format!(
                    "listener order: in transaction [{}] the engine called {} before {}, a \
                     call for an earlier time\n",
                    call.transaction + 1,
                    describe(before, later),
                    describe(call, time),
                ));
            }
            Some((_, later)) if time == later => {}
            _ => latest = Some((call, time)),
        }
    }
    None
}

/// The side-by-side table of every observed node, or `None` when the run
/// agrees with the oracle everywhere: every node's lists, every sample, and
/// the order of the listener calls across nodes.
pub fn compare(program: &Program, expected: &[Expected], run: &EngineRun) -> Option<String> {
    let types = build::check(program).ok();
    let mut tables = Vec::new();
    let mut disagree = false;
    for (position, (node, (engine, oracle))) in program
        .observe
        .iter()
        .zip(run.observations.iter().zip(expected))
        .enumerate()
    {
        let rows = rows(engine, oracle);
        disagree |= rows.iter().any(|row| row.differs);
        let what = match types.as_ref().map(|types| types[*node]) {
            Some(made) => made.to_string(),
            None => "a node".to_owned(),
        };
        let mut table = format!(
            "observed {position}: node {node} ({}), {what}\n",
            build::name(&program.definitions[*node])
        );
        let label = rows.iter().map(|row| row.label.len()).max().unwrap_or(0);
        let width = rows
            .iter()
            .map(|row| row.engine.len())
            .chain([6])
            .max()
            .unwrap_or(6);
        let _ = writeln!(table, "    {:label$}  {:width$}  oracle", "", "engine");
        for row in rows {
            let mark = if row.differs { "   <- differs" } else { "" };
            let _ = writeln!(
                table,
                "    {:label$}  {:width$}  {}{mark}",
                row.label, row.engine, row.oracle
            );
        }
        tables.push(table);
    }
    if run.observations.len() != expected.len() {
        disagree = true;
        tables.push(format!(
            "the engine observed {} nodes and the oracle {}\n",
            run.observations.len(),
            expected.len()
        ));
    }
    if !disagree {
        if let Some(message) = order(program, expected, &run.calls) {
            disagree = true;
            tables.push(message);
        }
    }
    disagree.then(|| tables.concat())
}

/// One way to run the engine: a mode's [`run`](build::run), named for
/// reports.
#[derive(Clone, Copy)]
pub struct Engine {
    /// The mode, as reports name it.
    pub name: &'static str,
    /// `run::<Local>` or `run::<Threaded>`.
    pub run: fn(&Program, RunOptions) -> Result<EngineRun, BuildError>,
}

impl fmt::Debug for Engine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name)
    }
}

/// What went wrong with one program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The builder refused the program.
    Build(BuildError),
    /// The oracle gave no answer the comparison can use: an error from the
    /// pool, `ERR`, `TIMEOUT`, or an event at a time outside the schedule.
    Oracle(String),
    /// A run panicked.
    Panic {
        /// The mode and options of the run.
        run: String,
        /// The panic's message.
        message: String,
    },
    /// A run disagreed with the oracle.
    Disagreement {
        /// The mode and options of the run.
        run: String,
        /// Every observed node, engine beside oracle.
        table: String,
    },
}

impl Failure {
    /// Whether two failures are of one kind, for a reduction that must keep
    /// the failure it started with: the same variant, and for a panic the
    /// same message but for its numbers, which name nodes.
    pub fn same_kind(&self, other: &Failure) -> bool {
        let digits = |text: &str| -> String {
            text.chars()
                .map(|c| if c.is_ascii_digit() { '#' } else { c })
                .collect()
        };
        match (self, other) {
            (Failure::Build(_), Failure::Build(_))
            | (Failure::Oracle(_), Failure::Oracle(_))
            | (Failure::Disagreement { .. }, Failure::Disagreement { .. }) => true,
            (Failure::Panic { message: a, .. }, Failure::Panic { message: b, .. }) => {
                digits(a) == digits(b)
            }
            _ => false,
        }
    }
}

/// A program and what went wrong with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// The program, with the window the comparison uses.
    pub program: Program,
    /// What went wrong.
    pub failure: Failure,
}

impl fmt::Display for Report {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.failure {
            Failure::Build(error) => {
                writeln!(formatter, "the builder refused the program: {error}")?
            }
            Failure::Oracle(message) => {
                writeln!(formatter, "the oracle gave no usable answer: {message}")?
            }
            Failure::Panic { run, message } => {
                writeln!(formatter, "the engine panicked ({run}): {message}")?;
            }
            Failure::Disagreement { run, .. } => {
                writeln!(formatter, "the engine disagrees with the oracle ({run})")?;
            }
        }
        write_program(formatter, &self.program)?;
        if let Failure::Disagreement { table, .. } = &self.failure {
            formatter.write_str(table)?;
        }
        Ok(())
    }
}

impl std::error::Error for Report {}

/// The program as Rust, as the Haskell line, and listed.
fn write_program(formatter: &mut fmt::Formatter<'_>, program: &Program) -> fmt::Result {
    writeln!(formatter, "program (Rust): {program:?}")?;
    writeln!(formatter, "program (Haskell): {program}")?;
    for (index, input) in program.inputs.iter().enumerate() {
        writeln!(formatter, "  input {index}: {input:?}")?;
    }
    for (index, definition) in program.definitions.iter().enumerate() {
        writeln!(formatter, "  {index:>2}  {definition:?}")?;
    }
    writeln!(formatter, "  observe {:?}", program.observe)?;
    writeln!(formatter, "schedule:")?;
    for (k, sends) in program.schedule.iter().enumerate() {
        let sends: Vec<String> = sends
            .iter()
            .map(|(input, value)| match value {
                Value::Integer(value) => format!("input {input} <- {value}"),
                Value::Boolean(value) => format!("input {input} <- {value}"),
                Value::List(values) => format!("input {input} <- {values:?}"),
            })
            .collect();
        let sends = if sends.is_empty() {
            "no sends".to_owned()
        } else {
            sends.join(", ")
        };
        writeln!(formatter, "  [{}] {sends}", k + 1)?;
    }
    Ok(())
}

/// Asks the oracle about the program, with the window
/// `FromFirstTransaction`, and holds every engine run with every option to
/// its answer, which it returns. The first failure is the report.
pub fn check_program(
    oracle: &Oracle,
    program: &Program,
    engines: &[Engine],
    runs: &[RunOptions],
) -> Result<Vec<Expected>, Box<Report>> {
    let mut program = program.clone();
    program.window = Window::FromFirstTransaction;
    let report = |program: &Program, failure| {
        Box::new(Report {
            program: program.clone(),
            failure,
        })
    };
    if let Err(error) = build::check(&program) {
        return Err(report(&program, Failure::Build(error)));
    }
    let answer = oracle
        .answer(&program)
        .map_err(|error| report(&program, Failure::Oracle(error.to_string())))?;
    let expected = expected(&program, &answer)
        .map_err(|message| report(&program, Failure::Oracle(message)))?;
    for engine in engines {
        for options in runs {
            let label = format!("{} mode, {options}", engine.name);
            let outcome =
                panic::catch_unwind(AssertUnwindSafe(|| (engine.run)(&program, *options)));
            let run = match outcome {
                Err(payload) => {
                    return Err(report(
                        &program,
                        Failure::Panic {
                            run: label,
                            message: build::panic_message(payload),
                        },
                    ));
                }
                Ok(Err(error)) => return Err(report(&program, Failure::Build(error))),
                Ok(Ok(run)) => run,
            };
            if let Some(table) = compare(&program, &expected, &run) {
                return Err(report(
                    &program,
                    Failure::Disagreement { run: label, table },
                ));
            }
        }
    }
    Ok(expected)
}

/// One way to run the engine with an input fed through a slot: a mode's
/// [`run_fed`](build::run_fed), named for reports.
#[derive(Clone, Copy)]
pub struct FedEngine {
    /// The mode, as reports name it.
    pub name: &'static str,
    /// `run_fed::<Local>` or `run_fed::<Threaded>`.
    pub run: fn(&Program, &Feed, RunOptions) -> Result<EngineRun, BuildError>,
}

impl fmt::Debug for FedEngine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name)
    }
}

/// A driver's steps as a report shows them: `w 3` for a write to the slot,
/// `pump`, and `tx [..]` for a transaction's sends.
fn script(feed: &Feed) -> String {
    let steps: Vec<String> = feed
        .script
        .iter()
        .map(|step| match step {
            Drive::Write(value) => format!("w {value}"),
            Drive::Pump => "pump".to_owned(),
            Drive::Transaction(sends) => format!("tx {sends:?}"),
        })
        .collect();
    format!("{}, pump", steps.join(", "))
}

/// The fold law against the oracle (RFD 7): asks the oracle about the
/// program with the feed's schedule, where each run of writes that a pump
/// ends is one transaction of the run's writes to the slot's input, which
/// the program declares coalescing with the slot's fold as an expression;
/// and holds every engine run, fed through the slot as the script says,
/// with every option, to that answer. The first failure is the report,
/// whose program carries that schedule and whose run names the script.
pub fn check_fed(
    oracle: &Oracle,
    program: &Program,
    feed: &Feed,
    engines: &[FedEngine],
    runs: &[RunOptions],
) -> Result<Vec<Expected>, Box<Report>> {
    let mut program = program.clone();
    program.window = Window::FromFirstTransaction;
    program.schedule = feed.schedule();
    let report = |program: &Program, failure| {
        Box::new(Report {
            program: program.clone(),
            failure,
        })
    };
    if let Err(error) = build::check(&program) {
        return Err(report(&program, Failure::Build(error)));
    }
    let answer = oracle
        .answer(&program)
        .map_err(|error| report(&program, Failure::Oracle(error.to_string())))?;
    let expected = expected(&program, &answer)
        .map_err(|message| report(&program, Failure::Oracle(message)))?;
    for engine in engines {
        for options in runs {
            let label = format!(
                "{} mode, {options}, input {} fed through a slot: {}",
                engine.name,
                feed.input,
                script(feed)
            );
            let outcome =
                panic::catch_unwind(AssertUnwindSafe(|| (engine.run)(&program, feed, *options)));
            let run = match outcome {
                Err(payload) => {
                    return Err(report(
                        &program,
                        Failure::Panic {
                            run: label,
                            message: build::panic_message(payload),
                        },
                    ));
                }
                Ok(Err(error)) => return Err(report(&program, Failure::Build(error))),
                Ok(Ok(run)) => run,
            };
            if let Some(table) = compare(&program, &expected, &run) {
                return Err(report(
                    &program,
                    Failure::Disagreement { run: label, table },
                ));
            }
        }
    }
    Ok(expected)
}

// ----- what the switches did -----

/// What some switches did, by the oracle's answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Switching {
    /// The switches the comparison sees: observed, or read by an observed
    /// node, however indirectly.
    pub switches: u64,
    /// Those whose outer selected another inner at some instant from `[1]`
    /// on, in a transaction or one of its children.
    pub switched: u64,
    /// The instants from `[1]` on at which one of them moved to another
    /// inner.
    pub moves: u64,
    /// The moves in child instants.
    pub moves_in_children: u64,
    /// The moves at whose instant the new inner fired or stepped: a
    /// `switch_stream` must not forward that event, and a `switch_cell`
    /// must step to that value, read after the instant.
    pub new_fired: u64,
    /// The moves at whose instant the old inner fired or stepped: a
    /// `switch_stream` forwards that event, and a `switch_cell` drops that
    /// step.
    pub old_fired: u64,
}

impl AddAssign for Switching {
    fn add_assign(&mut self, other: Switching) {
        self.switches += other.switches;
        self.switched += other.switched;
        self.moves += other.moves;
        self.moves_in_children += other.moves_in_children;
        self.new_fired += other.new_fired;
        self.old_fired += other.old_fired;
    }
}

/// What a program's switches did, `switch_stream`s and `switch_cell`s
/// apart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SwitchCount {
    /// The `switch_stream`s.
    pub streams: Switching,
    /// The `switch_cell`s, over `Cell`s and over `State`s.
    pub cells: Switching,
}

impl AddAssign for SwitchCount {
    fn add_assign(&mut self, other: SwitchCount) {
        self.streams += other.streams;
        self.cells += other.cells;
    }
}

/// How one switch's outer selects, in the watching program.
#[derive(Clone, Copy, Debug)]
enum Selection {
    /// A constant: the switch never moves.
    Constant,
    /// A hold of a pick: the observation at this position is the stream of
    /// the pick's indices; the switch starts at the first token, the
    /// hold's initial one, and index i selects token 1 + i.
    Hold(usize),
    /// A `MapPickCell`: the observation at this position is the cell of its
    /// indices; index i selects token i.
    Map(usize),
}

/// One switch the comparison sees, as the watching program observes it.
#[derive(Clone, Debug)]
struct Watched {
    /// A `switch_cell`, or a `switch_stream`.
    cell: bool,
    /// The tokens its outer may select, as nodes: for a hold, its initial
    /// one first and then the pick's list.
    tokens: Vec<usize>,
    /// Where each token is in the watching program's observations.
    observed: Vec<usize>,
    selection: Selection,
}

/// A program the oracle answers to show what another program's switches
/// did, and how to read that answer: see the module documentation.
#[derive(Clone, Debug)]
pub struct SwitchWatch {
    /// The program to ask the oracle about: every node of the original,
    /// with the window `Everything`, a node more per switch whose outer can
    /// select, and observing each switch's selections and every token its
    /// outer may select.
    pub program: Program,
    switches: Vec<Watched>,
}

/// Every node the observed nodes read, directly or through others, and
/// they themselves; a loop reads the definition its `Close` names.
fn seen(program: &Program) -> Vec<bool> {
    let n = program.definitions.len();
    let mut closes = vec![Vec::new(); n];
    for definition in &program.definitions {
        if let Definition::Close {
            forward,
            definition: Reference::TopLevel(node),
        } = definition
        {
            if *forward < n {
                closes[*forward].push(*node);
            }
        }
    }
    let mut seen = vec![false; n];
    let mut stack: Vec<usize> = program.observe.clone();
    while let Some(node) = stack.pop() {
        if node >= n || seen[node] {
            continue;
        }
        seen[node] = true;
        stack.extend(generate::references(&program.definitions[node]));
        stack.extend(closes[node].iter().copied());
    }
    seen
}

/// The program that shows what `program`'s switches did, or `None` if the
/// comparison sees no switch in it.
pub fn watch_switches(program: &Program) -> Option<SwitchWatch> {
    let seen = seen(program);
    let mut watching = Program {
        window: Window::Everything,
        observe: Vec::new(),
        ..program.clone()
    };
    let observe = |watching: &mut Program, node: usize| -> usize {
        match watching.observe.iter().position(|&o| o == node) {
            Some(position) => position,
            None => {
                watching.observe.push(node);
                watching.observe.len() - 1
            }
        }
    };
    let mut switches = Vec::new();
    for (node, definition) in program.definitions.iter().enumerate() {
        let (cell, outer) = match definition {
            Definition::SwitchCell(Reference::TopLevel(outer)) => (true, *outer),
            Definition::SwitchStream(Reference::TopLevel(outer)) => (false, *outer),
            _ => continue,
        };
        if !seen[node] {
            continue;
        }
        let tokens: Vec<usize> = build::switch_candidates(&program.definitions, outer)
            .iter()
            .filter_map(|token| match token {
                Reference::TopLevel(token) => Some(*token),
                Reference::Local(_) => None,
            })
            .collect();
        let choices = |listed: &[Reference]| listed.len() as i64;
        let selection = match &program.definitions[outer] {
            Definition::HoldStream {
                source: Reference::TopLevel(pick),
                ..
            }
            | Definition::HoldCell {
                source: Reference::TopLevel(pick),
                ..
            } => {
                let (index, listed, selector) = match &program.definitions[*pick] {
                    Definition::PickStream {
                        index,
                        streams,
                        source,
                    } => (index, streams, source),
                    Definition::PickCell {
                        index,
                        cells,
                        source,
                    } => (index, cells, source),
                    _ => continue,
                };
                watching.definitions.push(Definition::Map {
                    function: Expression::Modulo(Box::new(index.clone()), choices(listed)),
                    source: *selector,
                });
                let at = watching.definitions.len() - 1;
                Selection::Hold(observe(&mut watching, at))
            }
            Definition::MapPickCell { index, cells, cell } => {
                watching.definitions.push(Definition::MapCell {
                    function: Expression::Modulo(Box::new(index.clone()), choices(cells)),
                    cell: *cell,
                });
                let at = watching.definitions.len() - 1;
                Selection::Map(observe(&mut watching, at))
            }
            Definition::ConstantStream(_) | Definition::ConstantCell(_) => Selection::Constant,
            _ => continue,
        };
        let observed = tokens
            .iter()
            .map(|&token| observe(&mut watching, token))
            .collect();
        switches.push(Watched {
            cell,
            tokens,
            observed,
            selection,
        });
    }
    (!switches.is_empty()).then_some(SwitchWatch {
        program: watching,
        switches,
    })
}

impl SwitchWatch {
    /// What the switches did, by the oracle's answer to
    /// [`program`](SwitchWatch::program).
    pub fn count(&self, answer: &Answer) -> Result<SwitchCount, String> {
        let observations = match answer {
            Answer::Observed(observations) => observations,
            Answer::Error(message) => return Err(format!("the oracle answered ERR {message}")),
            Answer::Timeout => return Err("the oracle answered TIMEOUT".to_owned()),
        };
        if observations.len() != self.program.observe.len() {
            return Err(format!(
                "the oracle answered {} observations for {} observed nodes",
                observations.len(),
                self.program.observe.len()
            ));
        }
        let times = |position: usize| -> Vec<&Time> {
            match &observations[position] {
                Observation::Stream { events } => events.iter().map(|(time, _)| time).collect(),
                Observation::Cell { steps, .. } => steps.iter().map(|(time, _)| time).collect(),
            }
        };
        let index = |datum: &Datum| match datum {
            Datum::Integer(index) => Ok(*index as usize),
            Datum::List(_) => Err("the oracle answered a list for a pick's index".to_owned()),
        };
        let mut count = SwitchCount::default();
        for watched in &self.switches {
            // The token the switch follows before any selection, and each
            // selection after it: its time and its token's place.
            let (first, selections): (usize, Vec<(&Time, usize)>) = match watched.selection {
                Selection::Constant => (0, Vec::new()),
                Selection::Hold(position) => match &observations[position] {
                    Observation::Stream { events } => (
                        0,
                        events
                            .iter()
                            .map(|(time, datum)| Ok((time, 1 + index(datum)?)))
                            .collect::<Result<_, String>>()?,
                    ),
                    Observation::Cell { .. } => {
                        return Err("a pick's indices answered as a cell".to_owned());
                    }
                },
                Selection::Map(position) => match &observations[position] {
                    Observation::Cell { initial, steps } => (
                        index(initial)?,
                        steps
                            .iter()
                            .map(|(time, datum)| Ok((time, index(datum)?)))
                            .collect::<Result<_, String>>()?,
                    ),
                    Observation::Stream { .. } => {
                        return Err("a map_cell's indices answered as a stream".to_owned());
                    }
                },
            };
            let mut tally = Switching {
                switches: 1,
                ..Switching::default()
            };
            let fired = |place: usize, time: &Time| times(watched.observed[place]).contains(&time);
            let mut current = first;
            for (time, place) in selections {
                if place >= watched.tokens.len() {
                    return Err(format!("the oracle picked token {place} of a switch"));
                }
                let moved = watched.tokens[place] != watched.tokens[current];
                if moved && time.first().is_some_and(|&k| k >= 1) {
                    tally.moves += 1;
                    tally.moves_in_children += u64::from(time.len() > 1);
                    tally.new_fired += u64::from(fired(place, time));
                    tally.old_fired += u64::from(fired(current, time));
                }
                current = place;
            }
            tally.switched = u64::from(tally.moves > 0);
            if watched.cell {
                count.cells += tally;
            } else {
                count.streams += tally;
            }
        }
        Ok(count)
    }
}

// ----- what the constructs did -----

/// What some programs' constructs did, by the oracle's answer to their
/// watching programs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConstructCount {
    /// The constructs the comparison sees: top-level ones that an observed
    /// node reads, however indirectly.
    pub constructs: u64,
    /// Those that ran their body at an instant from `[1]` on.
    pub fired: u64,
    /// The switches the comparison sees over a hold of what one of those
    /// constructs emits, a token its body builds.
    pub switches: u64,
    /// Those that moved to a token a body built at an instant from `[1]`
    /// on: their construct fired then.
    pub switched: u64,
    /// Those that then showed it: a `switch_cell` steps to the built cell's
    /// value at the move, and a `switch_stream` forwarded an event after it.
    pub showed: u64,
    /// The constructs of values, among the fired ones, whose value reads a
    /// cell their body built, as it was before the instant.
    pub sampled: u64,
}

impl AddAssign for ConstructCount {
    fn add_assign(&mut self, other: ConstructCount) {
        self.constructs += other.constructs;
        self.fired += other.fired;
        self.switches += other.switches;
        self.switched += other.switched;
        self.showed += other.showed;
        self.sampled += other.sampled;
    }
}

/// A switch over a hold of a construct's tokens, as the watching program
/// observes it.
#[derive(Clone, Copy, Debug)]
struct Following {
    /// The construct's place in [`ConstructWatch::constructs`].
    construct: usize,
    /// A `switch_cell`, or a `switch_stream`.
    cell: bool,
    /// Where the switch is in the watching program's observations.
    observed: usize,
}

/// One construct the comparison sees, as the watching program observes
/// it.
#[derive(Clone, Copy, Debug)]
struct Constructed {
    /// Where its firings, one integer per event, are in the watching
    /// program's observations.
    observed: usize,
    /// Its body emits a node it built, not a top-level one.
    builds: bool,
    /// Its body emits a value that samples a node it built.
    samples: bool,
}

/// A program the oracle answers to show what another program's constructs
/// did, and how to read that answer.
#[derive(Clone, Debug)]
pub struct ConstructWatch {
    /// The program to ask the oracle about: every node of the original,
    /// with the window `Everything`, a node more per construct the
    /// comparison sees, which maps each of its events to 1, observed, and
    /// every switch over a hold of what it emits observed.
    pub program: Program,
    constructs: Vec<Constructed>,
    followers: Vec<Following>,
}

/// The program that shows what `program`'s constructs did, or `None` if
/// the comparison sees no construct in it.
pub fn watch_constructs(program: &Program) -> Option<ConstructWatch> {
    let seen = seen(program);
    let mut watching = Program {
        window: Window::Everything,
        observe: Vec::new(),
        ..program.clone()
    };
    let mut constructs = Vec::new();
    let mut places = vec![None; program.definitions.len()];
    for (node, definition) in program.definitions.iter().enumerate() {
        let Definition::Construct { body, .. } = definition else {
            continue;
        };
        if !seen[node] {
            continue;
        }
        watching.definitions.push(Definition::MapTo {
            value: Value::Integer(1),
            source: Reference::TopLevel(node),
        });
        watching.observe.push(watching.definitions.len() - 1);
        let mut sampled = Vec::new();
        if let BodyResult::Value(value) = &body.result {
            build::sampled(value, &mut sampled);
        }
        places[node] = Some(constructs.len());
        constructs.push(Constructed {
            observed: watching.observe.len() - 1,
            builds: matches!(body.result, BodyResult::Node(Reference::Local(_))),
            samples: sampled
                .iter()
                .any(|cell| matches!(cell, Reference::Local(_))),
        });
    }
    let mut followers = Vec::new();
    for (node, definition) in program.definitions.iter().enumerate() {
        let (cell, outer) = match definition {
            Definition::SwitchCell(Reference::TopLevel(outer)) => (true, *outer),
            Definition::SwitchStream(Reference::TopLevel(outer)) => (false, *outer),
            _ => continue,
        };
        let held = match program.definitions.get(outer) {
            Some(
                Definition::HoldCell {
                    source: Reference::TopLevel(held),
                    ..
                }
                | Definition::HoldStream {
                    source: Reference::TopLevel(held),
                    ..
                },
            ) => *held,
            _ => continue,
        };
        let Some(construct) = places.get(held).copied().flatten() else {
            continue;
        };
        if !seen[node] || !constructs[construct].builds {
            continue;
        }
        watching.observe.push(node);
        followers.push(Following {
            construct,
            cell,
            observed: watching.observe.len() - 1,
        });
    }
    (!constructs.is_empty()).then_some(ConstructWatch {
        program: watching,
        constructs,
        followers,
    })
}

impl ConstructWatch {
    /// What the constructs did, by the oracle's answer to
    /// [`program`](ConstructWatch::program).
    pub fn count(&self, answer: &Answer) -> Result<ConstructCount, String> {
        let observations = match answer {
            Answer::Observed(observations) => observations,
            Answer::Error(message) => return Err(format!("the oracle answered ERR {message}")),
            Answer::Timeout => return Err("the oracle answered TIMEOUT".to_owned()),
        };
        if observations.len() != self.program.observe.len() {
            return Err(format!(
                "the oracle answered {} observations for {} observed nodes",
                observations.len(),
                self.program.observe.len()
            ));
        }
        let times = |position: usize| -> Vec<&Time> {
            match &observations[position] {
                Observation::Stream { events } => events.iter().map(|(time, _)| time).collect(),
                Observation::Cell { steps, .. } => steps.iter().map(|(time, _)| time).collect(),
            }
        };
        let external = |time: &&Time| time.first().is_some_and(|&k| k >= 1);
        // Each construct's first event from [1] on.
        let first: Vec<Option<&Time>> = self
            .constructs
            .iter()
            .map(|constructed| times(constructed.observed).into_iter().find(external))
            .collect();
        let mut count = ConstructCount {
            constructs: self.constructs.len() as u64,
            ..ConstructCount::default()
        };
        for (constructed, first) in self.constructs.iter().zip(&first) {
            count.fired += u64::from(first.is_some());
            count.sampled += u64::from(first.is_some() && constructed.samples);
        }
        for following in &self.followers {
            count.switches += 1;
            let Some(moved) = first[following.construct] else {
                continue;
            };
            count.switched += 1;
            let after = times(following.observed).into_iter().any(|time| {
                if following.cell {
                    time >= moved
                } else {
                    time > moved
                }
            });
            count.showed += u64::from(after);
        }
        Ok(count)
    }
}
