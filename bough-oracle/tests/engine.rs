//! The engine against the oracle: programs built with the Bough API, run,
//! and compared with GHC's answers event by event and step by step.
//!
//! Every program runs in both modes, `Local` and `Threaded`: plainly, under
//! three shuffle seeds, and with the sends of each transaction permuted
//! under two seeds, a coalescing input's own sends keeping their order; and
//! once more collecting as every transaction opens, the collector's stress
//! setting, so that the memory model is held to the oracle too: a node the
//! program still needs that no root reaches would be collected, and its
//! next use would panic on a stale token.
//! Every run must match the oracle: per observed node and per external
//! transaction, the ordered list of events or steps, child transactions
//! included, whose child indices the engine does not show and the
//! comparison does not compare; the order of the listener calls across
//! nodes, against the oracle's times; and every sample after a transaction.
//!
//! The random programs come from `bough_oracle::programs()`, in four tests
//! that run in parallel. Most have loops, cell, state and stream loops,
//! child transactions, switches and constructs, and some loops run through
//! children or through a switch's selection; some constructs are nested,
//! run in child instants, build RFD 4's screens, or run RFD 2's navigation
//! loop; one in seven is a program of stages 1 and 2 alone. Each shard prints what its programs held, how many observed
//! nodes had events in child transactions, and what the switches the
//! comparison sees did, which a second question to the oracle about each
//! program shows: how many moved to another inner, and how often the new
//! inner or the old one fired at the move. `PROPTEST_CASES` sets how many
//! programs in all, 1024 unless set; `PROPTEST_RNG_SEED` fixes the seed,
//! which a failure prints. A failing program is shrunk by proptest, then by
//! `bough_oracle::reduce` to a program that fails the same way, and reported
//! with its schedule and every observed node, the engine beside the oracle.
//!
//! The fixed programs are the shapes RFD 1 asks for: counters, a loop
//! capped by its own value, two loops that read each other, the
//! sodium-rust#52 shape, a state loop, stream loops through a hold and
//! through children, splits sharing child indices with each other and with
//! a defer, and transaction zero's children; and the switches: the five
//! switch vectors of sodium.hs, the two switch_stream probes, a switch to a
//! quiet inner, two switches that reverse a dependency in one instant
//! (finding F46), switches at child instants, nested switches, and a switch
//! over States. Each asserts the oracle's answer, times included, as well
//! as the engine's agreement with it. One fixed program pins where the
//! engine and the text differ: a split built at a child instant, which the
//! text's `Split`, with no creation time, lets split its input's event from
//! the instant before.
//!
//! A test that needs GHC starts with `let Some(oracle) = oracle() else {
//! return };`: with `BOUGH_ORACLE=skip` it says that it skipped and returns.
//! The tests of the builder, the comparison and the generator alone, and
//! the tests that the engine refuses same-instant cycles through loops and
//! through switches, need no GHC.

use std::cell::{Cell as StdCell, RefCell};
use std::collections::hash_map::RandomState;
use std::env;
use std::hash::{BuildHasher, Hasher};
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::time::Instant;

use Definition::{
    Accumulate, AccumulateMut, CellLoop, Close, Constant, ConstantStream, Construct, Filter, Hold,
    HoldCell, HoldStream, InputCell, Lift, Map, MapCell, MapList, Merge, Once, PickCell,
    PickStream, Share, Snapshot, Steps, StepsWithCurrent, StreamLoop, SwitchCell, SwitchStream,
};
use Expression::{Argument, ArgumentAt, ConstructEvent, Literal, Sample, SecondArgument};
use Reference::TopLevel;
use bough::{Local, Threaded};
use bough_oracle::{
    Answer, Body, BodyResult, Definition, Engine, Expected, Expression, Held, Input, NodeType,
    Observation, Oracle, Program, Reference, Refusal, RunOptions, Scalar, SwitchCount, Switching,
    Type, Value, Window, check, check_program, compare, expected, guard_element, guard_filter,
    guard_map, programs, reduce, refusal, run, watch_switches, with_same_instant_cycle,
    with_switch_cycle,
};
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config, RngSeed, TestCaseError, TestError, TestRunner};

// ----- helpers -----

/// The oracle every test shares, or `None` when the tests skip.
fn oracle() -> Option<&'static Oracle> {
    bough_oracle::for_tests(PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("bough-oracle"))
}

/// Both modes, through the same builder.
const ENGINES: [Engine; 2] = [
    Engine {
        name: "Local",
        run: run::<Local>,
    },
    Engine {
        name: "Threaded",
        run: run::<Threaded>,
    },
];

/// The runs of every program: plain, three shuffle seeds, two permutations
/// of the sends, one of them under a shuffle too, and one that collects as
/// every transaction opens.
fn runs(seed: u64) -> Vec<RunOptions> {
    let mix = |k: u64| seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(k);
    let shuffled = |k: u64| RunOptions {
        shuffle_seed: Some(mix(k)),
        ..RunOptions::default()
    };
    vec![
        RunOptions::default(),
        shuffled(1),
        shuffled(2),
        shuffled(3),
        RunOptions {
            permute_sends: Some(mix(4)),
            ..RunOptions::default()
        },
        RunOptions {
            permute_sends: Some(mix(6)),
            ..shuffled(5)
        },
        RunOptions {
            collect_every_transaction: true,
            ..RunOptions::default()
        },
    ]
}

/// Holds the program to the oracle in every mode and run, and returns what
/// the oracle expects, for the test's own assertions.
fn agree(oracle: &Oracle, program: &Program) -> Vec<Expected> {
    check_program(oracle, program, &ENGINES, &runs(0)).unwrap_or_else(|report| panic!("{report}"))
}

/// A program over integer inputs, the window the comparison uses. Entry
/// k - 1 of the schedule is transaction k.
fn program(
    inputs: Vec<Input>,
    definitions: Vec<Definition>,
    observe: Vec<usize>,
    schedule: &[&[(usize, i64)]],
) -> Program {
    Program {
        window: Window::FromFirstTransaction,
        inputs,
        definitions,
        observe,
        schedule: schedule
            .iter()
            .map(|sends| {
                sends
                    .iter()
                    .map(|&(input, value)| (input, Value::Integer(value)))
                    .collect()
            })
            .collect(),
    }
}

fn integers(count: usize) -> Vec<Input> {
    vec![Input::new(Type::Integer); count]
}

/// A stream's expectation from its events in each transaction, each at the
/// transaction's own instant `[k]`.
fn stream(events: &[&[i64]]) -> Expected {
    Expected::Stream {
        events: events
            .iter()
            .enumerate()
            .map(|(k, events)| events.iter().map(|&v| (vec![k as i64 + 1], v)).collect())
            .collect(),
    }
}

/// A cell's expectation from its initial value and its step in each
/// transaction, at the transaction's own instant `[k]`.
fn cell(initial: i64, steps: &[Option<i64>]) -> Expected {
    let mut value = initial;
    Expected::Cell {
        initial,
        steps: steps
            .iter()
            .enumerate()
            .map(|(k, step)| step.iter().map(|&v| (vec![k as i64 + 1], v)).collect())
            .collect(),
        values: steps
            .iter()
            .map(|step| {
                value = step.unwrap_or(value);
                value
            })
            .collect(),
    }
}

/// A stream's expectation from its events over `transactions`
/// transactions, each with its time, `[k]` or a child time `[k, …]`.
fn timed_stream(transactions: usize, events: &[(&[i64], i64)]) -> Expected {
    let mut lists = vec![Vec::new(); transactions];
    for (time, value) in events {
        lists[time[0] as usize - 1].push((time.to_vec(), *value));
    }
    Expected::Stream { events: lists }
}

/// A cell's expectation from its value after transaction zero and its
/// steps over `transactions` transactions, each with its time.
fn timed_cell(initial: i64, transactions: usize, steps: &[(&[i64], i64)]) -> Expected {
    let mut lists: Vec<Vec<(Vec<i64>, i64)>> = vec![Vec::new(); transactions];
    for (time, value) in steps {
        lists[time[0] as usize - 1].push((time.to_vec(), *value));
    }
    let mut value = initial;
    let values = lists
        .iter()
        .map(|steps| {
            if let Some((_, last)) = steps.last() {
                value = *last;
            }
            value
        })
        .collect();
    Expected::Cell {
        initial,
        steps: lists,
        values,
    }
}

// ----- random programs -----

/// The programs in all when `PROPTEST_CASES` is not set: about ten seconds
/// on four cores in a debug build.
const DEFAULT_CASES: u32 = 1024;

/// The random tests split the programs between them and run in parallel.
const SHARDS: u32 = 4;

/// A seed nobody chose, for a run without `PROPTEST_RNG_SEED`.
fn fresh_seed() -> u64 {
    RandomState::new().build_hasher().finish()
}

/// What one shard's random programs held and did: how many had each kind
/// of loop, child transaction and switch; how many observed nodes had
/// events or steps, in child transactions among them, and how deep; how
/// many observed loop forwards stepped or fired, which a loop does only
/// when its feedback runs; and what the switches the comparison sees did,
/// by the oracle's answer to each program's watching program.
#[derive(Default)]
struct Tally {
    programs: u32,
    loops: u32,
    stream_loops: u32,
    state_loops: u32,
    splits: u32,
    defers: u32,
    children_in_loops: u32,
    switch_streams: u32,
    switch_cells: u32,
    switch_states: u32,
    nested_switches: u32,
    selectors_in_children: u32,
    constructs: u32,
    nested_constructs: u32,
    constructs_in_children: u32,
    linear_tokens: u32,
    navigation: u32,
    observed: u64,
    active: u64,
    in_children: u64,
    deepest: usize,
    /// Observed loop forwards, and those that stepped or fired.
    forwards: u64,
    active_forwards: u64,
    /// Programs with a switch the comparison sees, and those in which one
    /// moved to another inner.
    watched: u32,
    switched: u32,
    switching: SwitchCount,
    /// Watching programs the oracle gave no usable answer for, and the
    /// first reason.
    unwatched: u32,
    unwatched_reason: Option<String>,
}

impl Tally {
    fn add(
        &mut self,
        program: &Program,
        expected: &[Expected],
        switching: Option<Result<SwitchCount, String>>,
    ) {
        let c = census(program);
        self.programs += 1;
        self.loops += u32::from(c.loops);
        self.stream_loops += u32::from(c.stream_loops);
        self.state_loops += u32::from(c.state_loops);
        self.splits += u32::from(c.splits);
        self.defers += u32::from(c.defers);
        self.children_in_loops += u32::from(c.children_in_loops);
        self.switch_streams += u32::from(c.switch_streams);
        self.switch_cells += u32::from(c.switch_cells);
        self.switch_states += u32::from(c.switch_states);
        self.nested_switches += u32::from(c.nested_switches);
        self.selectors_in_children += u32::from(c.selectors_in_children);
        self.constructs += u32::from(c.constructs);
        self.nested_constructs += u32::from(c.nested_constructs);
        self.constructs_in_children += u32::from(c.constructs_in_children);
        self.linear_tokens += u32::from(c.linear_tokens);
        self.navigation += u32::from(c.navigation);
        match switching {
            None => {}
            Some(Ok(count)) => {
                self.watched += 1;
                self.switched += u32::from(count.streams.switched + count.cells.switched > 0);
                self.switching += count;
            }
            Some(Err(reason)) => {
                self.unwatched += 1;
                self.unwatched_reason.get_or_insert(reason);
            }
        }
        for (node, observed) in program.observe.iter().zip(expected) {
            let lists = match observed {
                Expected::Stream { events } => events,
                Expected::Cell { steps, .. } => steps,
            };
            let times: Vec<&Vec<i64>> = lists.iter().flatten().map(|(time, _)| time).collect();
            self.observed += 1;
            self.active += u64::from(!times.is_empty());
            if matches!(program.definitions[*node], CellLoop(_) | StreamLoop(_)) {
                self.forwards += 1;
                self.active_forwards += u64::from(!times.is_empty());
            }
            self.in_children += u64::from(times.iter().any(|time| time.len() > 1));
            self.deepest = self
                .deepest
                .max(times.iter().map(|time| time.len() - 1).max().unwrap_or(0));
        }
    }

    fn percent(&self, count: u32) -> f64 {
        100.0 * f64::from(count) / f64::from(self.programs.max(1))
    }
}

/// `part` of `whole` as a percentage.
fn share(part: u64, whole: u64) -> f64 {
    100.0 * part as f64 / whole.max(1) as f64
}

/// What one kind of switch did, for a tally's line.
fn switching(kind: &str, s: &Switching) -> String {
    format!(
        "{} {kind} seen, {:.0}% moved, {} moves ({:.0}% in child instants; the new inner fired \
         at {:.0}% of them, the old one at {:.0}%)",
        s.switches,
        share(s.switched, s.switches),
        s.moves,
        share(s.moves_in_children, s.moves),
        share(s.new_fired, s.moves),
        share(s.old_fired, s.moves),
    )
}

impl std::fmt::Display for Tally {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "with loops {:.0}% (stream loops {:.0}%, state loops {:.0}%), splits {:.0}%, \
             defers {:.0}%, a loop through children {:.0}%; of {} observed nodes {:.0}% had \
             events or steps and {:.0}% had them in child transactions, down to depth {}; of \
             {} observed loop forwards {:.0}% stepped or fired; switch_streams in {:.0}%, \
             switch_cells in {:.0}% (over States in {:.0}%), nested switches in {:.0}%, a \
             selector in child instants in {:.0}%; constructs in {:.0}% (nested in {:.0}%, in \
             child instants in {:.0}%, emitting linear streams in {:.0}%, the navigation loop in \
             {:.0}%); of {} programs whose switches the comparison sees, {:.0}% had one that \
             moved: {}; {}",
            self.percent(self.loops),
            self.percent(self.stream_loops),
            self.percent(self.state_loops),
            self.percent(self.splits),
            self.percent(self.defers),
            self.percent(self.children_in_loops),
            self.observed,
            share(self.active, self.observed),
            share(self.in_children, self.observed),
            self.deepest,
            self.forwards,
            share(self.active_forwards, self.forwards),
            self.percent(self.switch_streams),
            self.percent(self.switch_cells),
            self.percent(self.switch_states),
            self.percent(self.nested_switches),
            self.percent(self.selectors_in_children),
            self.percent(self.constructs),
            self.percent(self.nested_constructs),
            self.percent(self.constructs_in_children),
            self.percent(self.linear_tokens),
            self.percent(self.navigation),
            self.watched,
            share(u64::from(self.switched), u64::from(self.watched)),
            switching("switch_streams", &self.switching.streams),
            switching("switch_cells", &self.switching.cells),
        )?;
        if let Some(reason) = &self.unwatched_reason {
            write!(
                formatter,
                "; {} watching programs unanswered, the first: {reason}",
                self.unwatched
            )?;
        }
        Ok(())
    }
}

/// Runs this shard's share of the random programs.
fn random_programs(shard: u32) {
    let Some(oracle) = oracle() else { return };
    let total: u32 = env::var("PROPTEST_CASES")
        .ok()
        .and_then(|cases| cases.parse().ok())
        .unwrap_or(DEFAULT_CASES);
    let cases = total.div_ceil(SHARDS);
    let base = match Config::default().rng_seed {
        RngSeed::Fixed(seed) => seed,
        RngSeed::Random => fresh_seed(),
    };
    let config = Config {
        cases,
        failure_persistence: None,
        rng_seed: RngSeed::Fixed(base.wrapping_add(u64::from(shard))),
        ..Config::default()
    };
    let started = Instant::now();
    // How many programs ran, and when the first failure came.
    let tried = StdCell::new(0_u32);
    let first_failure = StdCell::new(None);
    let tally = RefCell::new(Tally::default());
    let mut runner = TestRunner::new(config);
    let result = runner.run(&(programs(), any::<u64>()), |(program, seed)| {
        tried.set(tried.get() + 1);
        match check_program(oracle, &program, &ENGINES, &runs(seed)) {
            Ok(expected) => {
                let switching = watch_switches(&program).map(|watch| {
                    oracle
                        .answer(&watch.program)
                        .map_err(|error| error.to_string())
                        .and_then(|answer| watch.count(&answer))
                });
                tally.borrow_mut().add(&program, &expected, switching);
                Ok(())
            }
            Err(report) => {
                if first_failure.get().is_none() {
                    first_failure.set(Some((tried.get(), started.elapsed())));
                }
                Err(TestCaseError::fail(report.to_string()))
            }
        }
    });
    match result {
        Ok(()) => eprintln!(
            "shard {shard}: {cases} random programs agree with the oracle, each in two modes \
             and seven runs, in {:.1?} (PROPTEST_RNG_SEED={base}); {}",
            started.elapsed(),
            tally.borrow()
        ),
        Err(TestError::Fail(_, (program, seed))) => {
            let (found, after) = first_failure.get().unwrap_or((0, started.elapsed()));
            let shrunk = started.elapsed();
            let runs = runs(seed);
            let failure = check_program(oracle, &program, &ENGINES, &runs)
                .expect_err("proptest's smallest program fails")
                .failure;
            // Only a failure of the same kind counts: a cut that makes the
            // engine refuse a loop, say, is not the disagreement it started
            // from.
            let reduced = reduce(&program, |p| {
                check_program(oracle, p, &ENGINES, &runs)
                    .is_err_and(|report| report.failure.same_kind(&failure))
            });
            let report = check_program(oracle, &reduced, &ENGINES, &runs)
                .expect_err("the reduced program still fails");
            panic!(
                "{report}\nproptest's smallest program, before reduction: {program:?}\n\
                 shard {shard}, PROPTEST_RNG_SEED={base}: program {found} failed first, after \
                 {after:.1?}; proptest had shrunk it after {shrunk:.1?}, and the reduction \
                 ended after {:.1?}",
                started.elapsed()
            );
        }
        Err(TestError::Abort(reason)) => panic!("{reason}"),
    }
}

#[test]
fn random_programs_agree_with_the_oracle_shard_0() {
    random_programs(0);
}

#[test]
fn random_programs_agree_with_the_oracle_shard_1() {
    random_programs(1);
}

#[test]
fn random_programs_agree_with_the_oracle_shard_2() {
    random_programs(2);
}

#[test]
fn random_programs_agree_with_the_oracle_shard_3() {
    random_programs(3);
}

/// The message of the panic `f` raises, or `None` if it returns.
fn panic_message<R>(f: impl FnOnce() -> R) -> Option<String> {
    let payload = panic::catch_unwind(AssertUnwindSafe(f)).err()?;
    Some(if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else {
        String::new()
    })
}

/// A loop whose definition depends on its own forward in the same instant
/// is refused at its close, in any program the generator makes: one of its
/// cell loops of integers is closed instead with a lift of the definition
/// and the forward, of the definition and a `map_cell` of the forward, or
/// of the definition and a hold of the forward's steps view, F3's shape,
/// which RFD 2's path rule accepts because the path passes through a hold.
/// The build panics with the engine's refusal in both modes. The oracle is
/// not asked: the semantics do not define such a loop, and the fixed-point
/// iteration may settle anyway (F3).
#[test]
fn same_instant_cycles_are_refused_at_close() {
    let base = match Config::default().rng_seed {
        RngSeed::Fixed(seed) => seed,
        RngSeed::Random => fresh_seed(),
    };
    let mut runner = TestRunner::new(Config {
        cases: 256,
        failure_persistence: None,
        rng_seed: RngSeed::Fixed(base),
        ..Config::default()
    });
    let refused = StdCell::new(0);
    let result = runner.run(&(programs(), any::<usize>()), |(program, which)| {
        let Some(cyclic) = with_same_instant_cycle(&program, which) else {
            return Ok(());
        };
        // A Sample in the build closure cannot read a loop closed through
        // itself, and check refuses that before the engine sees it.
        if check(&cyclic).is_err() {
            return Ok(());
        }
        for engine in ENGINES {
            let message = panic_message(|| (engine.run)(&cyclic, RunOptions::default()));
            prop_assert!(
                message
                    .as_deref()
                    .is_some_and(|m| m.contains("closing this loop makes a same-instant cycle")),
                "{} mode built a same-instant cycle without refusing it at its close: \
                 {message:?}\n{cyclic:?}",
                engine.name
            );
        }
        refused.set(refused.get() + 1);
        Ok(())
    });
    if let Err(error) = result {
        panic!("{error}\nPROPTEST_RNG_SEED={base}");
    }
    // About three programs in five have a cell loop of integers that is not
    // a State and that no Sample reads.
    assert!(
        refused.get() > 100,
        "only {} of 256 programs had a loop to rewire",
        refused.get()
    );
}

/// The engine's refusals of a switch that would close a same-instant
/// cycle, by where they come: the path check of a switch's first link or
/// of its move at commit; a read of a switch's value that goes round the
/// cycle before the switch's first link (F49) or while relink moves it;
/// and a read after the instant that meets the switch again (R10).
const SWITCH_REFUSALS: [&str; 3] = [
    "switching closes a same-instant cycle",
    "a same-instant cycle through a switch_cell read before its first link",
    "a same-instant cycle through a read after the instant",
];

/// A same-instant cycle through a switch's choices is refused, or poisons
/// the graph, in any program the generator makes with a switch: one of its
/// switches is made to select a loop's forward that the loop closes with a
/// node that reads the switch (`with_switch_cycle`). At its first link the
/// build must panic, at the link's path check or where a read of the
/// switch's value goes round the cycle first. At a move, in transaction 1,
/// the transaction must panic, at the move's path check, at relink's read
/// of the new selection, or where a read after the instant meets the
/// switch again, and leave the graph poisoned. Both modes, plainly and
/// under a shuffle. The oracle is not asked: the semantics do not define
/// such a program.
#[test]
fn same_instant_cycles_through_a_switch_are_refused_or_poison_the_graph() {
    let base = match Config::default().rng_seed {
        RngSeed::Fixed(seed) => seed,
        RngSeed::Random => fresh_seed(),
    };
    let mut runner = TestRunner::new(Config {
        cases: 256,
        failure_persistence: None,
        rng_seed: RngSeed::Fixed(base),
        ..Config::default()
    });
    // The programs refused at a first link and at a move, and of each, how
    // many had each refusal in some run.
    let counts = RefCell::new([[0_u32; 4]; 2]);
    let result = runner.run(&(programs(), any::<usize>()), |(program, which)| {
        let Some(cyclic) = with_switch_cycle(&program, which) else {
            return Ok(());
        };
        // A Sample in the build closure cannot read a switch that may
        // select the loop's forward, and check refuses the program.
        if check(&cyclic).is_err() {
            return Ok(());
        }
        let moves = cyclic.inputs.len() > program.inputs.len();
        let mut seen = [false; 3];
        let shuffled = RunOptions {
            shuffle_seed: Some(base ^ which as u64),
            ..RunOptions::default()
        };
        for options in [RunOptions::default(), shuffled] {
            for (name, outcome) in [
                ("Local", refusal::<Local>(&cyclic, options)),
                ("Threaded", refusal::<Threaded>(&cyclic, options)),
            ] {
                let outcome = outcome.map_err(|error| TestCaseError::fail(error.to_string()))?;
                let kind = |message: &str| {
                    SWITCH_REFUSALS
                        .iter()
                        .position(|refusal| message.contains(refusal))
                };
                let refused = match &outcome {
                    Refusal::Build(message) if !moves => kind(message).filter(|&k| k < 2),
                    Refusal::Transaction {
                        transaction: 0,
                        message,
                        poisoned: true,
                    } if moves => kind(message),
                    _ => None,
                };
                prop_assert!(
                    refused.is_some(),
                    "{name} mode, {options}: a switch that {} a cycle ended {outcome:?}\n{cyclic:?}",
                    if moves {
                        "moves in transaction 1 into"
                    } else {
                        "links"
                    }
                );
                if let Some(k) = refused {
                    seen[k] = true;
                }
            }
        }
        let mut counts = counts.borrow_mut();
        let row = &mut counts[usize::from(moves)];
        row[0] += 1;
        for (count, seen) in row[1..].iter_mut().zip(seen) {
            *count += u32::from(seen);
        }
        Ok(())
    });
    if let Err(error) = result {
        panic!("{error}\nPROPTEST_RNG_SEED={base}");
    }
    let [linked, moved] = *counts.borrow();
    eprintln!(
        "of 256 programs, {} were refused at a switch's first link, {} by the path check and {} \
         by a read before the link in some run; and {} at a move, {} by the path check, {} by \
         relink's read of the new selection and {} by a read after the instant in some run; \
         each in both modes, plainly and under a shuffle",
        linked[0], linked[1], linked[2], moved[0], moved[1], moved[2], moved[3]
    );
    // Of 256 programs, about 90 have a switch to rewire at its first link
    // and about 53 at a move, each give or take 7. The floors sit far below
    // both, so that no seed trips them.
    assert!(
        linked[0] > 40 && moved[0] > 20,
        "only {} and {} of 256 programs had a switch to rewire",
        linked[0],
        moved[0]
    );
}

// ----- fixed programs -----

/// RFD 2's flagship: a click counter, a label mapped from it, and a
/// listener that fires now and on every step. One send, then a transaction
/// with one send.
#[test]
fn rfd2_flagship_accumulate_map_cell_and_listen_cell() {
    let Some(oracle) = oracle() else { return };
    let flagship = program(
        integers(1),
        vec![
            Definition::Input(0),
            Accumulate {
                initial: Literal(0),
                function: SecondArgument + Literal(1),
                source: TopLevel(0),
            },
            MapCell {
                function: Argument * Literal(10),
                cell: TopLevel(1),
            },
        ],
        vec![2, 1],
        &[&[(0, 1)], &[(0, 1)]],
    );
    assert_eq!(
        agree(oracle, &flagship),
        [cell(0, &[Some(10), Some(20)]), cell(0, &[Some(1), Some(2)])]
    );
}

/// F4: marking reaches a hold behind a filter that rejects, and a map_cell
/// of it, and neither steps: no listener call, no steps event, the same
/// sample.
#[test]
fn f4_a_hold_and_a_map_cell_behind_a_rejecting_filter_do_not_step() {
    let Some(oracle) = oracle() else { return };
    let f4 = program(
        integers(1),
        vec![
            Definition::Input(0),
            Filter {
                predicate: Literal(5).less_than(Argument),
                source: TopLevel(0),
            },
            Hold {
                initial: Literal(0),
                source: TopLevel(1),
            },
            MapCell {
                function: Argument * Literal(2),
                cell: TopLevel(2),
            },
            Steps(TopLevel(3)),
        ],
        vec![2, 3, 4],
        &[&[(0, 3)], &[(0, 9)], &[(0, 4)]],
    );
    assert_eq!(
        agree(oracle, &f4),
        [
            cell(0, &[None, Some(9), None]),
            cell(0, &[None, Some(18), None]),
            stream(&[&[], &[18], &[]]),
        ]
    );
}

/// A coalescing input sent three times in one transaction folds left, the
/// first send on the left, before the hold of `input_cell` or any
/// listener sees it; the permuted runs interleave the stream's input with
/// the cell's and keep each one's own order.
#[test]
fn a_coalescing_input_sent_three_times_in_one_transaction_folds_left() {
    let Some(oracle) = oracle() else { return };
    let coalescing = program(
        vec![Input::coalescing(Type::Integer, Argument - SecondArgument)],
        vec![
            Definition::Input(0),
            InputCell {
                input: 0,
                initial: Literal(0),
            },
        ],
        vec![0, 1],
        &[
            &[(0, 1), (0, 2), (0, 3)],
            &[(0, 10)],
            &[],
            &[(0, 7), (0, 1)],
        ],
    );
    assert_eq!(
        agree(oracle, &coalescing),
        [
            stream(&[&[(1 - 2) - 3], &[10], &[], &[6]]),
            cell(0, &[Some(-4), Some(10), None, Some(6)]),
        ]
    );
}

/// A merge of two inputs sent in one transaction calls its function once
/// with the left event first, whichever input was sent first.
#[test]
fn a_merge_of_simultaneous_inputs_is_f_of_left_and_right() {
    let Some(oracle) = oracle() else { return };
    let merge = program(
        integers(2),
        vec![
            Definition::Input(0),
            Definition::Input(1),
            Merge {
                function: Argument - SecondArgument,
                left: TopLevel(0),
                right: TopLevel(1),
            },
        ],
        vec![2],
        &[&[(0, 20), (1, 3)], &[(1, 3), (0, 20)], &[(0, 5)], &[(1, 4)]],
    );
    assert_eq!(agree(oracle, &merge), [stream(&[&[17], &[17], &[5], &[4]])]);
}

/// `steps_with_current` built in the build closure fires in transaction
/// zero, so a hold over it starts at the cell's value, and a scan over it
/// has taken one event; then both follow the steps.
#[test]
fn steps_with_current_created_at_build_feeds_a_hold() {
    let Some(oracle) = oracle() else { return };
    let current = program(
        integers(1),
        vec![
            InputCell {
                input: 0,
                initial: Literal(7),
            },
            MapCell {
                function: Argument + Literal(100),
                cell: TopLevel(0),
            },
            StepsWithCurrent(TopLevel(1)),
            Share(TopLevel(2)),
            Hold {
                initial: Literal(0),
                source: TopLevel(3),
            },
            Definition::Scan {
                initial: Literal(0),
                output: SecondArgument * Literal(1000) + Argument,
                state: SecondArgument + Literal(1),
                source: TopLevel(3),
            },
        ],
        vec![4, 5, 3],
        &[&[(0, 5)], &[], &[(0, 6)]],
    );
    assert_eq!(
        agree(oracle, &current),
        [
            cell(107, &[Some(105), None, Some(106)]),
            stream(&[&[1105], &[], &[2106]]),
            stream(&[&[105], &[], &[106]]),
        ]
    );
}

/// A lift of six cells steps once per transaction however many of its
/// inputs step in it.
#[test]
fn a_lift_of_six_steps_once_when_several_inputs_step_together() {
    let Some(oracle) = oracle() else { return };
    let mut definitions: Vec<Definition> = (0..6)
        .map(|input| InputCell {
            input,
            initial: Literal(input as i64 + 1),
        })
        .collect();
    // Cell i is digit i of a number in base 100, cell 0 first.
    let digits = (1..6).fold(ArgumentAt(0), |number, index| {
        number * Literal(100) + ArgumentAt(index)
    });
    definitions.push(Lift {
        function: digits,
        cells: (0..6).map(TopLevel).collect(),
    });
    definitions.push(Steps(TopLevel(6)));
    let lift = program(
        integers(6),
        definitions,
        vec![6, 7],
        &[
            &[(0, 10), (2, 30), (4, 50)],
            &[(5, 60), (4, 50), (3, 40), (2, 30), (1, 20), (0, 10)],
            &[],
            &[(1, 21)],
        ],
    );
    let number = |digits: [i64; 6]| digits.iter().fold(0, |number, digit| number * 100 + digit);
    let before = number([1, 2, 3, 4, 5, 6]);
    let first = number([10, 2, 30, 4, 50, 6]);
    let second = number([10, 20, 30, 40, 50, 60]);
    let fourth = number([10, 21, 30, 40, 50, 60]);
    assert_eq!(
        agree(oracle, &lift),
        [
            cell(before, &[Some(first), Some(second), None, Some(fourth)]),
            stream(&[&[first], &[second], &[], &[fourth]]),
        ]
    );
}

/// `accumulate` and `accumulate_mut` over one shared stream with one
/// function give the same cell, and a snapshot and a lift read the State
/// like a cell.
#[test]
fn accumulate_and_accumulate_mut_agree() {
    let Some(oracle) = oracle() else { return };
    let step = (SecondArgument * Literal(3) + Argument).modulo(1000);
    let accumulators = program(
        integers(2),
        vec![
            Definition::Input(0),
            Share(TopLevel(0)),
            Accumulate {
                initial: Literal(1),
                function: step.clone(),
                source: TopLevel(1),
            },
            AccumulateMut {
                initial: Literal(1),
                function: step,
                source: TopLevel(1),
            },
            Definition::Input(1),
            Snapshot {
                function: Argument * Literal(10000) + SecondArgument,
                source: TopLevel(4),
                cell: TopLevel(3),
            },
            Lift {
                function: ArgumentAt(0) - ArgumentAt(1),
                cells: vec![TopLevel(2), TopLevel(3)],
            },
        ],
        vec![2, 3, 5, 6],
        &[&[(0, 4)], &[(0, 5), (1, 1)], &[(1, 2)], &[(0, 400), (1, 3)]],
    );
    let answers = agree(oracle, &accumulators);
    assert_eq!(answers[0], answers[1]);
    assert_eq!(answers[0], cell(1, &[Some(7), Some(26), None, Some(478)]));
    // The snapshot reads the state before the instant.
    assert_eq!(answers[2], stream(&[&[], &[10007], &[20026], &[30026]]));
    assert_eq!(answers[3], cell(0, &[Some(0), Some(0), None, Some(0)]));
}

// ----- fixed programs: loops -----

/// A counter: a cell loop closed with a hold of a snapshot of its own
/// forward, which reads the count from before each instant. After the
/// close anything may use the forward: a steps view of it carries the
/// definition's steps, a map_cell of it reads through, and another
/// snapshot of it reads the count before the instant.
#[test]
fn a_counter_reads_its_own_forward_through_a_snapshot() {
    let Some(oracle) = oracle() else { return };
    let counter = program(
        integers(1),
        vec![
            CellLoop(Type::Integer),
            Definition::Input(0),
            Share(TopLevel(1)),
            Snapshot {
                function: SecondArgument + Literal(1),
                source: TopLevel(2),
                cell: TopLevel(0),
            },
            Hold {
                initial: Literal(0),
                source: TopLevel(3),
            },
            Close {
                forward: 0,
                definition: TopLevel(4),
            },
            Steps(TopLevel(0)),
            MapCell {
                function: Argument * Literal(10),
                cell: TopLevel(0),
            },
            Snapshot {
                function: SecondArgument,
                source: TopLevel(2),
                cell: TopLevel(0),
            },
            Hold {
                initial: Literal(99),
                source: TopLevel(8),
            },
        ],
        vec![0, 4, 6, 7, 9],
        &[&[(0, 1)], &[], &[(0, 1)], &[(0, 1)]],
    );
    assert_eq!(
        agree(oracle, &counter),
        [
            cell(0, &[Some(1), None, Some(2), Some(3)]),
            cell(0, &[Some(1), None, Some(2), Some(3)]),
            stream(&[&[1], &[], &[2], &[3]]),
            cell(0, &[Some(10), None, Some(20), Some(30)]),
            cell(99, &[Some(0), None, Some(1), Some(2)]),
        ]
    );
}

/// Fact 1's capped counter, which the lazy text cannot run: a filter inside
/// the loop reads the loop's own value, so the count steps to 3 and stops.
#[test]
fn a_counter_capped_by_its_own_value_stops() {
    let Some(oracle) = oracle() else { return };
    let capped = program(
        integers(1),
        vec![
            CellLoop(Type::Integer),
            Definition::Input(0),
            Snapshot {
                function: SecondArgument + Literal(1),
                source: TopLevel(1),
                cell: TopLevel(0),
            },
            Filter {
                predicate: Argument.less_than(Literal(4)),
                source: TopLevel(2),
            },
            Hold {
                initial: Literal(0),
                source: TopLevel(3),
            },
            Close {
                forward: 0,
                definition: TopLevel(4),
            },
        ],
        vec![0],
        &[
            &[(0, 1)],
            &[(0, 1)],
            &[(0, 1)],
            &[(0, 1)],
            &[(0, 1)],
            &[(0, 1)],
        ],
    );
    assert_eq!(
        agree(oracle, &capped),
        [cell(0, &[Some(1), Some(2), Some(3), None, None, None])]
    );
}

/// A diamond through loops: two loops read each other through snapshots of
/// one shared stream, so both step in one instant, each reading the other
/// from before it. Downstream a lift joins the two forwards, a merge joins
/// their steps, and a lift joins the definitions with a hold of the shared
/// stream, upstream of both. A reads B, B accumulates what it reads of A,
/// and at [1] A steps to the value it had, which is still a step.
#[test]
fn two_loops_that_read_each_other_lifted_and_merged_downstream() {
    let Some(oracle) = oracle() else { return };
    let diamond = program(
        integers(1),
        vec![
            CellLoop(Type::Integer),
            CellLoop(Type::Integer),
            Definition::Input(0),
            Share(TopLevel(2)),
            Snapshot {
                function: Argument + SecondArgument,
                source: TopLevel(3),
                cell: TopLevel(1),
            },
            Hold {
                initial: Literal(1),
                source: TopLevel(4),
            },
            Snapshot {
                function: SecondArgument * Literal(2) - Argument,
                source: TopLevel(3),
                cell: TopLevel(0),
            },
            Accumulate {
                initial: Literal(0),
                function: SecondArgument + Argument,
                source: TopLevel(6),
            },
            Close {
                forward: 0,
                definition: TopLevel(5),
            },
            Close {
                forward: 1,
                definition: TopLevel(7),
            },
            Lift {
                function: ArgumentAt(0) * Literal(100) + ArgumentAt(1),
                cells: vec![TopLevel(0), TopLevel(1)],
            },
            Steps(TopLevel(0)),
            Steps(TopLevel(1)),
            Merge {
                function: Argument * Literal(1000) + SecondArgument,
                left: TopLevel(11),
                right: TopLevel(12),
            },
            Hold {
                initial: Literal(5),
                source: TopLevel(3),
            },
            Lift {
                function: ArgumentAt(0) + ArgumentAt(1) + ArgumentAt(2),
                cells: vec![TopLevel(5), TopLevel(7), TopLevel(14)],
            },
        ],
        vec![0, 1, 10, 13, 15],
        &[&[(0, 1)], &[(0, 2)], &[], &[(0, 3)]],
    );
    assert_eq!(
        agree(oracle, &diamond),
        [
            cell(1, &[Some(1), Some(3), None, Some(4)]),
            cell(0, &[Some(1), Some(1), None, Some(4)]),
            cell(100, &[Some(101), Some(301), None, Some(404)]),
            stream(&[&[1001], &[3001], &[], &[4004]]),
            cell(6, &[Some(3), Some(6), None, Some(11)]),
        ]
    );
}

/// The sodium-rust#52 shape, as RFD 1 asks: health, a loop, clamps its own
/// value plus the merge of heal and damage by the maximum from before the
/// instant, itself a loop over the level-ups, upstream of health; and a
/// lift reads health with the maximum. The lift is observation only, so
/// health is 100 at the record-0003 instant, where sodium-rust 2.1.3 gave
/// 60 with a lift; and 200, not 250, where the maximum steps with a heal.
#[test]
fn a_loop_cell_lifted_with_a_cell_upstream_of_itself() {
    let Some(oracle) = oracle() else { return };
    let shape = program(
        integers(3),
        vec![
            CellLoop(Type::Integer),
            Definition::Input(2),
            Snapshot {
                function: SecondArgument + Argument,
                source: TopLevel(1),
                cell: TopLevel(0),
            },
            Hold {
                initial: Literal(100),
                source: TopLevel(2),
            },
            Close {
                forward: 0,
                definition: TopLevel(3),
            },
            CellLoop(Type::Integer),
            Definition::Input(0),
            Definition::Input(1),
            Map {
                function: Literal(0) - Argument,
                source: TopLevel(7),
            },
            Merge {
                function: Argument + SecondArgument,
                left: TopLevel(6),
                right: TopLevel(8),
            },
            Snapshot {
                function: Argument + SecondArgument,
                source: TopLevel(9),
                cell: TopLevel(5),
            },
            Snapshot {
                function: Argument.minimum(SecondArgument).maximum(Literal(0)),
                source: TopLevel(10),
                cell: TopLevel(0),
            },
            Hold {
                initial: Literal(60),
                source: TopLevel(11),
            },
            Close {
                forward: 5,
                definition: TopLevel(12),
            },
            Lift {
                function: ArgumentAt(0) * Literal(1000) + ArgumentAt(1),
                cells: vec![TopLevel(5), TopLevel(0)],
            },
        ],
        vec![3, 12, 14, 5],
        &[
            &[(0, 100), (1, 50), (2, 100)],
            &[(1, 30)],
            &[(0, 500), (2, 50)],
        ],
    );
    assert_eq!(
        agree(oracle, &shape),
        [
            cell(100, &[Some(200), None, Some(250)]),
            cell(60, &[Some(100), Some(70), Some(200)]),
            cell(60100, &[Some(100200), Some(70200), Some(200250)]),
            cell(60, &[Some(100), Some(70), Some(200)]),
        ]
    );
}

/// A stream loop read through a hold of its forward and a snapshot of that:
/// a running total, which needs no child transaction.
#[test]
fn a_stream_loop_read_through_a_hold_and_a_snapshot() {
    let Some(oracle) = oracle() else { return };
    let total = program(
        integers(1),
        vec![
            StreamLoop(Type::Integer),
            Hold {
                initial: Literal(0),
                source: TopLevel(0),
            },
            Definition::Input(0),
            Snapshot {
                function: Argument + SecondArgument,
                source: TopLevel(2),
                cell: TopLevel(1),
            },
            Share(TopLevel(3)),
            Close {
                forward: 0,
                definition: TopLevel(4),
            },
        ],
        vec![1, 4],
        &[&[(0, 2)], &[(0, 3)], &[], &[(0, 10)]],
    );
    assert_eq!(
        agree(oracle, &total),
        [
            cell(0, &[Some(2), Some(5), None, Some(15)]),
            stream(&[&[2], &[5], &[], &[15]]),
        ]
    );
}

/// A loop closed with an in-place accumulator is a state loop: the builder
/// looks ahead at the Close and declares it with `state_loop`, and the
/// forward is a State, which a map_cell reads through and a snapshot reads
/// from before the instant. The accumulator admits an event while the state
/// before the instant is below 10, and a step of 0 at [4] is still a step.
#[test]
fn an_in_place_accumulator_closes_a_state_loop() {
    let Some(oracle) = oracle() else { return };
    let members = program(
        integers(1),
        vec![
            CellLoop(Type::Integer),
            Definition::Input(0),
            Share(TopLevel(1)),
            Snapshot {
                function: Expression::if_then_else(
                    SecondArgument.less_than(Literal(10)),
                    Argument,
                    Literal(0),
                ),
                source: TopLevel(2),
                cell: TopLevel(0),
            },
            AccumulateMut {
                initial: Literal(0),
                function: SecondArgument + Argument,
                source: TopLevel(3),
            },
            Close {
                forward: 0,
                definition: TopLevel(4),
            },
            MapCell {
                function: Argument * Literal(2),
                cell: TopLevel(0),
            },
            Snapshot {
                function: SecondArgument,
                source: TopLevel(2),
                cell: TopLevel(0),
            },
            Hold {
                initial: Literal(-1),
                source: TopLevel(7),
            },
        ],
        vec![0, 4, 6, 8],
        &[&[(0, 4)], &[(0, 5)], &[(0, 6)], &[(0, 7)]],
    );
    let types = check(&members).unwrap();
    assert_eq!(
        types[0],
        NodeType::Cell {
            value: Scalar::Integer,
            state: true
        },
        "a loop closed with a State is a State"
    );
    assert_eq!(
        agree(oracle, &members),
        [
            cell(0, &[Some(4), Some(9), Some(15), Some(15)]),
            cell(0, &[Some(4), Some(9), Some(15), Some(15)]),
            cell(0, &[Some(8), Some(18), Some(30), Some(30)]),
            cell(-1, &[Some(0), Some(4), Some(9), Some(15)]),
        ]
    );
}

// ----- fixed programs: child transactions -----

/// A countdown: a stream loop through a defer, merged with an input, whose
/// guard, `(x mod 7) - 1` kept while positive, ends it. Each event comes
/// back one child level down, `[1]`, `[1, 0]`, `[1, 0, 0]`, and an
/// accumulator over it steps at each.
#[test]
fn a_stream_loop_through_a_defer_counts_down_one_child_level_at_a_time() {
    let Some(oracle) = oracle() else { return };
    let countdown = program(
        integers(1),
        vec![
            StreamLoop(Type::Integer),
            Map {
                function: guard_map(7, 1),
                source: TopLevel(0),
            },
            Filter {
                predicate: guard_filter(),
                source: TopLevel(1),
            },
            Definition::Defer(TopLevel(2)),
            Definition::Input(0),
            Definition::OrElse {
                left: TopLevel(4),
                right: TopLevel(3),
            },
            Share(TopLevel(5)),
            Close {
                forward: 0,
                definition: TopLevel(6),
            },
            Accumulate {
                initial: Literal(0),
                function: SecondArgument + Argument,
                source: TopLevel(6),
            },
        ],
        vec![6, 8],
        &[&[(0, 3)], &[(0, 9)], &[(0, -1)]],
    );
    assert!(bough_oracle::well_founded(&countdown));
    assert_eq!(
        agree(oracle, &countdown),
        [
            timed_stream(
                3,
                &[
                    (&[1], 3),
                    (&[1, 0], 2),
                    (&[1, 0, 0], 1),
                    (&[2], 9),
                    (&[2, 0], 1),
                    (&[3], -1),
                    (&[3, 0], 5),
                    (&[3, 0, 0], 4),
                    (&[3, 0, 0, 0], 3),
                    (&[3, 0, 0, 0, 0], 2),
                    (&[3, 0, 0, 0, 0, 0], 1),
                ]
            ),
            timed_cell(
                0,
                3,
                &[
                    (&[1], 3),
                    (&[1, 0], 5),
                    (&[1, 0, 0], 6),
                    (&[2], 15),
                    (&[2, 0], 16),
                    (&[3], 15),
                    (&[3, 0], 20),
                    (&[3, 0, 0], 24),
                    (&[3, 0, 0, 0], 27),
                    (&[3, 0, 0, 0, 0], 29),
                    (&[3, 0, 0, 0, 0, 0], 30),
                ]
            ),
        ]
    );
}

/// A stream loop through a split: each event x makes two smaller ones in
/// its children, `(x mod 6) - 1 - i`, kept while positive, so the split
/// fires inside its own children. The children run depth first, which is
/// time order: `[1, 0, 0]` and its own child come before `[1, 1]`, where
/// breadth first would give 4, 3, 2, 2, 1, 1, 1 (the text's Split, before
/// the oracle's patch F7, orders them by parent, not by time).
#[test]
fn a_stream_loop_through_a_split_runs_its_children_depth_first() {
    let Some(oracle) = oracle() else { return };
    let tree = program(
        integers(1),
        vec![
            StreamLoop(Type::Integer),
            MapList {
                length: Literal(2),
                element: guard_element(6, 1),
                source: TopLevel(0),
            },
            Definition::Split(TopLevel(1)),
            Filter {
                predicate: guard_filter(),
                source: TopLevel(2),
            },
            Definition::Input(0),
            Merge {
                function: Argument * Literal(100) + SecondArgument,
                left: TopLevel(4),
                right: TopLevel(3),
            },
            Share(TopLevel(5)),
            Close {
                forward: 0,
                definition: TopLevel(6),
            },
            Hold {
                initial: Literal(0),
                source: TopLevel(6),
            },
        ],
        vec![6, 8],
        &[&[(0, 4)], &[(0, 2)]],
    );
    assert!(bough_oracle::well_founded(&tree));
    let events: &[(&[i64], i64)] = &[
        (&[1], 4),
        (&[1, 0], 3),
        (&[1, 0, 0], 2),
        (&[1, 0, 0, 0], 1),
        (&[1, 0, 1], 1),
        (&[1, 1], 2),
        (&[1, 1, 0], 1),
        (&[2], 2),
        (&[2, 0], 1),
    ];
    assert_eq!(
        agree(oracle, &tree),
        [timed_stream(2, events), timed_cell(0, 2, events)]
    );
}

/// Two splits that fire in one instant share child indices: element n of
/// each is in child n, and a merge combines them there. The longer split's
/// last element is alone in its child.
#[test]
fn two_splits_in_one_instant_share_child_indices() {
    let Some(oracle) = oracle() else { return };
    let splits = program(
        integers(1),
        vec![
            Definition::Input(0),
            Share(TopLevel(0)),
            MapList {
                length: Literal(3),
                element: Argument * Literal(10) + SecondArgument,
                source: TopLevel(1),
            },
            Definition::Split(TopLevel(2)),
            MapList {
                length: Literal(2),
                element: SecondArgument - Argument,
                source: TopLevel(1),
            },
            Definition::Split(TopLevel(4)),
            Merge {
                function: Argument * Literal(1000) + SecondArgument,
                left: TopLevel(3),
                right: TopLevel(5),
            },
        ],
        vec![6],
        &[&[(0, 1)], &[(0, 2)]],
    );
    assert_eq!(
        agree(oracle, &splits),
        [timed_stream(
            2,
            &[
                (&[1, 0], 9999),
                (&[1, 1], 11000),
                (&[1, 2], 12),
                (&[2, 0], 19998),
                (&[2, 1], 20999),
                (&[2, 2], 22),
            ]
        )]
    );
}

/// A defer is a split of one element, so it shares child index 0 with
/// every split that fires in its instant (finding F13), and a merge
/// combines the deferred event with the split's first element.
#[test]
fn a_split_and_a_defer_share_child_index_0() {
    let Some(oracle) = oracle() else { return };
    let shared = program(
        integers(1),
        vec![
            Definition::Input(0),
            Share(TopLevel(0)),
            MapList {
                length: Literal(2),
                element: Argument + SecondArgument,
                source: TopLevel(1),
            },
            Definition::Split(TopLevel(2)),
            Map {
                function: Argument * Literal(100),
                source: TopLevel(1),
            },
            Definition::Defer(TopLevel(4)),
            Merge {
                function: Argument - SecondArgument,
                left: TopLevel(3),
                right: TopLevel(5),
            },
        ],
        vec![6],
        &[&[(0, 1)], &[(0, 3)]],
    );
    assert_eq!(
        agree(oracle, &shared),
        [timed_stream(
            2,
            &[(&[1, 0], -99), (&[1, 1], 2), (&[2, 0], -297), (&[2, 1], 4)]
        )]
    );
}

/// Lists of length 0 to 3, split: an empty list emits nothing and runs no
/// child, and a hold over the elements steps once in each child, so a cell
/// listener hears several steps in one external transaction and a sample
/// after it reads the last.
#[test]
fn splits_of_lists_of_zero_to_three_elements() {
    let Some(oracle) = oracle() else { return };
    let lengths = program(
        integers(1),
        vec![
            Definition::Input(0),
            MapList {
                length: Argument,
                element: SecondArgument * Literal(10) + Argument,
                source: TopLevel(0),
            },
            Definition::Split(TopLevel(1)),
            Share(TopLevel(2)),
            Hold {
                initial: Literal(-1),
                source: TopLevel(3),
            },
        ],
        vec![3, 4],
        &[&[(0, 4)], &[(0, 1)], &[(0, 2)], &[(0, 3)], &[(0, 0)]],
    );
    let elements: &[(&[i64], i64)] = &[
        (&[2, 0], 1),
        (&[3, 0], 2),
        (&[3, 1], 12),
        (&[4, 0], 3),
        (&[4, 1], 13),
        (&[4, 2], 23),
    ];
    assert_eq!(
        agree(oracle, &lengths),
        [timed_stream(5, elements), timed_cell(-1, 5, elements)]
    );
}

/// A cell loop through a defer of its own steps view: legal where the same
/// loop without the defer is F3's same-instant cycle, and ended by its
/// guard. The forward steps one child level down each time round.
#[test]
fn a_cell_loop_through_a_defer_of_its_steps_view() {
    let Some(oracle) = oracle() else { return };
    let looped = program(
        integers(1),
        vec![
            CellLoop(Type::Integer),
            Steps(TopLevel(0)),
            Map {
                function: guard_map(7, 2),
                source: TopLevel(1),
            },
            Filter {
                predicate: guard_filter(),
                source: TopLevel(2),
            },
            Definition::Defer(TopLevel(3)),
            Definition::Input(0),
            Definition::OrElse {
                left: TopLevel(5),
                right: TopLevel(4),
            },
            Hold {
                initial: Literal(0),
                source: TopLevel(6),
            },
            Close {
                forward: 0,
                definition: TopLevel(7),
            },
        ],
        vec![0, 7],
        &[&[(0, 6)], &[(0, 3)]],
    );
    assert!(bough_oracle::well_founded(&looped));
    let steps: &[(&[i64], i64)] = &[
        (&[1], 6),
        (&[1, 0], 4),
        (&[1, 0, 0], 2),
        (&[2], 3),
        (&[2, 0], 1),
    ];
    assert_eq!(
        agree(oracle, &looped),
        [timed_cell(0, 2, steps), timed_cell(0, 2, steps)]
    );
}

/// Transaction zero has children (finding F11): a defer of a
/// steps_with_current built in the build fires at `[0, 0]`, before build
/// returns, so a hold over it starts the graph at the input cell's value.
/// After that it follows the cell one child level down.
#[test]
fn a_defer_in_transaction_zero_runs_before_build_returns() {
    let Some(oracle) = oracle() else { return };
    let zero = program(
        integers(1),
        vec![
            InputCell {
                input: 0,
                initial: Literal(5),
            },
            StepsWithCurrent(TopLevel(0)),
            Definition::Defer(TopLevel(1)),
            Share(TopLevel(2)),
            Hold {
                initial: Literal(0),
                source: TopLevel(3),
            },
        ],
        vec![4, 3],
        &[&[(0, 7)], &[]],
    );
    assert_eq!(
        agree(oracle, &zero),
        [
            timed_cell(5, 2, &[(&[1, 0], 7)]),
            timed_stream(2, &[(&[1, 0], 7)]),
        ]
    );
}

/// F3's loop, `c = hold 0 (merge ticks (map (+1) (steps c)))`, which RFD
/// 2's path rule accepts because the path passes through a hold, is a
/// same-instant cycle: the builder builds it as a user would, and the
/// engine refuses it at its close, in both modes, naming the cycle.
#[test]
fn a_loop_through_its_holds_steps_view_is_refused_at_close() {
    let f3 = program(
        integers(1),
        vec![
            CellLoop(Type::Integer),
            Definition::Input(0),
            Steps(TopLevel(0)),
            Map {
                function: Argument + Literal(1),
                source: TopLevel(2),
            },
            Merge {
                function: Argument + SecondArgument,
                left: TopLevel(1),
                right: TopLevel(3),
            },
            Hold {
                initial: Literal(0),
                source: TopLevel(4),
            },
            Close {
                forward: 0,
                definition: TopLevel(5),
            },
        ],
        vec![5],
        &[&[(0, 1)]],
    );
    assert!(
        check(&f3).is_ok(),
        "the builder leaves the refusal to the engine"
    );
    assert!(!bough_oracle::well_founded(&f3));
    for engine in ENGINES {
        let message = panic_message(|| (engine.run)(&f3, RunOptions::default()))
            .unwrap_or_else(|| panic!("{} mode built F3's loop", engine.name));
        assert!(
            message.contains("closing this loop makes a same-instant cycle"),
            "{message}"
        );
    }
}

// ----- fixed programs: switches -----

/// A letter's code: the vectors of sodium.hs carry letters.
fn code(letter: char) -> i64 {
    letter as i64
}

/// `SwitchS` of sodium.hs with its events moved from `[k]` to `[k + 1]`: the
/// outer selects s2 at [2], where s2's X is not forwarded and s1's b is:
/// a, b, Y, Z.
#[test]
fn the_switch_stream_vector_restated_with_inputs() {
    let Some(oracle) = oracle() else { return };
    let vector = program(
        integers(3),
        vec![
            Definition::Input(0),
            Share(TopLevel(0)),
            Definition::Input(1),
            Share(TopLevel(2)),
            Definition::Input(2),
            PickStream {
                index: Argument,
                streams: vec![TopLevel(1), TopLevel(3)],
                source: TopLevel(4),
            },
            HoldStream {
                initial: TopLevel(1),
                source: TopLevel(5),
            },
            SwitchStream(TopLevel(6)),
        ],
        vec![7],
        &[
            &[(0, code('a')), (1, code('W'))],
            &[(0, code('b')), (1, code('X')), (2, 1)],
            &[(0, code('c')), (1, code('Y'))],
            &[(0, code('d')), (1, code('Z'))],
        ],
    );
    let [a, b, y, z] = ['a', 'b', 'Y', 'Z'].map(code);
    assert_eq!(agree(oracle, &vector), [stream(&[&[a], &[b], &[y], &[z]])]);
}

/// One of sodium.hs's `SwitchC` vectors with its events moved from `[k]` to
/// `[k + 1]`: c2's letter before its sends, c2's and c3's sends and the
/// selections at [1] to [4]. c1 is 'a' and is sent b, c, d and e; c3 is
/// '1'; the selection is an index into c1, c2 and c3.
struct SwitchCVector {
    c2: char,
    c2_sends: [Option<char>; 4],
    c3_sends: [Option<char>; 4],
    selects: [Option<i64>; 4],
}

impl SwitchCVector {
    fn program(&self) -> Program {
        let c1_sends = ['b', 'c', 'd', 'e'];
        let schedule: Vec<Vec<(usize, i64)>> = (0..4)
            .map(|k| {
                let mut sends = vec![(0, code(c1_sends[k]))];
                sends.extend(self.c2_sends[k].map(|c| (1, code(c))));
                sends.extend(self.c3_sends[k].map(|c| (2, code(c))));
                sends.extend(self.selects[k].map(|s| (3, s)));
                sends
            })
            .collect();
        let schedule: Vec<&[(usize, i64)]> = schedule.iter().map(Vec::as_slice).collect();
        program(
            integers(4),
            vec![
                InputCell {
                    input: 0,
                    initial: Literal(code('a')),
                },
                InputCell {
                    input: 1,
                    initial: Literal(code(self.c2)),
                },
                InputCell {
                    input: 2,
                    initial: Literal(code('1')),
                },
                Definition::Input(3),
                PickCell {
                    index: Argument,
                    cells: vec![TopLevel(0), TopLevel(1), TopLevel(2)],
                    source: TopLevel(3),
                },
                HoldCell {
                    initial: TopLevel(0),
                    source: TopLevel(4),
                },
                SwitchCell(TopLevel(5)),
                Steps(TopLevel(6)),
            ],
            vec![6, 7],
            &schedule,
        )
    }
}

/// `SwitchC 1` to `SwitchC 4` of sodium.hs with their events moved from
/// `[k]` to `[k + 1]`. The outer switches to c2 at [2], where c2 steps to
/// X: after a step at [1] in 1, for the first time in 2. In 3, c2 is X from
/// the start and quiet at [2]. In 4 the outer switches on to c3 at [4],
/// where c3 steps. After the build the switch holds a, then b, X, Y, and Z,
/// or 5 in 4, and its steps view carries the same.
#[test]
fn the_switch_cell_vectors_restated_with_inputs() {
    let Some(oracle) = oracle() else { return };
    let all = [Some('W'), Some('X'), Some('Y'), Some('Z')];
    let none = [None; 4];
    let at_2 = [None, Some(1), None, None];
    let vectors = [
        SwitchCVector {
            c2: 'V',
            c2_sends: all,
            c3_sends: none,
            selects: at_2,
        },
        SwitchCVector {
            c2: 'W',
            c2_sends: [None, Some('X'), Some('Y'), Some('Z')],
            c3_sends: none,
            selects: at_2,
        },
        SwitchCVector {
            c2: 'X',
            c2_sends: [None, None, Some('Y'), Some('Z')],
            c3_sends: none,
            selects: at_2,
        },
        SwitchCVector {
            c2: 'V',
            c2_sends: all,
            c3_sends: [Some('2'), Some('3'), Some('4'), Some('5')],
            selects: [None, Some(1), None, Some(2)],
        },
    ];
    for (vector, last) in vectors.iter().zip(['Z', 'Z', 'Z', '5']) {
        let [b, x, y, last] = ['b', 'X', 'Y', last].map(code);
        assert_eq!(
            agree(oracle, &vector.program()),
            [
                cell(code('a'), &[Some(b), Some(x), Some(y), Some(last)]),
                stream(&[&[b], &[x], &[y], &[last]]),
            ]
        );
    }
}

/// The selector probe (SwitchS.hs): the outer selects z at [2], where the
/// old stream is quiet, so only the watcher reaches the switch; from [3] on
/// z's events come through: a, Y, Z. An outer that was only reach would
/// never move the switch.
#[test]
fn a_switch_stream_moves_on_a_selection_while_its_old_stream_is_quiet() {
    let Some(oracle) = oracle() else { return };
    let probe = program(
        integers(3),
        vec![
            Definition::Input(0),
            Share(TopLevel(0)),
            Definition::Input(1),
            Share(TopLevel(2)),
            Definition::Input(2),
            PickStream {
                index: Literal(1),
                streams: vec![TopLevel(1), TopLevel(3)],
                source: TopLevel(4),
            },
            HoldStream {
                initial: TopLevel(1),
                source: TopLevel(5),
            },
            SwitchStream(TopLevel(6)),
        ],
        vec![7],
        &[
            &[(0, code('a')), (1, code('X'))],
            &[(2, 0)],
            &[(0, code('b')), (1, code('Y'))],
            &[(0, code('c')), (1, code('Z'))],
        ],
    );
    let [a, y, z] = ['a', 'Y', 'Z'].map(code);
    assert_eq!(agree(oracle, &probe), [stream(&[&[a], &[], &[y], &[z]])]);
}

/// The loop through the selection (SwitchS.hs): every event v of the
/// switch selects, through a hold, the stream it follows from the next
/// instant on, ticks + v, and the switch's events close the stream loop
/// that selects. With the outer a dependency the loop would be a cycle,
/// refused at close; the outer is a watcher, and the loop gives 1, 2, 3, 4.
#[test]
fn a_switch_stream_loop_through_its_selection() {
    let Some(oracle) = oracle() else { return };
    let mut definitions = vec![
        StreamLoop(Type::Integer),
        Share(TopLevel(0)),
        Definition::Input(0),
        Share(TopLevel(2)),
    ];
    let mut streams = Vec::new();
    for k in 0..5 {
        definitions.push(Map {
            function: Argument + Literal(k),
            source: TopLevel(3),
        });
        definitions.push(Share(TopLevel(definitions.len() - 1)));
        streams.push(TopLevel(definitions.len() - 1));
    }
    let pick = definitions.len();
    definitions.push(PickStream {
        index: Argument,
        streams: streams.clone(),
        source: TopLevel(1),
    });
    definitions.push(HoldStream {
        initial: streams[0],
        source: TopLevel(pick),
    });
    definitions.push(SwitchStream(TopLevel(pick + 1)));
    definitions.push(Close {
        forward: 0,
        definition: TopLevel(pick + 2),
    });
    let tick: &[(usize, i64)] = &[(0, 1)];
    let probe = program(integers(1), definitions, vec![1], &[tick; 4]);
    assert!(bough_oracle::well_founded(&probe));
    assert_eq!(agree(oracle, &probe), [stream(&[&[1], &[2], &[3], &[4]])]);
}

/// A switch_cell that switches to a quiet inner steps with that inner's
/// value; the old inner's step at the switch instant is dropped; a switch
/// back to an inner that steps at that instant carries the step: 5, 2, 30,
/// 8. A map_cell and a steps view of the switch step with it.
#[test]
fn a_switch_cell_that_switches_to_a_quiet_inner_steps() {
    let Some(oracle) = oracle() else { return };
    let quiet = program(
        integers(3),
        vec![
            InputCell {
                input: 0,
                initial: Literal(1),
            },
            InputCell {
                input: 1,
                initial: Literal(2),
            },
            Definition::Input(2),
            PickCell {
                index: Argument,
                cells: vec![TopLevel(0), TopLevel(1)],
                source: TopLevel(2),
            },
            HoldCell {
                initial: TopLevel(0),
                source: TopLevel(3),
            },
            SwitchCell(TopLevel(4)),
            MapCell {
                function: Argument * Literal(10),
                cell: TopLevel(5),
            },
            Steps(TopLevel(5)),
        ],
        vec![5, 6, 7],
        &[
            &[(0, 5)],
            &[(0, 6), (2, 1)],
            &[(0, 7), (1, 30)],
            &[(0, 8), (2, 0)],
        ],
    );
    assert_eq!(
        agree(oracle, &quiet),
        [
            cell(1, &[Some(5), Some(2), Some(30), Some(8)]),
            cell(10, &[Some(50), Some(20), Some(300), Some(80)]),
            stream(&[&[5], &[2], &[30], &[8]]),
        ]
    );
}

/// Two switches reverse a dependency between them in one instant (finding
/// F46). Before [1], B follows p = A + 10, so B depends on A, which is a
/// cell loop's definition read through its forward; at [1], A moves to
/// y = B + 100 while B moves to a constant, so A depends on B. The graph
/// the two moves make together has no cycle, and GHC gives A 1 then 102
/// and B 11 then 2. A relink that checked each move as it made it would
/// find the cycle y, B, p, the loop, A through B's old inner. The program
/// is not what [`well_founded`](bough_oracle::well_founded) calls well
/// founded, since each switch may select a cell that depends on the other,
/// so the generator never makes one like it.
#[test]
fn two_switches_may_reverse_a_dependency_between_them_in_one_instant() {
    let Some(oracle) = oracle() else { return };
    let reversal = program(
        integers(1),
        vec![
            Definition::Input(0),
            Share(TopLevel(0)),
            Constant(Literal(1)),
            Constant(Literal(2)),
            CellLoop(Type::Integer),
            MapCell {
                function: Argument + Literal(10),
                cell: TopLevel(4),
            },
            PickCell {
                index: Literal(0),
                cells: vec![TopLevel(3)],
                source: TopLevel(1),
            },
            HoldCell {
                initial: TopLevel(5),
                source: TopLevel(6),
            },
            SwitchCell(TopLevel(7)),
            MapCell {
                function: Argument + Literal(100),
                cell: TopLevel(8),
            },
            PickCell {
                index: Literal(0),
                cells: vec![TopLevel(9)],
                source: TopLevel(1),
            },
            HoldCell {
                initial: TopLevel(2),
                source: TopLevel(10),
            },
            SwitchCell(TopLevel(11)),
            Close {
                forward: 4,
                definition: TopLevel(12),
            },
        ],
        vec![12, 8, 5, 9],
        &[&[(0, 0)], &[]],
    );
    assert!(!bough_oracle::well_founded(&reversal));
    assert_eq!(
        agree(oracle, &reversal),
        [
            cell(1, &[Some(102), None]),
            cell(11, &[Some(2), None]),
            cell(11, &[Some(112), None]),
            cell(111, &[Some(102), None]),
        ]
    );
}

/// Switches move at child instants. A switch_cell's selector is split into
/// two elements, so each child instant is a switch instant whose commit
/// moves the switch before the next child: at [1, 0] to c2, which stepped
/// at [1], and at [1, 1] to a constant; at [2] c1 steps while deselected,
/// [2, 0] selects it and [2, 1] c2. A switch_stream's selector is
/// deferred, so it moves at [k, 0], and z deferred twice fires at
/// [k, 0, 0], after the move of its transaction: at [1] the switch forwards
/// a, and z's 10 at [1, 0, 0]; at [2] nothing of a; at [3] it moves back
/// at [3, 0], and at [4] forwards a again but not z.
#[test]
fn switches_move_at_child_instants() {
    let Some(oracle) = oracle() else { return };
    let split = program(
        integers(3),
        vec![
            InputCell {
                input: 0,
                initial: Literal(1),
            },
            InputCell {
                input: 1,
                initial: Literal(2),
            },
            Constant(Literal(3)),
            Definition::Input(2),
            MapList {
                length: Literal(2),
                element: Argument + SecondArgument,
                source: TopLevel(3),
            },
            Definition::Split(TopLevel(4)),
            PickCell {
                index: Argument,
                cells: vec![TopLevel(0), TopLevel(1), TopLevel(2)],
                source: TopLevel(5),
            },
            HoldCell {
                initial: TopLevel(0),
                source: TopLevel(6),
            },
            SwitchCell(TopLevel(7)),
            Steps(TopLevel(8)),
        ],
        vec![8, 9],
        &[&[(1, 20), (2, 1)], &[(0, 5), (2, 0)], &[(1, 21)]],
    );
    let steps: &[(&[i64], i64)] = &[
        (&[1, 0], 20),
        (&[1, 1], 3),
        (&[2, 0], 5),
        (&[2, 1], 20),
        (&[3], 21),
    ];
    assert_eq!(
        agree(oracle, &split),
        [timed_cell(1, 3, steps), timed_stream(3, steps)]
    );
    let deferred = program(
        integers(3),
        vec![
            Definition::Input(0),
            Share(TopLevel(0)),
            Definition::Input(1),
            Definition::Defer(TopLevel(2)),
            Definition::Defer(TopLevel(3)),
            Share(TopLevel(4)),
            Definition::Input(2),
            Definition::Defer(TopLevel(6)),
            PickStream {
                index: Argument,
                streams: vec![TopLevel(1), TopLevel(5)],
                source: TopLevel(7),
            },
            HoldStream {
                initial: TopLevel(1),
                source: TopLevel(8),
            },
            SwitchStream(TopLevel(9)),
        ],
        vec![10],
        &[
            &[(0, 1), (1, 10), (2, 1)],
            &[(0, 2), (1, 20)],
            &[(0, 3), (2, 0)],
            &[(0, 4), (1, 40)],
        ],
    );
    assert_eq!(
        agree(oracle, &deferred),
        [timed_stream(
            4,
            &[(&[1], 1), (&[1, 0, 0], 10), (&[2, 0, 0], 20), (&[4], 4)]
        )]
    );
}

/// A switch_cell among two switch_cells, each between two leaves. Leaves,
/// middles and the top move in one instant and apart, over six
/// transactions of up to seven sends: at [3] the top moves to the second
/// middle, which moves to a quiet leaf in the same instant; at [6]
/// everything moves and every leaf steps. The top: 11, 22, 40, 23, 15, 36.
#[test]
fn a_switch_cell_among_switch_cells() {
    let Some(oracle) = oracle() else { return };
    let mut definitions: Vec<Definition> = (0..4)
        .map(|leaf| InputCell {
            input: leaf,
            initial: Literal(10 * (leaf as i64 + 1)),
        })
        .collect();
    definitions.extend((4..7).map(Definition::Input));
    let switch = |definitions: &mut Vec<Definition>, cells: [usize; 2], selector: usize| {
        let pick = definitions.len();
        definitions.push(PickCell {
            index: Argument,
            cells: cells.into_iter().map(TopLevel).collect(),
            source: TopLevel(selector),
        });
        definitions.push(HoldCell {
            initial: TopLevel(cells[0]),
            source: TopLevel(pick),
        });
        definitions.push(SwitchCell(TopLevel(pick + 1)));
        pick + 2
    };
    let first = switch(&mut definitions, [0, 1], 4);
    let second = switch(&mut definitions, [2, 3], 5);
    let top = switch(&mut definitions, [first, second], 6);
    definitions.push(Steps(TopLevel(top)));
    let nested = program(
        integers(7),
        definitions,
        vec![top, top + 1, first, second],
        &[
            &[(0, 11)],
            &[(1, 22), (4, 1)],
            &[(0, 13), (1, 23), (5, 1), (6, 1)],
            &[(2, 34), (6, 0)],
            &[(0, 15), (3, 45), (4, 0)],
            &[(0, 16), (1, 26), (2, 36), (3, 46), (4, 1), (5, 0), (6, 1)],
        ],
    );
    let top = [Some(11), Some(22), Some(40), Some(23), Some(15), Some(36)];
    assert_eq!(
        agree(oracle, &nested),
        [
            cell(10, &top),
            stream(&[&[11], &[22], &[40], &[23], &[15], &[36]]),
            cell(
                10,
                &[Some(11), Some(22), Some(23), None, Some(15), Some(26)]
            ),
            cell(30, &[None, None, Some(40), None, Some(45), Some(36)]),
        ]
    );
}

/// A switch_cell over two in-place accumulators is a State, and so is a
/// map_cell of it; its listeners hear each step after commit, the step at
/// the switch instant included, where the new State steps too: 3, 8, 32,
/// 321, 18.
#[test]
fn a_switch_cell_over_states_steps_as_a_state() {
    let Some(oracle) = oracle() else { return };
    let states = program(
        integers(2),
        vec![
            Definition::Input(0),
            Share(TopLevel(0)),
            AccumulateMut {
                initial: Literal(0),
                function: SecondArgument + Argument,
                source: TopLevel(1),
            },
            Filter {
                predicate: Argument.less_than(Literal(4)),
                source: TopLevel(1),
            },
            AccumulateMut {
                initial: Literal(0),
                function: SecondArgument * Literal(10) + Argument,
                source: TopLevel(3),
            },
            Definition::Input(1),
            PickCell {
                index: Argument,
                cells: vec![TopLevel(2), TopLevel(4)],
                source: TopLevel(5),
            },
            HoldCell {
                initial: TopLevel(2),
                source: TopLevel(6),
            },
            SwitchCell(TopLevel(7)),
            MapCell {
                function: Argument + Literal(1000),
                cell: TopLevel(8),
            },
        ],
        vec![8, 9],
        &[
            &[(0, 3)],
            &[(0, 5)],
            &[(0, 2), (1, 1)],
            &[(0, 1)],
            &[(0, 7), (1, 0)],
        ],
    );
    let state = NodeType::Cell {
        value: Scalar::Integer,
        state: true,
    };
    let types = check(&states).unwrap();
    assert_eq!([types[8], types[9]], [state; 2]);
    assert_eq!(
        agree(oracle, &states),
        [
            cell(0, &[Some(3), Some(8), Some(32), Some(321), Some(18)]),
            cell(
                1000,
                &[Some(1003), Some(1008), Some(1032), Some(1321), Some(1018)]
            ),
        ]
    );
}

// ----- fixed programs: constructs -----

/// A split built at a child instant splits nothing its input carried
/// before the split existed; the text's `Split`, which has no creation
/// time, does. A construct fed by a defer of x runs its closure at [1, 0],
/// and the body splits x, which fired at [1] with 1. In the text, the
/// children of that event are at [1, 0], [1, 1] and [1, 2], at and after
/// the split's creation, and an accumulator built with it takes all three:
/// 9, 20, 32. The engine's split exists from [1, 0] and takes nothing of
/// [1], as in Java's Sodium, where a stream forgets its firings in the
/// transaction's `last` actions, which `Transaction.close` runs before the
/// child transactions of its `post` queue; the accumulator stays at -1. A
/// defer is a split of one: in the text, a hold built at [1, 0] over a
/// defer built there takes x's event of [1] at [1, 0]. The text's `Execute`
/// and `SwitchS` have no creation time either, but they keep an event's
/// time, so what they carry from before a node's creation is before
/// anything built with it can observe (finding F44); a split moves it to a
/// later time, where it is observable. So the answers differ, and each is
/// pinned here: GHC's, the text's, and the engine's in both modes and every
/// run. The generator builds no split or defer in a body whose construct
/// may run at a child instant.
#[test]
fn a_split_built_at_a_child_instant_splits_nothing_from_before_it_unlike_the_text() {
    let Some(oracle) = oracle() else { return };
    let built = |definitions: Vec<Definition>, emitted: usize| {
        program(
            integers(1),
            vec![
                Definition::Input(0),
                Share(TopLevel(0)),
                Definition::Defer(TopLevel(1)),
                Construct {
                    body: body(definitions, BodyResult::Node(Reference::Local(emitted))),
                    source: TopLevel(2),
                },
                InputCell {
                    input: 0,
                    initial: Literal(0),
                },
                HoldCell {
                    initial: TopLevel(4),
                    source: TopLevel(3),
                },
                SwitchCell(TopLevel(5)),
            ],
            vec![6],
            &[&[(0, 1)]],
        )
    };
    let split = built(
        vec![
            MapList {
                length: Literal(3),
                element: Argument * Literal(10) + SecondArgument,
                source: TopLevel(1),
            },
            Definition::Split(Reference::Local(0)),
            Accumulate {
                initial: Literal(-1),
                function: SecondArgument + Argument,
                source: Reference::Local(1),
            },
        ],
        2,
    );
    let defer = built(
        vec![
            Definition::Defer(TopLevel(1)),
            Hold {
                initial: Literal(5),
                source: Reference::Local(0),
            },
        ],
        1,
    );
    // The switch follows the input cell at [1], and moves at [1, 0] to what
    // the body built.
    let text = |program: &Program| {
        expected(program, &oracle.answer(program).unwrap()).unwrap_or_else(|e| panic!("{e}"))
    };
    assert_eq!(
        text(&split),
        [timed_cell(
            0,
            1,
            &[(&[1], 1), (&[1, 0], 9), (&[1, 1], 20), (&[1, 2], 32)]
        )]
    );
    assert_eq!(text(&defer), [timed_cell(0, 1, &[(&[1], 1), (&[1, 0], 1)])]);
    let engine = [
        (&split, timed_cell(0, 1, &[(&[1], 1), (&[1, 0], -1)])),
        (&defer, timed_cell(0, 1, &[(&[1], 1), (&[1, 0], 5)])),
    ];
    for (program, created) in engine {
        for mode in ENGINES {
            for options in runs(0) {
                let run = (mode.run)(program, options).unwrap();
                if let Some(table) = compare(program, std::slice::from_ref(&created), &run) {
                    panic!("{} mode, {options}:\n{table}", mode.name);
                }
            }
        }
    }
}

// ----- the builder and the generator alone -----

/// Runs a program in `Local` mode, plainly.
fn run_local(program: &Program) -> bough_oracle::EngineRun {
    run::<Local>(program, RunOptions::default()).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn the_builder_fuses_two_adapters_into_the_materializer_and_gives_a_third_a_node() {
    let two = program(
        integers(1),
        vec![
            Definition::Input(0),
            Map {
                function: Argument * Literal(2),
                source: TopLevel(0),
            },
            Filter {
                predicate: Literal(4).less_than(Argument),
                source: TopLevel(1),
            },
            Hold {
                initial: Literal(0),
                source: TopLevel(2),
            },
        ],
        vec![3],
        &[&[(0, 1)], &[(0, 3)]],
    );
    let run = run_local(&two);
    // The input and the hold: the chain fused into the hold.
    assert_eq!(run.live_nodes, 2);
    // Threaded fuses one adapter, so the filter needs a node of the map.
    let threaded = bough_oracle::run::<Threaded>(&two, RunOptions::default()).unwrap();
    assert_eq!(threaded.live_nodes, 3);
    assert_eq!(threaded.observations, run.observations);
    assert_eq!(
        run.observations[0],
        bough_oracle::EngineObservation::Cell {
            registration: vec![0],
            steps_registration: vec![],
            values: vec![vec![], vec![6]],
            steps: vec![vec![], vec![6]],
            samples: vec![0, 6],
        }
    );
    let mut three = two.clone();
    three.definitions.insert(3, Once(TopLevel(2)));
    three.definitions[4] = Hold {
        initial: Literal(0),
        source: TopLevel(3),
    };
    three.observe = vec![4];
    // A node after two adapters, then the once fused into the hold.
    assert_eq!(run_local(&three).live_nodes, 3);
    // An input cell is two nodes, and a lift one.
    let cells = program(
        integers(2),
        vec![
            InputCell {
                input: 0,
                initial: Literal(1),
            },
            InputCell {
                input: 1,
                initial: Literal(2),
            },
            Lift {
                function: ArgumentAt(0) + ArgumentAt(1),
                cells: vec![TopLevel(0), TopLevel(1)],
            },
        ],
        vec![2],
        &[&[(0, 5), (1, 6)]],
    );
    let run = run_local(&cells);
    assert_eq!(run.live_nodes, 5);
    assert_eq!(
        run.observations[0],
        bough_oracle::EngineObservation::Cell {
            registration: vec![3],
            steps_registration: vec![],
            values: vec![vec![11]],
            steps: vec![vec![11]],
            samples: vec![11],
        }
    );
}

#[test]
fn the_builder_refuses_a_linear_stream_with_two_consumers_naming_the_node() {
    let twice = program(
        integers(1),
        vec![
            Definition::Input(0),
            Map {
                function: Argument,
                source: TopLevel(0),
            },
            Hold {
                initial: Literal(0),
                source: TopLevel(1),
            },
            Hold {
                initial: Literal(0),
                source: TopLevel(1),
            },
        ],
        vec![2, 3],
        &[],
    );
    let error = check(&twice).unwrap_err();
    assert_eq!(error.node, Some(1));
    assert_eq!(
        error.to_string(),
        "node 1 (Map): a linear stream has 2 consumers (node 2, node 3); \
         only a Share may have more than one"
    );
    // The observation is a consumer too.
    let observed = program(
        integers(1),
        vec![
            Definition::Input(0),
            Hold {
                initial: Literal(0),
                source: TopLevel(0),
            },
        ],
        vec![0, 1],
        &[],
    );
    assert!(
        check(&observed)
            .unwrap_err()
            .to_string()
            .contains("(node 1, the observation)")
    );
    // A share may have any number.
    let mut shared = twice.clone();
    shared.definitions[1] = Share(TopLevel(0));
    assert!(check(&shared).is_ok());
}

#[test]
fn the_builder_refuses_what_is_outside_the_subset_before_building() {
    let refused = |definitions: Vec<Definition>, observe: Vec<usize>| {
        let program = program(integers(1), definitions, observe, &[]);
        let error = run::<Local>(&program, RunOptions::default()).unwrap_err();
        assert_eq!(check(&program).unwrap_err(), error);
        error.to_string()
    };
    assert_eq!(
        refused(
            vec![Definition::Literal {
                event_type: Type::Integer,
                events: vec![(vec![1], Value::Integer(1))],
            }],
            vec![0]
        ),
        "node 0 (Literal): Literal is outside the subset: the engine cannot build a stream of \
         given events"
    );
    assert_eq!(
        refused(
            vec![
                Definition::Input(0),
                Definition::Hold {
                    initial: Literal(0),
                    source: TopLevel(0),
                },
                Definition::SwitchCell(TopLevel(1)),
            ],
            vec![1]
        ),
        "node 2 (SwitchCell): N 1 is a cell of integers; a switch reads a cell of tokens"
    );
    assert_eq!(
        refused(
            vec![
                Definition::Input(0),
                AccumulateMut {
                    initial: Literal(0),
                    function: Argument,
                    source: TopLevel(0),
                },
                MapCell {
                    function: Argument,
                    cell: TopLevel(1),
                },
                StepsWithCurrent(TopLevel(2)),
            ],
            vec![3]
        ),
        "node 3 (StepsWithCurrent): N 2 is a State, an in-place accumulator, a cell read \
         through one, or a loop closed with one, which has no stream view"
    );
    assert_eq!(
        refused(
            vec![
                Definition::Input(0),
                Map {
                    function: SecondArgument,
                    source: TopLevel(0),
                },
            ],
            vec![1]
        ),
        "node 1 (Map): Arg2 is not bound: this expression takes 1 argument(s)"
    );
    assert_eq!(
        refused(
            vec![
                Definition::Input(0),
                Definition::Gate {
                    source: TopLevel(0),
                    cell: TopLevel(0),
                },
            ],
            vec![1]
        ),
        "node 1 (Gate): N 0 is a stream, not a cell"
    );
    let double = program(
        integers(1),
        vec![Definition::Input(0)],
        vec![0],
        &[&[(0, 1), (0, 2)]],
    );
    assert_eq!(
        check(&double).unwrap_err().to_string(),
        "transaction 1: input 0 is sent more than once and does not coalesce"
    );
}

/// The builder declares each loop by its definition, as a user must: a
/// cell loop closed with a Cell is `cell_loop`, closed with an in-place
/// accumulator or a read-through cell over one is `state_loop`, whose
/// forward is a State, so a map_cell of it is a State and a steps view of
/// it is refused. A stream loop's definition is fused into the forward's
/// node, a defer and a split are two nodes each, and a MapList is a node of
/// its own.
#[test]
fn the_builder_declares_each_loop_by_its_definition() {
    let state = NodeType::Cell {
        value: Scalar::Integer,
        state: true,
    };
    let loops = program(
        integers(1),
        vec![
            CellLoop(Type::Integer),
            CellLoop(Type::Integer),
            Definition::Input(0),
            Share(TopLevel(2)),
            Snapshot {
                function: Argument + SecondArgument,
                source: TopLevel(3),
                cell: TopLevel(0),
            },
            AccumulateMut {
                initial: Literal(0),
                function: SecondArgument + Argument,
                source: TopLevel(4),
            },
            MapCell {
                function: Argument * Literal(2),
                cell: TopLevel(5),
            },
            // Loop 1 closes with a read-through cell over loop 0's
            // definition, and loop 0 with that definition: both are States.
            Close {
                forward: 1,
                definition: TopLevel(6),
            },
            Close {
                forward: 0,
                definition: TopLevel(5),
            },
            MapCell {
                function: Argument + Literal(1),
                cell: TopLevel(1),
            },
        ],
        vec![0, 1, 9],
        &[&[(0, 3)], &[(0, 4)]],
    );
    let types = check(&loops).unwrap();
    assert_eq!([types[0], types[1], types[9]], [state; 3]);
    assert_eq!(types[7], NodeType::Closed);
    let run = run_local(&loops);
    assert_eq!(
        run.observations[0],
        bough_oracle::EngineObservation::Cell {
            registration: vec![0],
            steps_registration: vec![],
            values: vec![vec![3], vec![10]],
            steps: vec![vec![3], vec![10]],
            samples: vec![3, 10],
        }
    );
    let mut stepped = loops.clone();
    stepped.definitions.push(Steps(TopLevel(1)));
    stepped.observe = vec![10];
    assert_eq!(
        check(&stepped).unwrap_err().to_string(),
        "node 10 (Steps): N 1 is a State, an in-place accumulator, a cell read through one, or \
         a loop closed with one, which has no stream view"
    );

    // A stream loop, a defer and a split with its MapList.
    let children = program(
        integers(1),
        vec![
            StreamLoop(Type::Integer),
            Hold {
                initial: Literal(0),
                source: TopLevel(0),
            },
            Definition::Input(0),
            Snapshot {
                function: Argument + SecondArgument,
                source: TopLevel(2),
                cell: TopLevel(1),
            },
            Close {
                forward: 0,
                definition: TopLevel(3),
            },
            Definition::Input(0),
            Definition::Defer(TopLevel(5)),
            MapList {
                length: Literal(2),
                element: Argument + SecondArgument,
                source: TopLevel(6),
            },
            Definition::Split(TopLevel(7)),
        ],
        vec![1, 8],
        &[&[(0, 5)]],
    );
    let run = run_local(&children);
    // The loop's node with its definition fused in, the hold, the loop's
    // input; the second input, the defer's two, the MapList's node and the
    // split's two.
    assert_eq!(run.live_nodes, 3 + 1 + 2 + 1 + 2);
    assert_eq!(
        run.observations[1],
        bough_oracle::EngineObservation::Stream {
            events: vec![vec![5, 6]]
        }
    );
}

/// What the engine cannot build, or the oracle would refuse, about loops
/// and children, the builder refuses before building, naming the node.
#[test]
fn the_builder_refuses_loops_and_children_it_cannot_build() {
    let refused = |definitions: Vec<Definition>, observe: Vec<usize>| {
        let program = program(integers(1), definitions, observe, &[]);
        let error = run::<Local>(&program, RunOptions::default()).unwrap_err();
        assert_eq!(check(&program).unwrap_err(), error);
        error.to_string()
    };
    let counter = |close: Option<usize>| {
        let mut definitions = vec![
            CellLoop(Type::Integer),
            Definition::Input(0),
            Snapshot {
                function: SecondArgument + Literal(1),
                source: TopLevel(1),
                cell: TopLevel(0),
            },
            Hold {
                initial: Literal(0),
                source: TopLevel(2),
            },
        ];
        definitions.extend(close.map(|definition| Close {
            forward: 0,
            definition: TopLevel(definition),
        }));
        definitions
    };
    assert_eq!(
        refused(counter(None), vec![3]),
        "node 0 (CellLoop): the loop is never closed"
    );
    let mut twice = counter(Some(3));
    twice.push(Close {
        forward: 0,
        definition: TopLevel(3),
    });
    assert_eq!(
        refused(twice, vec![3]),
        "node 5 (Close): the loop at node 0 is closed more than once"
    );
    assert_eq!(
        refused(counter(Some(2)), vec![3]),
        "node 4 (Close): the loop at node 0 is a cell of integers, and N 2 is a stream of \
         integers"
    );
    let mut not_a_loop = counter(Some(3));
    not_a_loop.push(Close {
        forward: 3,
        definition: TopLevel(3),
    });
    assert_eq!(
        refused(not_a_loop, vec![3]),
        "node 5 (Close): node 3 is not a loop declared before this Close"
    );
    // The engine has no value for a forward before its Close, directly or
    // through a read-through cell.
    let mut sampled = counter(None);
    sampled.insert(
        3,
        MapCell {
            function: Argument,
            cell: TopLevel(0),
        },
    );
    sampled[4] = Hold {
        initial: Expression::Sample(TopLevel(3)),
        source: TopLevel(2),
    };
    sampled.push(Close {
        forward: 0,
        definition: TopLevel(4),
    });
    assert_eq!(
        refused(sampled, vec![4]),
        "node 4 (Hold): a Sample reads N 0, a loop's forward that is not closed here; the \
         engine has no value for it before its Close"
    );
    let mut after = counter(Some(3));
    after.push(Constant(Expression::Sample(TopLevel(0))));
    assert!(check(&program(integers(1), after, vec![5], &[])).is_ok());
    // Lists: only a split reads them, and nothing observes them.
    let lists = |reader: Definition, observe: usize| {
        refused(
            vec![
                Definition::Input(0),
                MapList {
                    length: Literal(1),
                    element: Argument,
                    source: TopLevel(0),
                },
                reader,
            ],
            vec![observe],
        )
    };
    assert_eq!(
        lists(Definition::Defer(TopLevel(1)), 2),
        "node 2 (Defer): N 1 is a stream of lists, which only a Split reads"
    );
    assert_eq!(
        lists(Definition::Split(TopLevel(0)), 2),
        "node 2 (Split): N 0 is a stream of integers; a split reads the lists of a MapList"
    );
    assert_eq!(
        lists(Definition::Split(TopLevel(1)), 1),
        "observe: node 1 is a stream of lists; the comparison observes streams and cells of \
         integers and booleans"
    );
    assert_eq!(
        refused(counter(Some(3)), vec![4]),
        "observe: node 4 is a Close, which makes no node; the comparison observes streams and \
         cells of integers and booleans"
    );
}

/// The builder builds switches as a user writes them. A pick is
/// `map(..).node(b)` over a node, so a chain before it gets a node first,
/// and a hold of the picks is the outer; a constant of a token and a
/// map_cell to tokens are outers too. A switch_stream over shared streams
/// is a linear stream, a switch_cell over cells a Cell and over States a
/// State. The values are worked out by hand: the switch_stream forwards the
/// old stream at [2], where it switches; the switch_cell switches to a
/// quiet constant at [2], dropping its old inner's step there; the switch
/// over States follows the map_cell of c1, which steps at every
/// transaction, so it steps at each, to the value after the instant of the
/// State it selects.
#[test]
fn the_builder_builds_switches_as_a_user_writes_them() {
    let switches = Program {
        window: Window::FromFirstTransaction,
        inputs: vec![
            Input::new(Type::Integer),
            Input::new(Type::Integer),
            Input::new(Type::Integer),
            Input::new(Type::Integer),
            Input::new(Type::Boolean),
            Input::new(Type::Integer),
        ],
        definitions: vec![
            Definition::Input(0),
            Share(TopLevel(0)),
            Definition::Input(1),
            Map {
                function: Argument * Literal(10),
                source: TopLevel(2),
            },
            Share(TopLevel(3)),
            Definition::Input(2),
            Map {
                function: Argument + Literal(1),
                source: TopLevel(5),
            },
            Definition::PickStream {
                index: Argument,
                streams: vec![TopLevel(1), TopLevel(4)],
                source: TopLevel(6),
            },
            Definition::HoldStream {
                initial: TopLevel(1),
                source: TopLevel(7),
            },
            Definition::SwitchStream(TopLevel(8)),
            InputCell {
                input: 3,
                initial: Literal(7),
            },
            Constant(Literal(8)),
            Definition::Input(4),
            Definition::PickCell {
                index: Argument,
                cells: vec![TopLevel(10), TopLevel(11)],
                source: TopLevel(12),
            },
            Definition::HoldCell {
                initial: TopLevel(10),
                source: TopLevel(13),
            },
            Definition::SwitchCell(TopLevel(14)),
            Steps(TopLevel(15)),
            Definition::Input(5),
            Share(TopLevel(17)),
            AccumulateMut {
                initial: Literal(0),
                function: SecondArgument + Argument,
                source: TopLevel(18),
            },
            AccumulateMut {
                initial: Literal(100),
                function: SecondArgument - Argument,
                source: TopLevel(18),
            },
            Definition::MapPickCell {
                index: Argument,
                cells: vec![TopLevel(19), TopLevel(20)],
                cell: TopLevel(10),
            },
            Definition::SwitchCell(TopLevel(21)),
            Definition::ConstantStream(TopLevel(4)),
            Definition::SwitchStream(TopLevel(23)),
        ],
        observe: vec![9, 15, 16, 22, 24],
        schedule: vec![
            vec![
                (0, Value::Integer(1)),
                (1, Value::Integer(2)),
                (2, Value::Integer(-1)),
                (3, Value::Integer(9)),
                (5, Value::Integer(5)),
            ],
            vec![
                (0, Value::Integer(3)),
                (1, Value::Integer(4)),
                (2, Value::Integer(0)),
                (3, Value::Integer(10)),
                (4, Value::Boolean(true)),
                (5, Value::Integer(6)),
            ],
            vec![
                (0, Value::Integer(5)),
                (1, Value::Integer(6)),
                (3, Value::Integer(11)),
                (5, Value::Integer(7)),
            ],
        ],
    };
    let types = check(&switches).unwrap();
    let integers = bough_oracle::Held::Streams(Scalar::Integer);
    assert_eq!(types[7], NodeType::Tokens(integers));
    assert_eq!(types[8], NodeType::Outer(integers));
    assert_eq!(
        types[21],
        NodeType::Outer(bough_oracle::Held::Cells { state: true })
    );
    assert_eq!(
        types[22],
        NodeType::Cell {
            value: Scalar::Integer,
            state: true
        },
        "a switch over States is a State"
    );
    let run = run_local(&switches);
    // Five inputs and an input cell's two nodes; two shares; the node the
    // chain before the first pick gets, and the pick; a hold and a switch;
    // a constant, a pick, a hold, a switch and a steps view; a share, two
    // accumulators, a map_cell and a switch; a constant and a switch.
    assert_eq!(run.live_nodes, 7 + 2 + 4 + 5 + 5 + 2);
    let stream = |events: Vec<Vec<i64>>| bough_oracle::EngineObservation::Stream { events };
    let cell = |registration: i64, steps: Vec<Vec<i64>>, samples: Vec<i64>| {
        bough_oracle::EngineObservation::Cell {
            registration: vec![registration],
            steps_registration: vec![],
            values: steps.clone(),
            steps,
            samples,
        }
    };
    assert_eq!(
        run.observations,
        [
            stream(vec![vec![1], vec![3], vec![60]]),
            cell(7, vec![vec![9], vec![8], vec![]], vec![9, 8, 8]),
            stream(vec![vec![9], vec![8], vec![]]),
            cell(100, vec![vec![95], vec![11], vec![82]], vec![95, 11, 82]),
            stream(vec![vec![20], vec![40], vec![60]]),
        ]
    );
    let threaded = bough_oracle::run::<Threaded>(&switches, RunOptions::default()).unwrap();
    assert_eq!(threaded.observations, run.observations);
    // A candidate no selection has reached yet lives only by the builder's
    // depends declarations; collecting at every transaction shows it.
    let collecting = RunOptions {
        collect_every_transaction: true,
        ..RunOptions::default()
    };
    for collected in [
        bough_oracle::run::<Local>(&switches, collecting).unwrap(),
        bough_oracle::run::<Threaded>(&switches, collecting).unwrap(),
    ] {
        assert_eq!(collected.observations, run.observations);
    }
}

/// What the engine cannot switch among, or the oracle would refuse, the
/// builder refuses before building, naming the node.
#[test]
fn the_builder_refuses_switches_it_cannot_build() {
    let refused = |definitions: Vec<Definition>, observe: Vec<usize>| {
        let program = program(integers(2), definitions, observe, &[]);
        let error = run::<Local>(&program, RunOptions::default()).unwrap_err();
        assert_eq!(check(&program).unwrap_err(), error);
        error.to_string()
    };
    let accumulate_mut = |source: usize| AccumulateMut {
        initial: Literal(0),
        function: SecondArgument + Argument,
        source: TopLevel(source),
    };
    let pick_stream = |streams: Vec<usize>, source: usize| Definition::PickStream {
        index: Argument,
        streams: streams.into_iter().map(TopLevel).collect(),
        source: TopLevel(source),
    };
    let pick_cell = |cells: Vec<usize>, source: usize| Definition::PickCell {
        index: Argument,
        cells: cells.into_iter().map(TopLevel).collect(),
        source: TopLevel(source),
    };
    assert_eq!(
        refused(
            vec![
                Definition::Input(0),
                Definition::Input(1),
                pick_stream(vec![0], 1),
            ],
            vec![0]
        ),
        "node 2 (PickStream): N 0 is a linear stream; a pick may select a stream many times, so \
         a switch here follows a Share"
    );
    assert_eq!(
        refused(
            vec![
                InputCell {
                    input: 0,
                    initial: Literal(0),
                },
                Definition::ToBoolean(TopLevel(0)),
                Definition::Input(1),
                pick_cell(vec![1], 2),
            ],
            vec![0]
        ),
        "node 3 (PickCell): N 1 is a cell of booleans; a switch here follows cells of integers"
    );
    assert_eq!(
        refused(
            vec![
                Definition::Input(0),
                Share(TopLevel(0)),
                accumulate_mut(1),
                Constant(Literal(1)),
                pick_cell(vec![2, 3], 1),
            ],
            vec![2]
        ),
        "node 4 (PickCell): the listed cells mix Cells and States, which the engine types apart"
    );
    assert_eq!(
        refused(
            vec![
                Definition::Input(0),
                accumulate_mut(0),
                Constant(Literal(1)),
                Definition::MapPickCell {
                    index: Argument,
                    cells: vec![TopLevel(2)],
                    cell: TopLevel(1),
                },
            ],
            vec![2]
        ),
        "node 3 (MapPickCell): N 1 is a State; a map_cell of it would be a State of cells, which \
         the engine has no switch over"
    );
    assert_eq!(
        refused(
            vec![
                Definition::Input(0),
                Share(TopLevel(0)),
                Definition::Input(1),
                pick_stream(vec![1], 2),
                Definition::Node(TopLevel(3)),
            ],
            vec![4]
        ),
        "node 4 (Node): N 3 is a stream of shared streams of integers, which only a hold of \
         them reads"
    );
    assert_eq!(
        refused(
            vec![
                Definition::Input(0),
                Share(TopLevel(0)),
                Definition::Input(1),
                pick_stream(vec![1], 2),
                Definition::HoldStream {
                    initial: TopLevel(1),
                    source: TopLevel(3),
                },
                Definition::HoldStream {
                    initial: TopLevel(1),
                    source: TopLevel(3),
                },
            ],
            vec![1]
        ),
        "node 3 (PickStream): a linear stream has 2 consumers (node 4, node 5); only a Share may \
         have more than one"
    );
    assert_eq!(
        refused(
            vec![
                Definition::Input(0),
                Share(TopLevel(0)),
                accumulate_mut(1),
                Constant(Literal(1)),
                pick_cell(vec![3], 1),
                Definition::HoldCell {
                    initial: TopLevel(2),
                    source: TopLevel(4),
                },
            ],
            vec![2]
        ),
        "node 5 (HoldCell): N 4 carries cells of integers, and the initial N 2 is a State of \
         integers"
    );
    assert_eq!(
        refused(
            vec![
                Constant(Literal(1)),
                Definition::ConstantCell(TopLevel(0)),
                Definition::SwitchStream(TopLevel(1)),
            ],
            vec![2]
        ),
        "node 2 (SwitchStream): N 1 is a cell of cells of integers; switch_stream needs a cell \
         of streams"
    );
    assert_eq!(
        refused(
            vec![
                Definition::Input(0),
                Share(TopLevel(0)),
                Definition::ConstantStream(TopLevel(1)),
            ],
            vec![2]
        ),
        "observe: node 2 is a cell of shared streams of integers; the comparison observes \
         streams and cells of integers and booleans"
    );
    // A Sample of a switch that may select a loop closed with the switch
    // would not end in the engine (finding F49).
    let f49 = refused(
        vec![
            CellLoop(Type::Integer),
            Definition::ConstantCell(TopLevel(0)),
            Definition::SwitchCell(TopLevel(1)),
            Close {
                forward: 0,
                definition: TopLevel(2),
            },
            Constant(Expression::Sample(TopLevel(2))),
        ],
        vec![4],
    );
    assert!(
        f49.starts_with("node 4 (Constant): a Sample reads ")
            && f49.ends_with("through a loop closed with itself"),
        "{f49}"
    );
}

/// A construct's body as the builder takes it.
fn body(definitions: Vec<Definition>, result: BodyResult) -> Body {
    Body {
        definitions,
        result,
    }
}

/// The builder builds constructs as a user writes them. The chain before a
/// construct gets a node, and the construct is that node's `construct`,
/// whose closure builds the body with the build context it is handed, at
/// the event's instant, and emits a value or a token: here a value, a cell,
/// a linear stream, a State, and a shared stream, which a switch_stream in
/// the body takes from a constant of a linear stream it built. Holds of the
/// tokens feed a switch each. The values are worked out by hand: each body
/// runs at [2] and [4], where `go` fires, and what it builds over `x`,
/// which fires then too, takes x's event there; a switch_cell steps at the
/// move to the new inner's value after the instant, dropping the old one's
/// step, and a switch_stream forwards its old inner at the move and the new
/// one after it.
#[test]
fn the_builder_builds_constructs_as_a_user_writes_them() {
    let constructs = Program {
        window: Window::FromFirstTransaction,
        inputs: integers(3),
        definitions: vec![
            Definition::Input(0),
            Share(TopLevel(0)),
            Definition::Input(1),
            Share(TopLevel(2)),
            Constant(Literal(0)),
            // The event, and a hold's value before the instant: its initial.
            Construct {
                body: body(
                    vec![Hold {
                        initial: ConstructEvent,
                        source: TopLevel(3),
                    }],
                    BodyResult::Value(ConstructEvent * Literal(100) + Sample(Reference::Local(0))),
                ),
                source: TopLevel(1),
            },
            // A hold of x plus the event, which takes x's event at [2].
            Construct {
                body: body(
                    vec![
                        Map {
                            function: Argument + ConstructEvent,
                            source: TopLevel(3),
                        },
                        Hold {
                            initial: Literal(0),
                            source: Reference::Local(0),
                        },
                    ],
                    BodyResult::Node(Reference::Local(1)),
                ),
                source: TopLevel(1),
            },
            HoldCell {
                initial: TopLevel(4),
                source: TopLevel(6),
            },
            SwitchCell(TopLevel(7)),
            // Linear screens from a first one.
            Definition::Input(2),
            Construct {
                body: body(
                    vec![Map {
                        function: Argument + ConstructEvent * Literal(1000),
                        source: TopLevel(3),
                    }],
                    BodyResult::Node(Reference::Local(0)),
                ),
                source: TopLevel(1),
            },
            HoldStream {
                initial: TopLevel(9),
                source: TopLevel(10),
            },
            SwitchStream(TopLevel(11)),
            // States from the event on.
            Construct {
                body: body(
                    vec![AccumulateMut {
                        initial: ConstructEvent,
                        function: SecondArgument + Argument,
                        source: TopLevel(3),
                    }],
                    BodyResult::Node(Reference::Local(0)),
                ),
                source: TopLevel(1),
            },
            AccumulateMut {
                initial: Literal(0),
                function: SecondArgument + Argument,
                source: TopLevel(3),
            },
            HoldCell {
                initial: TopLevel(14),
                source: TopLevel(13),
            },
            SwitchCell(TopLevel(15)),
            // Shared streams, from x itself.
            Construct {
                body: body(
                    vec![
                        Map {
                            function: Argument * Literal(2),
                            source: TopLevel(3),
                        },
                        ConstantStream(Reference::Local(0)),
                        SwitchStream(Reference::Local(1)),
                        Share(Reference::Local(2)),
                    ],
                    BodyResult::Node(Reference::Local(3)),
                ),
                source: TopLevel(1),
            },
            HoldStream {
                initial: TopLevel(3),
                source: TopLevel(17),
            },
            SwitchStream(TopLevel(18)),
        ],
        observe: vec![5, 8, 12, 16, 19],
        schedule: vec![
            vec![(1, Value::Integer(1)), (2, Value::Integer(7))],
            vec![(0, Value::Integer(2)), (1, Value::Integer(3))],
            vec![(1, Value::Integer(4))],
            vec![
                (0, Value::Integer(5)),
                (1, Value::Integer(6)),
                (2, Value::Integer(9)),
            ],
            vec![(1, Value::Integer(7))],
        ],
    };
    let types = check(&constructs).unwrap();
    assert_eq!(types[5], NodeType::Stream(Scalar::Integer));
    assert_eq!(
        [types[6], types[10], types[13], types[17]],
        [
            NodeType::Tokens(Held::Cells { state: false }),
            NodeType::Tokens(Held::Linear(Scalar::Integer)),
            NodeType::Tokens(Held::Cells { state: true }),
            NodeType::Tokens(Held::Streams(Scalar::Integer)),
        ]
    );
    assert_eq!(types[11], NodeType::Outer(Held::Linear(Scalar::Integer)));
    let run = run_local(&constructs);
    // Three inputs, two shares, a constant; five constructs, each over a
    // share, so a node each; four holds of tokens and four switches; an
    // accumulator. The first screen is the input's own node.
    assert_eq!(run.live_nodes, 3 + 2 + 1 + 5 + 4 + 4 + 1);
    let stream = |events: Vec<Vec<i64>>| bough_oracle::EngineObservation::Stream { events };
    let cell = |registration: i64, steps: Vec<Vec<i64>>, samples: Vec<i64>| {
        bough_oracle::EngineObservation::Cell {
            registration: vec![registration],
            steps_registration: vec![],
            values: steps.clone(),
            steps,
            samples,
        }
    };
    let expected = [
        stream(vec![vec![], vec![202], vec![], vec![505], vec![]]),
        cell(
            0,
            vec![vec![], vec![5], vec![6], vec![11], vec![12]],
            vec![0, 5, 6, 11, 12],
        ),
        stream(vec![vec![7], vec![], vec![2004], vec![2006], vec![5007]]),
        cell(
            0,
            vec![vec![1], vec![5], vec![9], vec![11], vec![18]],
            vec![1, 5, 9, 11, 18],
        ),
        stream(vec![vec![1], vec![3], vec![8], vec![12], vec![14]]),
    ];
    assert_eq!(run.observations, expected);
    // Every capture is declared, so collecting as every transaction opens
    // changes nothing, in either mode.
    let collecting = RunOptions {
        collect_every_transaction: true,
        ..RunOptions::default()
    };
    for run in [
        bough_oracle::run::<Threaded>(&constructs, RunOptions::default()).unwrap(),
        bough_oracle::run::<Local>(&constructs, collecting).unwrap(),
        bough_oracle::run::<Threaded>(&constructs, collecting).unwrap(),
    ] {
        assert_eq!(run.observations, expected);
    }
}

/// What the engine cannot build in a construct body, or the oracle would
/// refuse, the builder refuses before building, naming the construct and
/// the body's node.
#[test]
fn the_builder_refuses_constructs_it_cannot_build() {
    let refused = |definitions: Vec<Definition>, observe: Vec<usize>| {
        let program = program(integers(2), definitions, observe, &[]);
        let error = run::<Local>(&program, RunOptions::default()).unwrap_err();
        assert_eq!(check(&program).unwrap_err(), error);
        error.to_string()
    };
    let construct = |definitions: Vec<Definition>, result: BodyResult| Construct {
        body: body(definitions, result),
        source: TopLevel(1),
    };
    // Node 0 is a linear stream, node 1 a Share, node 2 a cell.
    let with = |built: Definition| {
        vec![
            Definition::Input(0),
            Share(TopLevel(0)),
            InputCell {
                input: 1,
                initial: Literal(0),
            },
            built,
        ]
    };
    let value = BodyResult::Value(ConstructEvent);
    assert_eq!(
        refused(
            with(construct(vec![CellLoop(Type::Integer)], value.clone())),
            vec![3]
        ),
        "node 3 (Construct): body node 0 (CellLoop): a loop in a construct body is outside the \
         subset: the oracle builds a body's runs for events before its construct existed (F44), \
         and a loop there may not settle"
    );
    assert_eq!(
        refused(
            with(construct(vec![Definition::Input(0)], value.clone())),
            vec![3]
        ),
        "node 3 (Construct): body node 0 (Input): an input a construct body builds is outside \
         the subset: I/O code would receive its token and wire it after the transaction"
    );
    assert_eq!(
        refused(
            with(construct(
                vec![Hold {
                    initial: Literal(0),
                    source: TopLevel(0),
                }],
                value.clone()
            )),
            vec![3]
        ),
        "node 3 (Construct): body node 0 (Hold): N 0 is a linear stream, which a construct body \
         cannot consume, since each run would consume it again; a body reads a Share"
    );
    assert_eq!(
        refused(
            with(construct(
                vec![Definition::ToBoolean(TopLevel(2))],
                BodyResult::Node(Reference::Local(0))
            )),
            vec![1]
        ),
        "node 3 (Construct): body result: Local 0 is a cell of booleans; a body emits a stream, \
         or a cell of integers, for a switch to follow"
    );
    assert_eq!(
        refused(
            with(construct(
                vec![
                    Map {
                        function: Argument,
                        source: TopLevel(1),
                    },
                    Hold {
                        initial: Literal(0),
                        source: Reference::Local(0),
                    },
                ],
                BodyResult::Node(Reference::Local(0))
            )),
            vec![1]
        ),
        "node 3 (Construct): body node 0 (Map): a linear stream has 2 consumers (body node 1, \
         the result); only a Share may have more than one"
    );
    assert_eq!(
        refused(
            with(construct(
                vec![],
                BodyResult::Value(Sample(Reference::Local(0)))
            )),
            vec![1]
        ),
        "node 3 (Construct): body result: Local 0 does not name a node of this body defined \
         before this one"
    );
    assert_eq!(
        refused(with(Constant(ConstructEvent)), vec![3]),
        "node 3 (Constant): CArg is used outside a construct body"
    );
    // A cell of linear streams has one switch_stream, and a body would
    // build another at each run.
    let screens = |switches: Vec<Definition>| {
        let mut definitions = with(construct(
            vec![Map {
                function: Argument,
                source: TopLevel(1),
            }],
            BodyResult::Node(Reference::Local(0)),
        ));
        definitions.push(Definition::Input(0));
        definitions.push(HoldStream {
            initial: TopLevel(4),
            source: TopLevel(3),
        });
        definitions.extend(switches);
        definitions
    };
    assert_eq!(
        refused(
            screens(vec![SwitchStream(TopLevel(5)), SwitchStream(TopLevel(5))]),
            vec![6]
        ),
        "node 5 (HoldStream): a cell of linear streams has 2 switch_streams (node 6, node 7); it \
         may have one"
    );
    assert_eq!(
        refused(
            screens(vec![construct(
                vec![SwitchStream(TopLevel(5))],
                BodyResult::Node(Reference::Local(0))
            )]),
            vec![1]
        ),
        "node 6 (Construct): body node 0 (SwitchStream): N 5 is a cell of linear streams of \
         integers, which has one switch_stream, and a construct body would build one at every run"
    );
    // A hold of linear streams starts from a linear stream.
    let mut shared_first = screens(vec![]);
    shared_first[5] = HoldStream {
        initial: TopLevel(1),
        source: TopLevel(3),
    };
    assert_eq!(
        refused(shared_first, vec![1]),
        "node 5 (HoldStream): N 1 is a Share, and a cell of linear streams holds linear streams"
    );
    assert_eq!(
        refused(
            vec![
                Definition::Input(0),
                Hold {
                    initial: Literal(0),
                    source: Reference::Local(0),
                }
            ],
            vec![1]
        ),
        "node 1 (Hold): Local 0 names a construct body's node, and this is the top level"
    );
}

/// The comparison holds the order of listener calls across nodes to the
/// oracle's times: a call for an event at an earlier time must come before
/// a call for one at a later time. Here two streams agree event by event,
/// and the engine's calls come in time order, then out of it.
#[test]
fn the_comparison_holds_listener_calls_across_nodes_to_time_order() {
    let two = program(
        integers(1),
        vec![
            Definition::Input(0),
            Definition::Defer(TopLevel(0)),
            Definition::Input(0),
        ],
        vec![1, 2],
        &[&[(0, 1)]],
    );
    let expected = [
        timed_stream(1, &[(&[1, 0], 1)]),
        timed_stream(1, &[(&[1], 1)]),
    ];
    let call = |observed: usize| bough_oracle::Call {
        observed,
        listened: bough_oracle::Listened::Stream,
        transaction: 0,
        index: 0,
    };
    let mut run = bough_oracle::EngineRun {
        observations: vec![
            bough_oracle::EngineObservation::Stream {
                events: vec![vec![1]],
            },
            bough_oracle::EngineObservation::Stream {
                events: vec![vec![1]],
            },
        ],
        calls: vec![call(1), call(0)],
        live_nodes: 4,
    };
    assert_eq!(compare(&two, &expected, &run), None);
    run.calls.reverse();
    let table = compare(&two, &expected, &run).expect("the calls are out of time order");
    assert!(
        table.contains(
            "listener order: in transaction [1] the engine called observed 0 (node 1, \
             Defer)'s listen for [1, 0] before observed 1 (node 2, Input)'s listen for [1], a \
             call for an earlier time"
        ),
        "{table}"
    );
}

/// The oracle's events and steps at child times belong to their external
/// transaction, sorted by time, with their times kept for the order check.
/// Two events of one node at one time, or an event outside the schedule,
/// are the oracle's error, not a disagreement.
#[test]
fn the_comparison_reads_child_times_into_their_external_transaction() {
    let two = program(
        integers(1),
        vec![
            Definition::Input(0),
            Definition::Defer(TopLevel(0)),
            Hold {
                initial: Literal(7),
                source: TopLevel(1),
            },
        ],
        vec![1, 2],
        &[&[], &[]],
    );
    let answer = Answer::Observed(vec![
        Observation::stream([(vec![2, 0, 0], 3), (vec![1], 1), (vec![2], 2)]),
        Observation::cell(7, [(vec![1, 1], 8), (vec![1, 0], 9)]),
    ]);
    assert_eq!(
        expected(&two, &answer).unwrap(),
        [
            timed_stream(2, &[(&[1], 1), (&[2], 2), (&[2, 0, 0], 3)]),
            Expected::Cell {
                initial: 7,
                steps: vec![vec![(vec![1, 0], 9), (vec![1, 1], 8)], vec![]],
                values: vec![8, 8],
            },
        ]
    );
    let quiet = Observation::cell(7, Vec::<(Vec<i64>, i64)>::new());
    let twice = Answer::Observed(vec![
        Observation::stream([(vec![1, 0], 1), (vec![1, 0], 2)]),
        quiet.clone(),
    ]);
    assert_eq!(
        expected(&two, &twice).unwrap_err(),
        "the oracle answered two events of one node at [1, 0]"
    );
    let late = Answer::Observed(vec![Observation::stream([(vec![3, 0], 1)]), quiet]);
    assert_eq!(
        expected(&two, &late).unwrap_err(),
        "the oracle answered an event at [3, 0], which is in no transaction of the schedule"
    );
}

/// What kinds of loop, child transaction and switch a program has.
#[derive(Clone, Copy, Debug, Default)]
struct Census {
    loops: bool,
    cell_loops: bool,
    state_loops: bool,
    stream_loops: bool,
    splits: bool,
    defers: bool,
    /// A split or a defer on a loop's cycle: a guarded one.
    children_in_loops: bool,
    /// A split or a defer, or a loop.
    loops_or_children: bool,
    switch_streams: bool,
    switch_cells: bool,
    /// A switch_cell over States.
    switch_states: bool,
    /// A switch that may select a switch, or a share of one.
    nested_switches: bool,
    /// A pick whose selector is a defer or a split.
    selectors_in_children: bool,
    /// A loop closed with a switch, or with a hold of a switch_stream's
    /// events: the generator's loops through a switch's selection, and any
    /// other loop a switch closes.
    loops_closed_with_switches: bool,
    constructs: bool,
    /// A construct in a construct's body.
    nested_constructs: bool,
    /// A construct whose source is a split or a defer, whose closure runs
    /// in child instants.
    constructs_in_children: bool,
    /// A construct that emits linear streams: RFD 4's dynamic pattern.
    linear_tokens: bool,
    /// A construct whose source is a stream loop's forward: RFD 2's
    /// navigation loop.
    navigation: bool,
    /// A body's switch_cell over a top-level outer, which may have switched
    /// before the body ran (F6).
    late_switches: bool,
    /// A body's steps_with_current, which fires at the body's instant.
    late_steps: bool,
}

/// Every construct body of a program, those nested in bodies included.
fn bodies(program: &Program) -> Vec<&Body> {
    fn nested<'a>(definitions: &'a [Definition], found: &mut Vec<&'a Body>) {
        for definition in definitions {
            if let Construct { body, .. } = definition {
                found.push(body);
                nested(&body.definitions, found);
            }
        }
    }
    let mut found = Vec::new();
    nested(&program.definitions, &mut found);
    found
}

fn census(program: &Program) -> Census {
    let types = check(program).expect("a generated program passes check");
    let mut census = Census::default();
    let definition = |reference: &Reference| match reference {
        TopLevel(node) => program.definitions.get(*node),
        Reference::Local(_) => None,
    };
    let is_switch = |reference: &Reference| match definition(reference) {
        Some(Definition::SwitchCell(_) | Definition::SwitchStream(_)) => true,
        Some(Share(source)) => {
            matches!(definition(source), Some(Definition::SwitchStream(_)))
        }
        _ => false,
    };
    for (node, definition) in program.definitions.iter().enumerate() {
        match definition {
            Definition::SwitchStream(TopLevel(outer)) | Definition::SwitchCell(TopLevel(outer)) => {
                if matches!(definition, Definition::SwitchStream(_)) {
                    census.switch_streams = true;
                } else {
                    census.switch_cells = true;
                    census.switch_states |= matches!(
                        types[node],
                        bough_oracle::NodeType::Cell { state: true, .. }
                    );
                }
                census.nested_switches |=
                    bough_oracle::switch_candidates(&program.definitions, *outer)
                        .iter()
                        .any(is_switch);
            }
            Close {
                definition: TopLevel(closing),
                ..
            } => {
                census.loops_closed_with_switches |= match program.definitions.get(*closing) {
                    Some(Definition::SwitchStream(_) | Definition::SwitchCell(_)) => true,
                    Some(Hold {
                        source: TopLevel(held),
                        ..
                    }) => matches!(
                        program.definitions.get(*held),
                        Some(Definition::SwitchStream(_))
                    ),
                    _ => false,
                };
            }
            Definition::PickStream { source, .. } | Definition::PickCell { source, .. } => {
                census.selectors_in_children |= matches!(
                    program.definitions.get(match source {
                        TopLevel(node) => *node,
                        Reference::Local(_) => usize::MAX,
                    }),
                    Some(Definition::Defer(_) | Definition::Split(_))
                );
            }
            Definition::CellLoop(_) => {
                census.cell_loops = true;
                census.state_loops |= matches!(
                    types[node],
                    bough_oracle::NodeType::Cell { state: true, .. }
                );
            }
            Definition::StreamLoop(_) => census.stream_loops = true,
            Definition::Split(_) => census.splits = true,
            Definition::Defer(_) => census.defers = true,
            Definition::Filter { predicate, .. } if *predicate == bough_oracle::guard_filter() => {
                census.children_in_loops = true;
            }
            _ => {}
        }
    }
    for (node, definition) in program.definitions.iter().enumerate() {
        if let Construct { source, .. } = definition {
            census.constructs = true;
            census.constructs_in_children |= matches!(
                definition_of(program, source),
                Some(Definition::Split(_) | Definition::Defer(_))
            );
            census.navigation |= matches!(definition_of(program, source), Some(StreamLoop(_)));
            census.linear_tokens |= types[node] == NodeType::Tokens(Held::Linear(Scalar::Integer));
        }
    }
    let bodies = bodies(program);
    census.nested_constructs = bodies.iter().any(|body| {
        body.definitions
            .iter()
            .any(|d| matches!(d, Construct { .. }))
    });
    census.late_switches = bodies.iter().any(|body| {
        body.definitions
            .iter()
            .any(|d| matches!(d, SwitchCell(TopLevel(_))))
    });
    census.late_steps = bodies.iter().any(|body| {
        body.definitions
            .iter()
            .any(|d| matches!(d, StepsWithCurrent(_)))
    });
    census.loops = census.cell_loops || census.stream_loops;
    census.loops_or_children = census.loops || census.splits || census.defers;
    census
}

/// The top-level definition a reference names, if it names one.
fn definition_of<'a>(program: &'a Program, reference: &Reference) -> Option<&'a Definition> {
    match reference {
        TopLevel(node) => program.definitions.get(*node),
        Reference::Local(_) => None,
    }
}

/// Every generated program passes the builder's check and is well founded,
/// and across a few hundred of them every definition kind of the subset
/// occurs and every materialized kind is observed. Loops, child
/// transactions and switches are in most programs.
#[test]
fn the_generator_makes_well_formed_programs_with_every_kind() {
    let mut runner = TestRunner::new(Config {
        rng_seed: RngSeed::Fixed(20260924),
        ..Config::default()
    });
    let strategy = programs();
    let mut defined = std::collections::BTreeSet::new();
    let mut observed = std::collections::BTreeSet::new();
    let (mut shared_diamonds, mut lifts, mut sizes, mut watched) = (0, 0, Vec::new(), 0);
    let mut counts = [0_usize; 21];
    // What construct bodies define, and how many definitions each has.
    let mut in_bodies = std::collections::BTreeSet::new();
    let mut body_sizes = Vec::new();
    const PROGRAMS: usize = 400;
    for _ in 0..PROGRAMS {
        let program = strategy.new_tree(&mut runner).unwrap().current();
        check(&program).unwrap_or_else(|error| panic!("{error}\n{program:?}"));
        assert!(
            bough_oracle::well_founded(&program),
            "a generated program is not well founded: {program:?}"
        );
        assert!((5..=bough_oracle::MAX_DEFINITIONS).contains(&program.definitions.len()));
        assert!((1..=10).contains(&program.schedule.len()));
        sizes.push(program.definitions.len());
        watched += program.observe.len();
        for definition in &program.definitions {
            defined.insert(bough_oracle::name(definition));
            if let Lift { cells, .. } = definition {
                lifts |= 1 << cells.len();
            }
        }
        for &node in &program.observe {
            observed.insert(bough_oracle::name(&program.definitions[node]));
        }
        for body in bodies(&program) {
            body_sizes.push(body.definitions.len());
            for definition in &body.definitions {
                in_bodies.insert(bough_oracle::name(definition));
            }
        }
        // A diamond: a Share read by two nodes that both lead to one node.
        let readers = |node: usize| -> Vec<usize> {
            (0..program.definitions.len())
                .filter(|&j| bough_oracle::references(&program.definitions[j]).contains(&node))
                .collect()
        };
        for (node, definition) in program.definitions.iter().enumerate() {
            if matches!(definition, Share(_)) && readers(node).len() >= 2 {
                shared_diamonds += 1;
            }
        }
        let c = census(&program);
        for (count, has) in counts.iter_mut().zip([
            c.loops,
            c.cell_loops,
            c.state_loops,
            c.stream_loops,
            c.splits,
            c.defers,
            c.children_in_loops,
            c.loops_or_children,
            c.switch_streams,
            c.switch_cells,
            c.switch_states,
            c.nested_switches,
            c.selectors_in_children,
            c.loops_closed_with_switches,
            c.constructs,
            c.nested_constructs,
            c.constructs_in_children,
            c.linear_tokens,
            c.navigation,
            c.late_switches,
            c.late_steps,
        ]) {
            *count += usize::from(has);
        }
    }
    let every = [
        "Input",
        "InputCell",
        "Never",
        "Constant",
        "Map",
        "Filter",
        "FilterMap",
        "MapTo",
        "Snapshot",
        "Gate",
        "Once",
        "Node",
        "Share",
        "Merge",
        "OrElse",
        "Hold",
        "Accumulate",
        "AccumulateMut",
        "Scan",
        "MapCell",
        "ToBoolean",
        "Lift",
        "Steps",
        "StepsWithCurrent",
        "CellLoop",
        "StreamLoop",
        "Close",
        "MapList",
        "Split",
        "Defer",
        "PickStream",
        "PickCell",
        "HoldStream",
        "HoldCell",
        "ConstantStream",
        "ConstantCell",
        "MapPickCell",
        "SwitchStream",
        "SwitchCell",
        "Construct",
    ];
    for kind in every {
        assert!(defined.contains(kind), "no {kind} in {PROGRAMS} programs");
    }
    // What the bodies build: the shapes of stage 6.
    for kind in [
        "Hold",
        "Map",
        "Filter",
        "FilterMap",
        "MapTo",
        "Snapshot",
        "Gate",
        "StepsWithCurrent",
        "Steps",
        "SwitchCell",
        "SwitchStream",
        "Accumulate",
        "AccumulateMut",
        "Scan",
        "MapCell",
        "Lift",
        "Merge",
        "OrElse",
        "Once",
        "Split",
        "Defer",
        "Constant",
        "Construct",
        "HoldCell",
        "HoldStream",
        "Share",
    ] {
        assert!(
            in_bodies.contains(kind),
            "no {kind} in a construct body in {PROGRAMS} programs"
        );
    }
    for kind in [
        "Input",
        "InputCell",
        "Never",
        "Constant",
        "Node",
        "Share",
        "Merge",
        "OrElse",
        "Hold",
        "Accumulate",
        "AccumulateMut",
        "Scan",
        "MapCell",
        "ToBoolean",
        "Lift",
        "Steps",
        "StepsWithCurrent",
        "CellLoop",
        "StreamLoop",
        "Split",
        "Defer",
        "SwitchStream",
        "SwitchCell",
    ] {
        assert!(
            observed.contains(kind),
            "no {kind} observed in {PROGRAMS} programs"
        );
    }
    // Lifts of two to six cells.
    assert_eq!(lifts, 0b111_1100, "lift arities seen: {lifts:b}");
    assert!(
        shared_diamonds > 100,
        "{shared_diamonds} shared streams read twice"
    );
    let percent = |count: usize| 100.0 * count as f64 / PROGRAMS as f64;
    let [
        loops,
        cell_loops,
        state_loops,
        stream_loops,
        splits,
        defers,
        guarded,
        either,
        switch_streams,
        switch_cells,
        switch_states,
        nested,
        selectors_in_children,
        loops_closed_with_switches,
        constructs,
        nested_constructs,
        constructs_in_children,
        linear_tokens,
        navigation,
        late_switches,
        late_steps,
    ] = counts;
    let mean = sizes.iter().sum::<usize>() as f64 / sizes.len() as f64;
    eprintln!(
        "{PROGRAMS} programs, {mean:.1} definitions and {:.1} observed nodes on average, \
         {shared_diamonds} shares read twice or more; with loops {:.0}% (cell {:.0}%, state \
         {:.0}%, stream {:.0}%), splits {:.0}%, defers {:.0}%, a loop through children {:.0}%, \
         a loop or children {:.0}%; switch_streams {:.0}%, switch_cells {:.0}% (over States \
         {:.0}%), nested switches {:.0}%, a selector in child instants {:.0}%, a loop closed \
         with a switch {:.0}%; constructs {:.0}% (nested {:.0}%, in child instants {:.0}%, \
         emitting linear streams {:.0}%, the navigation loop {:.0}%), a switch_cell built in a \
         body {:.0}%, a steps_with_current built in a body {:.0}%, {} bodies of {:.1} \
         definitions on average",
        watched as f64 / PROGRAMS as f64,
        percent(loops),
        percent(cell_loops),
        percent(state_loops),
        percent(stream_loops),
        percent(splits),
        percent(defers),
        percent(guarded),
        percent(either),
        percent(switch_streams),
        percent(switch_cells),
        percent(switch_states),
        percent(nested),
        percent(selectors_in_children),
        percent(loops_closed_with_switches),
        percent(constructs),
        percent(nested_constructs),
        percent(constructs_in_children),
        percent(linear_tokens),
        percent(navigation),
        percent(late_switches),
        percent(late_steps),
        body_sizes.len(),
        body_sizes.iter().sum::<usize>() as f64 / body_sizes.len().max(1) as f64,
    );
    // Switches of every kind are in many programs.
    assert!(
        percent(switch_streams) > 30.0 && percent(switch_cells) > 30.0,
        "switch_streams in {switch_streams}, switch_cells in {switch_cells}"
    );
    assert!(
        percent(switch_states) > 10.0
            && percent(nested) > 10.0
            && percent(selectors_in_children) > 10.0
            && percent(loops_closed_with_switches) > 10.0,
        "switch_cells over States in {switch_states}, nested switches in {nested}, selectors \
         in child instants in {selectors_in_children}, loops closed with switches in \
         {loops_closed_with_switches}"
    );
    // Constructs are in most programs, of every shape.
    assert!(percent(constructs) > 50.0, "constructs in {constructs}");
    assert!(
        percent(nested_constructs) > 8.0
            && percent(constructs_in_children) > 12.0
            && percent(linear_tokens) > 20.0
            && percent(navigation) > 5.0
            && percent(late_switches) > 8.0
            && percent(late_steps) > 8.0,
        "nested constructs in {nested_constructs}, constructs in child instants in \
         {constructs_in_children}, linear streams emitted in {linear_tokens}, navigation loops \
         in {navigation}, switch_cells built in bodies in {late_switches}, steps_with_current \
         built in bodies in {late_steps}"
    );
    // Loops and children are in most programs, and the first-order and
    // cell programs of stages 1 and 2 stay in the mix.
    assert!(
        percent(loops) > 60.0 && percent(loops) < 97.0,
        "loops in {loops}"
    );
    assert!(
        percent(splits) + percent(defers) > 60.0,
        "children: {splits} and {defers}"
    );
    assert!(percent(stream_loops) > 25.0 && percent(guarded) > 25.0);
    assert!(either < PROGRAMS, "every program has a loop or children");
}

/// The reducer keeps what a failure needs and drops the rest, and every
/// program it keeps passes the builder's check and, as the one it started
/// from is, is well founded. The failure here is made up: an observed
/// merge. The program found has loops, which the reducer cuts open, and
/// switches, which it cuts away.
#[test]
fn the_reducer_keeps_what_the_failure_needs_and_drops_the_rest() {
    let mut runner = TestRunner::new(Config {
        rng_seed: RngSeed::Fixed(7),
        ..Config::default()
    });
    let strategy = programs();
    let observes_a_merge = |program: &Program| {
        program
            .observe
            .iter()
            .any(|&node| matches!(program.definitions[node], Merge { .. }))
    };
    let has =
        |program: &Program, kind: fn(&Definition) -> bool| program.definitions.iter().any(kind);
    let program = (0..1000)
        .map(|_| strategy.new_tree(&mut runner).unwrap().current())
        .find(|program| {
            observes_a_merge(program)
                && program.definitions.len() > 20
                && has(program, |d| matches!(d, CellLoop(_) | StreamLoop(_)))
                && has(program, |d| {
                    matches!(d, Definition::SwitchCell(_) | Definition::SwitchStream(_))
                })
        })
        .expect("a big program with loops and switches that observes a merge");
    let reduced = reduce(&program, |candidate| {
        assert!(check(candidate).is_ok(), "{candidate:?}");
        assert!(bough_oracle::well_founded(candidate), "{candidate:?}");
        observes_a_merge(candidate)
    });
    // Two streams and their merge, observed; no transactions; the merge's
    // function a literal; the inputs nothing reads gone.
    assert_eq!(reduced.definitions.len(), 3, "{reduced:?}");
    assert!(
        matches!(
            &reduced.definitions[2],
            Merge {
                function: Literal(0),
                ..
            }
        ),
        "{reduced:?}"
    );
    assert_eq!(reduced.observe, [2]);
    assert!(reduced.schedule.is_empty());
    assert!(reduced.inputs.len() <= 2, "{reduced:?}");
}

/// The reducer cuts a construct's body to what a failure needs, as it cuts
/// the top level. The failure here is made up: an observed node reads,
/// however indirectly, a construct whose body snapshots a cell. What is
/// left is one construct, whose body is the snapshot and at most what it
/// reads, and the few top-level nodes it and the observation need.
#[test]
fn the_reducer_cuts_a_construct_body_to_what_the_failure_needs() {
    let mut runner = TestRunner::new(Config {
        rng_seed: RngSeed::Fixed(11),
        ..Config::default()
    });
    let strategy = programs();
    // Whether an observed node reads a construct whose body snapshots.
    let fails = |program: &Program| {
        let mut read = vec![false; program.definitions.len()];
        let mut stack = program.observe.clone();
        while let Some(node) = stack.pop() {
            if std::mem::replace(&mut read[node], true) {
                continue;
            }
            stack.extend(bough_oracle::references(&program.definitions[node]));
            stack.extend(program.definitions.iter().enumerate().filter_map(
                |(close, definition)| match definition {
                    Close { forward, .. } if *forward == node => Some(close),
                    _ => None,
                },
            ));
        }
        program
            .definitions
            .iter()
            .zip(read)
            .any(|(definition, read)| {
                read && matches!(definition, Construct { body, .. }
                if body.definitions.iter().any(|d| matches!(d, Snapshot { .. })))
            })
    };
    let program = (0..2000)
        .map(|_| strategy.new_tree(&mut runner).unwrap().current())
        .find(|program| fails(program) && bodies(program).iter().any(|b| b.definitions.len() >= 4))
        .expect("a program that reads a body with a snapshot, and has a body of four definitions");
    let reduced = reduce(&program, |candidate| {
        assert!(check(candidate).is_ok(), "{candidate:?}");
        assert!(bough_oracle::well_founded(candidate), "{candidate:?}");
        fails(candidate)
    });
    let left = bodies(&reduced);
    assert_eq!(left.len(), 1, "{reduced:?}");
    assert!(left[0].definitions.len() <= 2, "{reduced:?}");
    assert!(reduced.definitions.len() <= 8, "{reduced:?}");
}
