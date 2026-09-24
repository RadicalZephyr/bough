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

use std::fmt::{self, Write as _};
use std::panic::{self, AssertUnwindSafe};

use crate::answer::{Answer, Datum, Observation};
use crate::build::{self, BuildError, Call, EngineObservation, EngineRun, RunOptions};
use crate::ghc::Oracle;
use crate::program::{Program, Time, Value, Window};

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

/// A panic's message.
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => match payload.downcast::<&'static str>() {
            Ok(message) => (*message).to_owned(),
            Err(_) => "a panic with no message".to_owned(),
        },
    }
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
                            message: panic_message(payload),
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
