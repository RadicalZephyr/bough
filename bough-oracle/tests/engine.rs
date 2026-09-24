//! The engine against the oracle: programs built with the Bough API, run,
//! and compared with GHC's answers event by event and step by step.
//!
//! Every program runs in both modes, `Local` and `Threaded`: plainly, under
//! three shuffle seeds, and with the sends of each transaction permuted
//! under two seeds, a coalescing input's own sends keeping their order.
//! Every run must match the oracle.
//!
//! The random programs come from `bough_oracle::programs()`, in four tests
//! that run in parallel. `PROPTEST_CASES` sets how many programs in all,
//! 512 unless set; `PROPTEST_RNG_SEED` fixes the seed, which a failure
//! prints. A failing program is shrunk by proptest, then by
//! `bough_oracle::reduce`, and reported with its schedule and every
//! observed node, the engine beside the oracle.
//!
//! A test that needs GHC starts with `let Some(oracle) = oracle() else {
//! return };`: with `BOUGH_ORACLE=skip` it says that it skipped and returns.
//! The tests of the builder and the generator alone need no GHC.

use std::cell::Cell as StdCell;
use std::collections::hash_map::RandomState;
use std::env;
use std::hash::{BuildHasher, Hasher};
use std::path::PathBuf;
use std::time::Instant;

use Definition::{
    Accumulate, AccumulateMut, CellLoop, Close, Constant, Filter, Hold, InputCell, Lift, Map,
    MapCell, MapList, Merge, Once, Share, Snapshot, Steps, StepsWithCurrent, StreamLoop,
};
use Expression::{Argument, ArgumentAt, Literal, SecondArgument};
use Reference::TopLevel;
use bough::{Local, Threaded};
use bough_oracle::{
    Definition, Engine, Expected, Expression, Input, NodeType, Oracle, Program, Reference,
    RunOptions, Scalar, Type, Value, Window, check, check_program, expected, programs, reduce, run,
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

/// The runs of every program: plain, three shuffle seeds, and two
/// permutations of the sends, one of them under a shuffle too.
fn runs(seed: u64) -> Vec<RunOptions> {
    let mix = |k: u64| seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(k);
    vec![
        RunOptions::default(),
        RunOptions {
            shuffle_seed: Some(mix(1)),
            permute_sends: None,
        },
        RunOptions {
            shuffle_seed: Some(mix(2)),
            permute_sends: None,
        },
        RunOptions {
            shuffle_seed: Some(mix(3)),
            permute_sends: None,
        },
        RunOptions {
            shuffle_seed: None,
            permute_sends: Some(mix(4)),
        },
        RunOptions {
            shuffle_seed: Some(mix(5)),
            permute_sends: Some(mix(6)),
        },
    ]
}

/// Holds the program to the oracle in every mode and run, and returns what
/// the oracle expects, for the test's own assertions.
fn agree(oracle: &Oracle, program: &Program) -> Vec<Expected> {
    if let Err(report) = check_program(oracle, program, &ENGINES, &runs(0)) {
        panic!("{report}");
    }
    let answer = oracle.answer(program).unwrap();
    expected(program, &answer).unwrap()
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

fn stream(events: &[&[i64]]) -> Expected {
    Expected::Stream {
        events: events.iter().map(|events| events.to_vec()).collect(),
    }
}

/// A cell's expectation from its initial value and its step in each
/// transaction.
fn cell(initial: i64, steps: &[Option<i64>]) -> Expected {
    let mut value = initial;
    Expected::Cell {
        initial,
        steps: steps
            .iter()
            .map(|step| step.iter().copied().collect())
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

// ----- random programs -----

/// The programs in all when `PROPTEST_CASES` is not set: about five
/// seconds on four cores.
const DEFAULT_CASES: u32 = 512;

/// The random tests split the programs between them and run in parallel.
const SHARDS: u32 = 4;

/// A seed nobody chose, for a run without `PROPTEST_RNG_SEED`.
fn fresh_seed() -> u64 {
    RandomState::new().build_hasher().finish()
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
    let mut runner = TestRunner::new(config);
    let result = runner.run(&(programs(), any::<u64>()), |(program, seed)| {
        tried.set(tried.get() + 1);
        check_program(oracle, &program, &ENGINES, &runs(seed)).map_err(|report| {
            if first_failure.get().is_none() {
                first_failure.set(Some((tried.get(), started.elapsed())));
            }
            TestCaseError::fail(report.to_string())
        })
    });
    match result {
        Ok(()) => eprintln!(
            "shard {shard}: {cases} random programs agree with the oracle, each in two modes \
             and six runs, in {:.1?} (PROPTEST_RNG_SEED={base})",
            started.elapsed()
        ),
        Err(TestError::Fail(_, (program, seed))) => {
            let (found, after) = first_failure.get().unwrap_or((0, started.elapsed()));
            let shrunk = started.elapsed();
            let runs = runs(seed);
            let reduced = reduce(&program, |p| {
                check_program(oracle, p, &ENGINES, &runs).is_err()
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
        "node 2 (SwitchCell): SwitchCell is outside the subset of stages 1 to 4"
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

/// Every generated program passes the builder's check, and across a few
/// hundred of them every definition kind of the subset occurs and every
/// materialized kind is observed.
#[test]
fn the_generator_makes_well_formed_programs_with_every_kind() {
    let mut runner = TestRunner::new(Config {
        rng_seed: RngSeed::Fixed(20260924),
        ..Config::default()
    });
    let strategy = programs();
    let mut defined = std::collections::BTreeSet::new();
    let mut observed = std::collections::BTreeSet::new();
    let (mut shared_diamonds, mut lifts, mut sizes) = (0, 0, Vec::new());
    for _ in 0..400 {
        let program = strategy.new_tree(&mut runner).unwrap().current();
        let types = check(&program).unwrap_or_else(|error| panic!("{error}\n{program:?}"));
        assert!((5..=bough_oracle::MAX_DEFINITIONS).contains(&program.definitions.len()));
        assert!((1..=10).contains(&program.schedule.len()));
        sizes.push(program.definitions.len());
        for definition in &program.definitions {
            defined.insert(bough_oracle::name(definition));
            if let Lift { cells, .. } = definition {
                lifts |= 1 << cells.len();
            }
        }
        for &node in &program.observe {
            observed.insert(bough_oracle::name(&program.definitions[node]));
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
        let _ = types;
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
    ];
    for kind in every {
        assert!(defined.contains(kind), "no {kind} in 400 programs");
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
    ] {
        assert!(
            observed.contains(kind),
            "no {kind} observed in 400 programs"
        );
    }
    // Lifts of two to six cells.
    assert_eq!(lifts, 0b111_1100, "lift arities seen: {lifts:b}");
    assert!(
        shared_diamonds > 100,
        "{shared_diamonds} shared streams read twice"
    );
    let mean = sizes.iter().sum::<usize>() as f64 / sizes.len() as f64;
    eprintln!(
        "400 programs, {mean:.1} definitions on average, {shared_diamonds} shares read twice or more"
    );
}

/// The reducer keeps what a failure needs and drops the rest, and every
/// program it keeps passes the builder's check. The failure here is made
/// up: an observed merge.
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
    let program = (0..1000)
        .map(|_| strategy.new_tree(&mut runner).unwrap().current())
        .find(|program| observes_a_merge(program) && program.definitions.len() > 20)
        .expect("a big program that observes a merge");
    let reduced = reduce(&program, |candidate| {
        assert!(check(candidate).is_ok(), "{candidate:?}");
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
