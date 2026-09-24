//! Everything that needs GHC, in one test binary: the oracle's answers
//! against the semantics' own vectors, the derived operations through the
//! interpreter, loops, the protocol and the pool, and the Haskell tests.
//!
//! Every test starts with `let Some(oracle) = oracle() else { return };`.
//! Without GHC that panics with the install hint; with `BOUGH_ORACLE=skip` it
//! says the test skipped, and the test returns.

use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::Duration;

use Definition::{
    Accumulate, AccumulateMut, CellLoop, Close, Constant, ConstantCell, ConstantStream, Construct,
    Defer, Filter, FilterMap, Gate, Hold, HoldCell, HoldStream, InputCell, Lift, Map, MapCell,
    MapList, MapPickCell, MapTo, Merge, Never, Node, Once, OrElse, PickCell, PickStream, Scan,
    Share, Snapshot, Split, Steps, StepsWithCurrent, StreamLoop, SwitchCell, SwitchStream,
    ToBoolean,
};
use Expression::{Argument, ArgumentAt, ConstructEvent, Literal, SecondArgument};
use Reference::{Local, TopLevel};
use bough_oracle::{
    Answer, Body, BodyResult, Definition, Error, Expression, Input, Observation, Oracle, Program,
    Reference, Type, Value, Window,
};

// ----- helpers -----

/// The oracle every test shares, or `None` when the tests skip.
fn oracle() -> Option<&'static Oracle> {
    bough_oracle::for_tests(directory())
}

/// Where the oracle and the Haskell tests are built.
fn directory() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("bough-oracle")
}

/// A pool of its own, for the tests that count or kill processes.
fn own_pool() -> Oracle {
    Oracle::build(directory()).unwrap_or_else(|error| panic!("{error}"))
}

/// The observations of an `OK` answer; panics on anything else.
fn observe(oracle: &Oracle, program: &Program) -> Vec<Observation> {
    match oracle.answer(program) {
        Ok(Answer::Observed(observations)) => observations,
        other => panic!("{program}\nanswered {}", brief(&other)),
    }
}

/// The message of an `ERR` answer; panics on anything else.
fn error_message(oracle: &Oracle, program: &Program) -> String {
    match oracle.answer(program) {
        Ok(Answer::Error(message)) => message,
        other => panic!("{program}\nanswered {}", brief(&other)),
    }
}

/// The start of an outcome's description: a program can be long.
fn brief(outcome: &Result<Answer, Error>) -> String {
    format!("{outcome:?}").chars().take(600).collect()
}

/// A vector's program: no inputs, no sends, everything answered.
fn vector(definitions: Vec<Definition>, observe: Vec<usize>) -> Program {
    Program {
        window: Window::Everything,
        inputs: vec![],
        definitions,
        observe,
        schedule: vec![],
    }
}

/// A program over integer inputs, answered from the first transaction. The
/// schedule's entry k - 1 holds the sends of transaction k.
fn driven(
    inputs: usize,
    definitions: Vec<Definition>,
    observe: Vec<usize>,
    schedule: &[&[(usize, i64)]],
) -> Program {
    Program {
        window: Window::FromFirstTransaction,
        inputs: vec![Input::new(Type::Integer); inputs],
        definitions,
        observe,
        schedule: schedule
            .iter()
            .map(|sends| {
                sends
                    .iter()
                    .map(|(input, value)| (*input, Value::Integer(*value)))
                    .collect()
            })
            .collect(),
    }
}

/// A literal stream of integers.
fn numbers(events: &[(&[i64], i64)]) -> Definition {
    Definition::Literal {
        event_type: Type::Integer,
        events: events
            .iter()
            .map(|(time, value)| (time.to_vec(), Value::Integer(*value)))
            .collect(),
    }
}

/// A literal stream of characters, carried as their codes.
fn letters(events: &[(&[i64], char)]) -> Definition {
    Definition::Literal {
        event_type: Type::Integer,
        events: events
            .iter()
            .map(|(time, letter)| (time.to_vec(), Value::Integer(code(*letter))))
            .collect(),
    }
}

fn code(letter: char) -> i64 {
    i64::from(u32::from(letter))
}

/// The expected events of a stream of integers.
fn stream(events: &[(&[i64], i64)]) -> Observation {
    Observation::stream(events.iter().map(|(time, value)| (time.to_vec(), *value)))
}

/// The expected value and steps of a cell of integers.
fn cell(initial: i64, steps: &[(&[i64], i64)]) -> Observation {
    Observation::cell(
        initial,
        steps.iter().map(|(time, value)| (time.to_vec(), *value)),
    )
}

fn letter_stream(events: &[(&[i64], char)]) -> Observation {
    Observation::stream(
        events
            .iter()
            .map(|(time, letter)| (time.to_vec(), code(*letter))),
    )
}

fn letter_cell(initial: char, steps: &[(&[i64], char)]) -> Observation {
    Observation::cell(
        code(initial),
        steps
            .iter()
            .map(|(time, letter)| (time.to_vec(), code(*letter))),
    )
}

// ----- the twenty vectors of sodium.hs -----

/// Events written as (time, value) pairs.
type Timed<'a, T> = &'a [(&'a [i64], T)];

/// sodium.hs's SwitchC vectors as a program: each inner cell a hold of
/// letters, a selector stream that picks inner k at its times, a hold of
/// cells starting at the first inner, and the switch.
fn switch_program(inners: &[(char, Timed<'_, char>)], switches: Timed<'_, i64>) -> Program {
    let mut definitions = Vec::new();
    for (initial, events) in inners {
        let events_node = definitions.len();
        definitions.push(letters(events));
        definitions.push(Hold {
            initial: Literal(code(*initial)),
            source: TopLevel(events_node),
        });
    }
    let selector = definitions.len();
    definitions.push(numbers(switches));
    definitions.push(PickCell {
        index: Argument,
        cells: (0..inners.len())
            .map(|inner| TopLevel(2 * inner + 1))
            .collect(),
        source: TopLevel(selector),
    });
    definitions.push(HoldCell {
        initial: TopLevel(1),
        source: TopLevel(selector + 1),
    });
    definitions.push(SwitchCell(TopLevel(selector + 2)));
    let switch = definitions.len() - 1;
    vector(definitions, vec![switch])
}

/// The twenty vectors of `haskell/sodium.hs`, restated as literal streams
/// through the interpreter: a stream of characters carries their codes, a
/// cell of functions is a lift, a stream of streams or cells picks among
/// nodes, and Execute's `return 'a'` is a construct body.
fn sodium_vectors() -> Vec<(&'static str, Program, Vec<Observation>)> {
    let c1: Timed<'_, char> = &[(&[0], 'b'), (&[1], 'c'), (&[2], 'd'), (&[3], 'e')];
    let c2: Timed<'_, char> = &[(&[0], 'W'), (&[1], 'X'), (&[2], 'Y'), (&[3], 'Z')];
    vec![
        (
            "Never",
            vector(vec![Never(Type::Integer)], vec![0]),
            vec![stream(&[])],
        ),
        (
            "MapS",
            vector(
                vec![
                    numbers(&[(&[0], 5), (&[1], 10), (&[2], 12)]),
                    Map {
                        function: Literal(1) + Argument,
                        source: TopLevel(0),
                    },
                ],
                vec![1],
            ),
            vec![stream(&[(&[0], 6), (&[1], 11), (&[2], 13)])],
        ),
        (
            "Snapshot",
            vector(
                vec![
                    numbers(&[(&[1], 4), (&[5], 7)]),
                    Hold {
                        initial: Literal(3),
                        source: TopLevel(0),
                    },
                    letters(&[(&[0], 'a'), (&[3], 'b'), (&[5], 'c')]),
                    Snapshot {
                        function: SecondArgument,
                        source: TopLevel(2),
                        cell: TopLevel(1),
                    },
                    // snapshot2: MapS, sample and Execute.
                    Construct {
                        body: Body {
                            definitions: vec![],
                            result: BodyResult::Value(Expression::Sample(TopLevel(1))),
                        },
                        source: TopLevel(2),
                    },
                ],
                vec![3, 4],
            ),
            vec![
                stream(&[(&[0], 3), (&[3], 4), (&[5], 4)]),
                stream(&[(&[0], 3), (&[3], 4), (&[5], 4)]),
            ],
        ),
        (
            "Merge",
            vector(
                vec![
                    numbers(&[(&[0], 0), (&[2], 2)]),
                    numbers(&[(&[1], 10), (&[2], 20), (&[3], 30)]),
                    Merge {
                        function: Argument + SecondArgument,
                        left: TopLevel(0),
                        right: TopLevel(1),
                    },
                ],
                vec![2],
            ),
            vec![stream(&[(&[0], 0), (&[1], 10), (&[2], 22), (&[3], 30)])],
        ),
        (
            "Filter",
            vector(
                vec![
                    numbers(&[(&[0], 5), (&[1], 6), (&[2], 7)]),
                    Filter {
                        predicate: Argument.modulo(2),
                        source: TopLevel(0),
                    },
                ],
                vec![1],
            ),
            vec![stream(&[(&[0], 5), (&[2], 7)])],
        ),
        (
            "SwitchS",
            vector(
                vec![
                    letters(&[(&[0], 'a'), (&[1], 'b'), (&[2], 'c'), (&[3], 'd')]),
                    letters(c2),
                    numbers(&[(&[1], 1)]),
                    PickStream {
                        index: Argument,
                        streams: vec![TopLevel(0), TopLevel(1)],
                        source: TopLevel(2),
                    },
                    HoldStream {
                        initial: TopLevel(0),
                        source: TopLevel(3),
                    },
                    SwitchStream(TopLevel(4)),
                ],
                vec![5],
            ),
            vec![letter_stream(&[
                (&[0], 'a'),
                (&[1], 'b'),
                (&[2], 'Y'),
                (&[3], 'Z'),
            ])],
        ),
        (
            "Execute",
            vector(
                vec![
                    numbers(&[(&[0], 0)]),
                    Construct {
                        body: Body {
                            definitions: vec![],
                            result: BodyResult::Value(Literal(code('a'))),
                        },
                        source: TopLevel(0),
                    },
                ],
                vec![1],
            ),
            vec![letter_stream(&[(&[0], 'a')])],
        ),
        (
            "Updates",
            vector(
                vec![
                    letters(&[(&[1], 'b'), (&[3], 'c')]),
                    Hold {
                        initial: Literal(code('a')),
                        source: TopLevel(0),
                    },
                    Steps(TopLevel(1)),
                ],
                vec![2],
            ),
            vec![letter_stream(&[(&[1], 'b'), (&[3], 'c')])],
        ),
        (
            "Value 1",
            vector(
                vec![
                    letters(&[(&[1], 'b'), (&[3], 'c')]),
                    Hold {
                        initial: Literal(code('a')),
                        source: TopLevel(0),
                    },
                    StepsWithCurrent(TopLevel(1)),
                ],
                vec![2],
            ),
            vec![letter_stream(&[(&[0], 'a'), (&[1], 'b'), (&[3], 'c')])],
        ),
        (
            "Value 2",
            vector(
                vec![
                    letters(&[(&[0], 'b'), (&[1], 'c'), (&[3], 'd')]),
                    Hold {
                        initial: Literal(code('a')),
                        source: TopLevel(0),
                    },
                    StepsWithCurrent(TopLevel(1)),
                ],
                vec![2],
            ),
            vec![letter_stream(&[(&[0], 'b'), (&[1], 'c'), (&[3], 'd')])],
        ),
        (
            "Split",
            vector(
                vec![
                    Definition::Literal {
                        event_type: Type::List,
                        events: vec![
                            (vec![0], Value::List(vec![code('a'), code('b')])),
                            (vec![1], Value::List(vec![code('c')])),
                        ],
                    },
                    Split(TopLevel(0)),
                ],
                vec![1],
            ),
            vec![letter_stream(&[
                (&[0, 0], 'a'),
                (&[0, 1], 'b'),
                (&[1, 0], 'c'),
            ])],
        ),
        (
            "Constant",
            vector(vec![Constant(Literal(code('a')))], vec![0]),
            vec![letter_cell('a', &[])],
        ),
        (
            "Hold",
            vector(
                vec![
                    letters(&[(&[1], 'b'), (&[3], 'c')]),
                    Hold {
                        initial: Literal(code('a')),
                        source: TopLevel(0),
                    },
                ],
                vec![1],
            ),
            vec![letter_cell('a', &[(&[1], 'b'), (&[3], 'c')])],
        ),
        (
            "MapC",
            vector(
                vec![
                    numbers(&[(&[2], 3), (&[3], 5)]),
                    Hold {
                        initial: Literal(0),
                        source: TopLevel(0),
                    },
                    MapCell {
                        function: Literal(1) + Argument,
                        cell: TopLevel(1),
                    },
                ],
                vec![2],
            ),
            vec![cell(1, &[(&[2], 4), (&[3], 6)])],
        ),
        (
            // A cell of functions (5+) and (6+) is a cell of the addends.
            "Apply",
            vector(
                vec![
                    numbers(&[(&[1], 5), (&[3], 6)]),
                    Hold {
                        initial: Literal(0),
                        source: TopLevel(0),
                    },
                    numbers(&[(&[1], 200), (&[2], 300), (&[4], 400)]),
                    Hold {
                        initial: Literal(100),
                        source: TopLevel(2),
                    },
                    Lift {
                        function: ArgumentAt(0) + ArgumentAt(1),
                        cells: vec![TopLevel(1), TopLevel(3)],
                    },
                ],
                vec![4],
            ),
            vec![cell(
                100,
                &[(&[1], 205), (&[2], 305), (&[3], 306), (&[4], 406)],
            )],
        ),
        (
            "SwitchC 1",
            switch_program(&[('a', c1), ('V', c2)], &[(&[1], 1)]),
            vec![letter_cell(
                'a',
                &[(&[0], 'b'), (&[1], 'X'), (&[2], 'Y'), (&[3], 'Z')],
            )],
        ),
        (
            "SwitchC 2",
            switch_program(
                &[('a', c1), ('W', &[(&[1], 'X'), (&[2], 'Y'), (&[3], 'Z')])],
                &[(&[1], 1)],
            ),
            vec![letter_cell(
                'a',
                &[(&[0], 'b'), (&[1], 'X'), (&[2], 'Y'), (&[3], 'Z')],
            )],
        ),
        (
            "SwitchC 3",
            switch_program(
                &[('a', c1), ('X', &[(&[2], 'Y'), (&[3], 'Z')])],
                &[(&[1], 1)],
            ),
            vec![letter_cell(
                'a',
                &[(&[0], 'b'), (&[1], 'X'), (&[2], 'Y'), (&[3], 'Z')],
            )],
        ),
        (
            "SwitchC 4",
            switch_program(
                &[
                    ('a', c1),
                    ('V', c2),
                    ('1', &[(&[0], '2'), (&[1], '3'), (&[2], '4'), (&[3], '5')]),
                ],
                &[(&[1], 1), (&[3], 2)],
            ),
            vec![letter_cell(
                'a',
                &[(&[0], 'b'), (&[1], 'X'), (&[2], 'Y'), (&[3], '5')],
            )],
        ),
        (
            // run (sample c) [1] and [2]: a construct samples at its event's
            // instant.
            "Sample",
            vector(
                vec![
                    letters(&[(&[1], 'b')]),
                    Hold {
                        initial: Literal(code('a')),
                        source: TopLevel(0),
                    },
                    numbers(&[(&[1], 0), (&[2], 0)]),
                    Construct {
                        body: Body {
                            definitions: vec![],
                            result: BodyResult::Value(Expression::Sample(TopLevel(1))),
                        },
                        source: TopLevel(2),
                    },
                ],
                vec![3],
            ),
            vec![letter_stream(&[(&[1], 'a'), (&[2], 'b')])],
        ),
    ]
}

#[test]
fn the_twenty_vectors_of_sodium_hs() {
    let Some(oracle) = oracle() else { return };
    let vectors = sodium_vectors();
    assert_eq!(vectors.len(), 20);
    for (name, program, expected) in vectors {
        assert_eq!(observe(oracle, &program), expected, "{name}");
    }
}

#[test]
fn the_five_common_tests_vectors_at_the_times_of_denotational_hs() {
    let Some(oracle) = oracle() else { return };
    // SemanticTests.hs puts both split children at [0,0]; Denotational.hs
    // gives [0,0] and [0,1], and the common tests compare per top-level
    // instant only.
    let vectors = [
        (
            "split",
            vector(
                vec![
                    Definition::Literal {
                        event_type: Type::List,
                        events: vec![(vec![0], Value::List(vec![code('a'), code('b')]))],
                    },
                    Split(TopLevel(0)),
                ],
                vec![1],
            ),
            letter_stream(&[(&[0, 0], 'a'), (&[0, 1], 'b')]),
        ),
        (
            "defer1",
            vector(
                vec![letters(&[(&[0], 'a'), (&[1], 'b')]), Defer(TopLevel(0))],
                vec![1],
            ),
            letter_stream(&[(&[0, 0], 'a'), (&[1, 0], 'b')]),
        ),
        (
            "defer2",
            vector(
                vec![
                    letters(&[(&[0], 'a'), (&[1], 'b')]),
                    letters(&[(&[1], 'B')]),
                    Defer(TopLevel(0)),
                    OrElse {
                        left: TopLevel(2),
                        right: TopLevel(1),
                    },
                ],
                vec![3],
            ),
            letter_stream(&[(&[0, 0], 'a'), (&[1], 'B'), (&[1, 0], 'b')]),
        ),
        (
            "orElse1",
            vector(
                vec![
                    numbers(&[(&[0], 0), (&[2], 2)]),
                    numbers(&[(&[1], 10), (&[2], 20), (&[3], 30)]),
                    OrElse {
                        left: TopLevel(0),
                        right: TopLevel(1),
                    },
                ],
                vec![2],
            ),
            stream(&[(&[0], 0), (&[1], 10), (&[2], 2), (&[3], 30)]),
        ),
        (
            "deferSimultaneous",
            vector(
                vec![
                    letters(&[(&[1], 'b')]),
                    letters(&[(&[0], 'A'), (&[1], 'B')]),
                    Defer(TopLevel(0)),
                    Defer(TopLevel(1)),
                    OrElse {
                        left: TopLevel(2),
                        right: TopLevel(3),
                    },
                ],
                vec![4],
            ),
            letter_stream(&[(&[0, 0], 'A'), (&[1, 0], 'b')]),
        ),
    ];
    for (name, program, expected) in vectors {
        assert_eq!(observe(oracle, &program), vec![expected], "{name}");
    }
}

// ----- the derived operations -----

/// A construct at `[2]` whose body emits a cell, held into a switch cell
/// built at `[0]`, so the answer shows the body's cell from `[2]` on. Input 0
/// carries 97, 98 and 99 at `[1]`, `[2]` and `[3]`; input 1 fires once, at
/// `[2]`. The switch cell starts at -2.
fn constructed_at_2(body: Vec<Definition>, result: Reference) -> Program {
    driven(
        2,
        vec![
            Definition::Input(0),
            Definition::Input(1),
            Construct {
                body: Body {
                    definitions: body,
                    result: BodyResult::Node(result),
                },
                source: TopLevel(1),
            },
            Constant(Literal(-2)),
            HoldCell {
                initial: TopLevel(3),
                source: TopLevel(2),
            },
            SwitchCell(TopLevel(4)),
        ],
        vec![5],
        &[&[(0, 97)], &[(0, 98), (1, 0)], &[(0, 99)]],
    )
}

#[test]
fn scan_created_at_2_starts_its_state_at_2() {
    let Some(oracle) = oracle() else { return };
    // Output event * 100 + state; the state counts events from [2].
    let program = constructed_at_2(
        vec![
            Scan {
                initial: Literal(0),
                output: Argument * Literal(100) + SecondArgument,
                state: SecondArgument + Literal(1),
                source: TopLevel(0),
            },
            Hold {
                initial: Literal(-1),
                source: Local(0),
            },
        ],
        Local(1),
    );
    assert_eq!(
        observe(oracle, &program),
        vec![cell(-2, &[(&[2], 9800), (&[3], 9901)])]
    );
}

#[test]
fn once_created_at_2_sees_the_event_at_2() {
    let Some(oracle) = oracle() else { return };
    let program = constructed_at_2(
        vec![
            Once(TopLevel(0)),
            Hold {
                initial: Literal(-1),
                source: Local(0),
            },
        ],
        Local(1),
    );
    assert_eq!(observe(oracle, &program), vec![cell(-2, &[(&[2], 98)])]);
}

#[test]
fn accumulate_created_at_2_takes_the_event_at_2() {
    let Some(oracle) = oracle() else { return };
    let program = constructed_at_2(
        vec![Accumulate {
            initial: Literal(0),
            function: Argument + SecondArgument,
            source: TopLevel(0),
        }],
        Local(0),
    );
    assert_eq!(
        observe(oracle, &program),
        vec![cell(-2, &[(&[2], 98), (&[3], 197)])]
    );
}

#[test]
fn a_cell_loop_declared_in_a_construct_at_2_counts_from_2() {
    let Some(oracle) = oracle() else { return };
    let program = constructed_at_2(
        vec![
            CellLoop(Type::Integer),
            Snapshot {
                function: SecondArgument + Literal(1),
                source: TopLevel(0),
                cell: Local(0),
            },
            Hold {
                initial: Literal(0),
                source: Local(1),
            },
            Close {
                forward: 0,
                definition: Local(2),
            },
        ],
        Local(2),
    );
    assert_eq!(
        observe(oracle, &program),
        vec![cell(-2, &[(&[2], 1), (&[3], 2)])]
    );
}

#[test]
fn a_stream_loop_declared_in_a_construct_at_2_counts_from_2() {
    let Some(oracle) = oracle() else { return };
    // scan by a stream loop: the states, held by a hold of their own.
    let program = constructed_at_2(
        vec![
            StreamLoop(Type::Integer),
            Hold {
                initial: Literal(0),
                source: Local(0),
            },
            Snapshot {
                function: SecondArgument + Literal(1),
                source: TopLevel(0),
                cell: Local(1),
            },
            Close {
                forward: 0,
                definition: Local(2),
            },
            Hold {
                initial: Literal(-1),
                source: Local(2),
            },
        ],
        Local(4),
    );
    assert_eq!(
        observe(oracle, &program),
        vec![cell(-2, &[(&[2], 1), (&[3], 2)])]
    );
}

#[test]
fn sample_in_a_construct_reads_the_value_before_the_instant() {
    let Some(oracle) = oracle() else { return };
    let program = driven(
        2,
        vec![
            InputCell {
                input: 0,
                initial: Literal(0),
            },
            Definition::Input(1),
            Construct {
                body: Body {
                    definitions: vec![],
                    result: BodyResult::Value(Expression::Sample(TopLevel(0)) + ConstructEvent),
                },
                source: TopLevel(1),
            },
        ],
        vec![2],
        &[&[], &[(0, 5), (1, 100)], &[(1, 200)]],
    );
    assert_eq!(
        observe(oracle, &program),
        vec![stream(&[(&[2], 100), (&[3], 205)])]
    );
}

#[test]
fn gate_reads_the_cell_before_the_instant() {
    let Some(oracle) = oracle() else { return };
    let program = driven(
        2,
        vec![
            Definition::Input(0),
            InputCell {
                input: 1,
                initial: Literal(1),
            },
            ToBoolean(TopLevel(1)),
            Gate {
                source: TopLevel(0),
                cell: TopLevel(2),
            },
        ],
        vec![3],
        &[&[(0, 1)], &[(0, 2), (1, 0)], &[(0, 3), (1, 1)], &[(0, 4)]],
    );
    assert_eq!(
        observe(oracle, &program),
        vec![stream(&[(&[1], 1), (&[2], 2), (&[4], 4)])]
    );
}

#[test]
fn filter_map_keeps_and_maps() {
    let Some(oracle) = oracle() else { return };
    let program = driven(
        1,
        vec![
            Definition::Input(0),
            FilterMap {
                keep: Argument.modulo(2).equal(Literal(0)),
                function: Argument * Literal(10),
                source: TopLevel(0),
            },
        ],
        vec![1],
        &[&[(0, 1)], &[(0, 2)], &[(0, 3)], &[(0, 4)]],
    );
    assert_eq!(
        observe(oracle, &program),
        vec![stream(&[(&[2], 20), (&[4], 40)])]
    );
}

#[test]
fn map_to_replaces_each_event_with_a_boolean_or_a_list() {
    let Some(oracle) = oracle() else { return };
    let program = driven(
        1,
        vec![
            Definition::Input(0),
            MapTo {
                value: Value::Boolean(true),
                source: TopLevel(0),
            },
            MapTo {
                value: Value::List(vec![1, -2]),
                source: TopLevel(0),
            },
        ],
        vec![1, 2],
        &[&[(0, 7)], &[], &[(0, 8)]],
    );
    assert_eq!(
        observe(oracle, &program),
        vec![
            Observation::stream([(vec![1], true), (vec![3], true)]),
            Observation::stream([(vec![1], vec![1, -2]), (vec![3], vec![1, -2])]),
        ]
    );
}

#[test]
fn lift_of_three_steps_once_when_two_inputs_step_together() {
    let Some(oracle) = oracle() else { return };
    let program = driven(
        3,
        vec![
            InputCell {
                input: 0,
                initial: Literal(1),
            },
            InputCell {
                input: 1,
                initial: Literal(10),
            },
            InputCell {
                input: 2,
                initial: Literal(100),
            },
            Lift {
                function: ArgumentAt(0) * Literal(1_000_000)
                    + ArgumentAt(1) * Literal(1000)
                    + ArgumentAt(2),
                cells: vec![TopLevel(0), TopLevel(1), TopLevel(2)],
            },
        ],
        vec![3],
        &[&[(0, 2)], &[(0, 3), (1, 20)], &[(2, 300)]],
    );
    assert_eq!(
        observe(oracle, &program),
        vec![cell(
            1_010_100,
            &[(&[1], 2_010_100), (&[2], 3_020_100), (&[3], 3_020_300)]
        )]
    );
}

#[test]
fn lift_of_six() {
    let Some(oracle) = oracle() else { return };
    // Cell n starts at n and steps to 2n at [1]; the lift reads them as the
    // digits of a number in base 100, cell 0 first.
    let mut definitions: Vec<Definition> = (0..6)
        .map(|input| InputCell {
            input,
            initial: Literal(i64::try_from(input).unwrap() + 1),
        })
        .collect();
    let digits = (1..6).fold(ArgumentAt(0), |number, index| {
        number * Literal(100) + ArgumentAt(index)
    });
    definitions.push(Lift {
        function: digits,
        cells: (0..6).map(TopLevel).collect(),
    });
    let sends: Vec<(usize, i64)> = (0..6)
        .map(|input| (input, 2 * (i64::try_from(input).unwrap() + 1)))
        .collect();
    let program = driven(6, definitions, vec![6], &[&sends]);
    assert_eq!(
        observe(oracle, &program),
        vec![cell(10_203_040_506, &[(&[1], 20_406_081_012)])]
    );
}

#[test]
fn a_coalescing_input_folds_left_before_any_hold() {
    let Some(oracle) = oracle() else { return };
    let subtract = Program {
        window: Window::FromFirstTransaction,
        inputs: vec![Input::coalescing(Type::Integer, Argument - SecondArgument)],
        definitions: vec![
            Definition::Input(0),
            InputCell {
                input: 0,
                initial: Literal(0),
            },
        ],
        observe: vec![0, 1],
        schedule: vec![
            vec![
                (0, Value::Integer(1)),
                (0, Value::Integer(2)),
                (0, Value::Integer(3)),
            ],
            vec![(0, Value::Integer(10))],
            vec![],
            vec![(0, Value::Integer(7)), (0, Value::Integer(1))],
        ],
    };
    // f (f 1 2) 3: first send on the left.
    assert_eq!(
        observe(oracle, &subtract),
        vec![
            stream(&[(&[1], (1 - 2) - 3), (&[2], 10), (&[4], 6)]),
            cell(0, &[(&[1], (1 - 2) - 3), (&[2], 10), (&[4], 6)]),
        ]
    );
}

#[test]
fn steps_with_current_fires_at_its_creation() {
    let Some(oracle) = oracle() else { return };
    // In the build: at [0], and a defer of it at [0,0], a child of
    // transaction zero.
    let built = vector(
        vec![
            Constant(Literal(7)),
            StepsWithCurrent(TopLevel(0)),
            Hold {
                initial: Literal(0),
                source: TopLevel(1),
            },
            Defer(TopLevel(1)),
        ],
        vec![1, 2, 3],
    );
    assert_eq!(
        observe(oracle, &built),
        vec![
            stream(&[(&[0], 7)]),
            cell(0, &[(&[0], 7)]),
            stream(&[(&[0, 0], 7)]),
        ]
    );
    // In a construct at [2], after the cell stepped at [1]: one event at
    // [2] with the current value, then the cell's steps. A hold built with
    // it takes the event at [2].
    let constructed = driven(
        2,
        vec![
            InputCell {
                input: 0,
                initial: Literal(0),
            },
            Definition::Input(1),
            Construct {
                body: Body {
                    definitions: vec![
                        StepsWithCurrent(TopLevel(0)),
                        Hold {
                            initial: Literal(-1),
                            source: Local(0),
                        },
                    ],
                    result: BodyResult::Node(Local(1)),
                },
                source: TopLevel(1),
            },
            Constant(Literal(-2)),
            HoldCell {
                initial: TopLevel(3),
                source: TopLevel(2),
            },
            SwitchCell(TopLevel(4)),
        ],
        vec![5],
        &[&[(0, 5)], &[(1, 0)], &[(0, 6)]],
    );
    assert_eq!(
        observe(oracle, &constructed),
        vec![cell(-2, &[(&[2], 5), (&[3], 6)])]
    );
}

#[test]
fn defer_moves_each_event_to_a_child_instant_that_it_shares_with_split() {
    let Some(oracle) = oracle() else { return };
    let program = Program {
        window: Window::FromFirstTransaction,
        inputs: vec![Input::new(Type::List), Input::new(Type::Integer)],
        definitions: vec![
            Definition::Input(0),
            Definition::Input(1),
            Split(TopLevel(0)),
            Defer(TopLevel(1)),
            Merge {
                function: Argument + SecondArgument,
                left: TopLevel(2),
                right: TopLevel(3),
            },
        ],
        observe: vec![3, 4],
        schedule: vec![
            vec![(0, Value::List(vec![1, 2])), (1, Value::Integer(100))],
            vec![(1, Value::Integer(200))],
        ],
    };
    assert_eq!(
        observe(oracle, &program),
        vec![
            stream(&[(&[1, 0], 100), (&[2, 0], 200)]),
            stream(&[(&[1, 0], 101), (&[1, 1], 2), (&[2, 0], 200)]),
        ]
    );
}

#[test]
fn a_split_fed_by_its_own_children_answers_in_time_order_patch_f7() {
    let Some(oracle) = oracle() else { return };
    // The fidelity review's program: without F7 the items come out of time
    // order and the merge sees 100 and 10 at [0,0,0] as two events.
    let program = vector(
        vec![
            Definition::Literal {
                event_type: Type::List,
                events: vec![(vec![0], Value::List(vec![1, 2]))],
            },
            StreamLoop(Type::List),
            Split(TopLevel(1)),
            Filter {
                predicate: Argument.less_than(Literal(10)),
                source: TopLevel(2),
            },
            MapList {
                length: Literal(2),
                element: Argument * Literal(10) + SecondArgument,
                source: TopLevel(3),
            },
            OrElse {
                left: TopLevel(0),
                right: TopLevel(4),
            },
            Close {
                forward: 1,
                definition: TopLevel(5),
            },
            Split(TopLevel(5)),
            numbers(&[(&[0, 0, 0], 100)]),
            Merge {
                function: Argument + SecondArgument,
                left: TopLevel(7),
                right: TopLevel(8),
            },
        ],
        vec![7, 9],
    );
    assert_eq!(
        observe(oracle, &program),
        vec![
            stream(&[
                (&[0, 0], 1),
                (&[0, 0, 0], 10),
                (&[0, 0, 1], 11),
                (&[0, 1], 2),
                (&[0, 1, 0], 20),
                (&[0, 1, 1], 21),
            ]),
            stream(&[
                (&[0, 0], 1),
                (&[0, 0, 0], 110),
                (&[0, 0, 1], 11),
                (&[0, 1], 2),
                (&[0, 1, 0], 20),
                (&[0, 1, 1], 21),
            ]),
        ]
    );
}

#[test]
fn switch_cell_created_after_its_outer_switched_starts_from_the_new_inner_patch_f6() {
    let Some(oracle) = oracle() else { return };
    // The outer switches from 'a' to 'x' at [1]. A construct at [3] creates
    // a switch cell over it and holds its steps: the text's SwitchC steps to
    // the deselected 'a' at [3], so without F6 the hold would take 'a'.
    let program = driven(
        1,
        vec![
            Definition::Input(0),
            Constant(Literal(code('a'))),
            Constant(Literal(code('x'))),
            PickCell {
                index: Argument,
                cells: vec![TopLevel(1), TopLevel(2)],
                source: TopLevel(0),
            },
            HoldCell {
                initial: TopLevel(1),
                source: TopLevel(3),
            },
            numbers(&[(&[3], 0)]),
            Construct {
                body: Body {
                    definitions: vec![
                        SwitchCell(TopLevel(4)),
                        Steps(Local(0)),
                        Hold {
                            initial: Literal(0),
                            source: Local(1),
                        },
                    ],
                    result: BodyResult::Node(Local(2)),
                },
                source: TopLevel(5),
            },
            Constant(Literal(-2)),
            HoldCell {
                initial: TopLevel(7),
                source: TopLevel(6),
            },
            SwitchCell(TopLevel(8)),
        ],
        vec![9],
        &[&[(0, 1)], &[], &[]],
    );
    assert_eq!(
        observe(oracle, &program),
        vec![cell(-2, &[(&[3], code('x'))])]
    );
}

#[test]
fn switch_stream_uses_the_old_stream_at_the_switch_instant() {
    let Some(oracle) = oracle() else { return };
    let program = driven(
        3,
        vec![
            Definition::Input(0),
            Definition::Input(1),
            Definition::Input(2),
            PickStream {
                index: Argument,
                streams: vec![TopLevel(0), TopLevel(1)],
                source: TopLevel(2),
            },
            HoldStream {
                initial: TopLevel(0),
                source: TopLevel(3),
            },
            SwitchStream(TopLevel(4)),
        ],
        vec![5],
        &[
            &[(0, 1), (1, 10)],
            &[(0, 2), (1, 20), (2, 1)],
            &[(0, 3), (1, 30)],
        ],
    );
    assert_eq!(
        observe(oracle, &program),
        vec![stream(&[(&[1], 1), (&[2], 2), (&[3], 30)])]
    );
}

#[test]
fn switch_cell_steps_at_every_switch_with_the_new_cells_value() {
    let Some(oracle) = oracle() else { return };
    // The selector picks the second cell at [2], as it steps, and again at
    // [3], where it is quiet: a step each time.
    let program = driven(
        3,
        vec![
            InputCell {
                input: 0,
                initial: Literal(1),
            },
            InputCell {
                input: 1,
                initial: Literal(10),
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
            Steps(TopLevel(5)),
        ],
        vec![5, 6],
        &[&[(0, 2)], &[(1, 20), (2, 1)], &[(2, 1)]],
    );
    assert_eq!(
        observe(oracle, &program),
        vec![
            cell(1, &[(&[1], 2), (&[2], 20), (&[3], 20)]),
            stream(&[(&[1], 2), (&[2], 20), (&[3], 20)]),
        ]
    );
}

#[test]
fn constant_tokens_and_a_cell_of_cells_picked_by_a_cell() {
    let Some(oracle) = oracle() else { return };
    let program = driven(
        2,
        vec![
            Definition::Input(0),
            ConstantStream(TopLevel(0)),
            SwitchStream(TopLevel(1)),
            InputCell {
                input: 1,
                initial: Literal(0),
            },
            Constant(Literal(10)),
            Constant(Literal(20)),
            MapPickCell {
                index: Argument,
                cells: vec![TopLevel(4), TopLevel(5)],
                cell: TopLevel(3),
            },
            SwitchCell(TopLevel(6)),
            ConstantCell(TopLevel(5)),
            SwitchCell(TopLevel(8)),
        ],
        vec![2, 7, 9],
        &[&[(0, 5)], &[(1, 1)], &[(0, 6), (1, 0)]],
    );
    assert_eq!(
        observe(oracle, &program),
        vec![
            stream(&[(&[1], 5), (&[3], 6)]),
            cell(10, &[(&[2], 20), (&[3], 10)]),
            cell(20, &[]),
        ]
    );
}

#[test]
fn node_share_merge_or_else_and_accumulate_mut_denote_what_they_stand_for() {
    let Some(oracle) = oracle() else { return };
    let program = driven(
        2,
        vec![
            Definition::Input(0),
            Node(TopLevel(0)),
            Share(TopLevel(1)),
            Definition::Input(1),
            Merge {
                function: Argument - SecondArgument,
                left: TopLevel(2),
                right: TopLevel(3),
            },
            OrElse {
                left: TopLevel(2),
                right: TopLevel(3),
            },
            Accumulate {
                initial: Literal(0),
                function: Argument + SecondArgument,
                source: TopLevel(2),
            },
            AccumulateMut {
                initial: Literal(0),
                function: Argument + SecondArgument,
                source: TopLevel(2),
            },
        ],
        vec![2, 4, 5, 6, 7],
        &[&[(0, 20)], &[(0, 20), (1, 3)], &[(1, 4)]],
    );
    assert_eq!(
        observe(oracle, &program),
        vec![
            stream(&[(&[1], 20), (&[2], 20)]),
            // f(left, right) when both fire.
            stream(&[(&[1], 20), (&[2], 17), (&[3], 4)]),
            stream(&[(&[1], 20), (&[2], 20), (&[3], 4)]),
            cell(0, &[(&[1], 20), (&[2], 40)]),
            cell(0, &[(&[1], 20), (&[2], 40)]),
        ]
    );
}

#[test]
fn never_answers_no_events() {
    let Some(oracle) = oracle() else { return };
    let program = driven(
        0,
        vec![
            Never(Type::Boolean),
            Hold {
                initial: Literal(1),
                source: TopLevel(0),
            },
        ],
        vec![0, 1],
        &[&[], &[]],
    );
    assert_eq!(
        observe(oracle, &program),
        vec![
            stream(&[]),
            Observation::cell(true, Vec::<(Vec<i64>, i64)>::new())
        ]
    );
}

// ----- loops -----

/// The uncapped counter over `ticks` transactions: a hold of a snapshot of
/// itself, plus one at each tick.
fn counter(ticks: usize, cap: Option<i64>) -> Program {
    let mut definitions = vec![
        Definition::Input(0),
        CellLoop(Type::Integer),
        Snapshot {
            function: SecondArgument + Literal(1),
            source: TopLevel(0),
            cell: TopLevel(1),
        },
    ];
    if let Some(cap) = cap {
        definitions.push(Filter {
            predicate: Argument.less_than(Literal(cap + 1)),
            source: TopLevel(2),
        });
    }
    let feed = definitions.len() - 1;
    definitions.push(Hold {
        initial: Literal(0),
        source: TopLevel(feed),
    });
    let hold = definitions.len() - 1;
    definitions.push(Close {
        forward: 1,
        definition: TopLevel(hold),
    });
    let schedule = vec![&[(0_usize, 0_i64)][..]; ticks];
    driven(1, definitions, vec![hold], &schedule)
}

#[test]
fn the_capped_counter_a_filter_inside_a_loop_through_a_hold() {
    let Some(oracle) = oracle() else { return };
    let expected: Vec<(Vec<i64>, i64)> = (1..=10).map(|k| (vec![k], k)).collect();
    assert_eq!(
        observe(oracle, &counter(15, Some(10))),
        vec![Observation::cell(0, expected)]
    );
}

#[test]
fn the_uncapped_counter() {
    let Some(oracle) = oracle() else { return };
    let expected: Vec<(Vec<i64>, i64)> = (1..=8).map(|k| (vec![k], k)).collect();
    assert_eq!(
        observe(oracle, &counter(8, None)),
        vec![Observation::cell(0, expected)]
    );
}

#[test]
fn a_loop_whose_answer_chains_through_250_instants_converges() {
    let Some(oracle) = oracle() else { return };
    // One round per tick, 251 rounds: more than the 200 that once were all
    // any loop had.
    let expected: Vec<(Vec<i64>, i64)> = (1..=250).map(|k| (vec![k], k)).collect();
    assert_eq!(
        observe(oracle, &counter(250, None)),
        vec![Observation::cell(0, expected)]
    );
}

#[test]
fn a_stream_loop_through_defer_counts_down_in_child_instants() {
    let Some(oracle) = oracle() else { return };
    let program = vector(
        vec![
            numbers(&[(&[1], 3), (&[2], 1)]),
            StreamLoop(Type::Integer),
            Defer(TopLevel(1)),
            Map {
                function: Argument - Literal(1),
                source: TopLevel(2),
            },
            Filter {
                predicate: Literal(0).less_than(Argument),
                source: TopLevel(3),
            },
            OrElse {
                left: TopLevel(0),
                right: TopLevel(4),
            },
            Close {
                forward: 1,
                definition: TopLevel(5),
            },
        ],
        vec![5],
    );
    assert_eq!(
        observe(oracle, &program),
        vec![stream(&[
            (&[1], 3),
            (&[1, 0], 2),
            (&[1, 0, 0], 1),
            (&[2], 1)
        ])]
    );
}

#[test]
fn a_loop_that_does_not_settle_answers_err() {
    let Some(oracle) = oracle() else { return };
    // A same-instant cycle through a hold's steps view, which the text
    // diverges on and the engine must refuse (finding F3): at [1], x = 0 +
    // (x + 1). The iteration counts up at [1] and stays small, so the rounds
    // allowed, 200 and two for its loop and its step, run out.
    let program = driven(
        1,
        vec![
            Definition::Input(0),
            StreamLoop(Type::Integer),
            Hold {
                initial: Literal(0),
                source: TopLevel(1),
            },
            Steps(TopLevel(2)),
            Map {
                function: Argument + Literal(1),
                source: TopLevel(3),
            },
            Merge {
                function: Argument + SecondArgument,
                left: TopLevel(0),
                right: TopLevel(4),
            },
            Close {
                forward: 1,
                definition: TopLevel(5),
            },
        ],
        vec![5],
        &[&[(0, 0)]],
    );
    assert_eq!(
        error_message(oracle, &program),
        "the loops did not converge in 204 rounds; still changing: node 1 at [1]"
    );
}

/// Which update drives health: the program of sodium-rust#52, and two of
/// the reductions the issue reports (research-loop-shapes, shape 3).
#[derive(Clone, Copy, Debug)]
enum Drive {
    Full,
    NoMaxRead,
    NoMerge,
}

/// The health-and-shield slice (research-loop-shapes, shapes 1 and 2b):
/// three cell loops, max_health, shield and health, over the inputs heal
/// (0), damage (1) and level_up (2). snapshot3 is two snapshots, the first
/// passing on the delta plus the current health, which is all the clamp
/// needs. The lifts of #52's rows follow: the fraction, as health * 1000 +
/// max_health because expressions have no division, and effective. The
/// answer is max_health, shield, took, delta, health, then the lifts.
fn slice(drive: Drive, fraction: bool, effective: bool, schedule: &[&[(usize, i64)]]) -> Program {
    let zero = || Literal(0);
    let mut definitions = vec![
        Definition::Input(0),
        Definition::Input(1),
        Definition::Input(2),
        Share(TopLevel(1)),
        // max_health = level_up.snapshot(max_loop, |e, c| c + e).hold(100)
        CellLoop(Type::Integer),
        Snapshot {
            function: SecondArgument + Argument,
            source: TopLevel(2),
            cell: TopLevel(4),
        },
        Hold {
            initial: Literal(100),
            source: TopLevel(5),
        },
        Close {
            forward: 4,
            definition: TopLevel(6),
        },
        // shield = damage.snapshot(shield_loop, |d, s| s.saturating_sub(d)).hold(30)
        CellLoop(Type::Integer),
        Snapshot {
            function: (SecondArgument - Argument).maximum(zero()),
            source: TopLevel(3),
            cell: TopLevel(8),
        },
        Hold {
            initial: Literal(30),
            source: TopLevel(9),
        },
        Close {
            forward: 8,
            definition: TopLevel(10),
        },
        // healed = heal.map(|h| h as i64)
        Map {
            function: Argument,
            source: TopLevel(0),
        },
        // took = damage.snapshot(shield, |d, s| -(d.saturating_sub(s)))
        Snapshot {
            function: zero() - (Argument - SecondArgument).maximum(zero()),
            source: TopLevel(3),
            cell: TopLevel(10),
        },
    ];
    // delta = healed.merge(took, +), or took alone.
    definitions.push(match drive {
        Drive::NoMerge => Node(TopLevel(13)),
        Drive::Full | Drive::NoMaxRead => Merge {
            function: Argument + SecondArgument,
            left: TopLevel(12),
            right: TopLevel(13),
        },
    });
    definitions.push(CellLoop(Type::Integer));
    match drive {
        Drive::Full | Drive::NoMerge => {
            // health = delta.snapshot3(health_loop, max_health, clamp).hold(60)
            definitions.push(Snapshot {
                function: Argument + SecondArgument,
                source: TopLevel(14),
                cell: TopLevel(15),
            });
            definitions.push(Snapshot {
                function: zero().maximum(SecondArgument.minimum(Argument)),
                source: TopLevel(16),
                cell: TopLevel(6),
            });
        }
        Drive::NoMaxRead => {
            // health = delta.snapshot(health_loop, |d, cur| max(0, cur + d)).hold(60)
            definitions.push(Snapshot {
                function: (Argument + SecondArgument).maximum(zero()),
                source: TopLevel(14),
                cell: TopLevel(15),
            });
            definitions.push(Map {
                function: Argument,
                source: TopLevel(16),
            });
        }
    }
    definitions.push(Hold {
        initial: Literal(60),
        source: TopLevel(17),
    });
    definitions.push(Close {
        forward: 15,
        definition: TopLevel(18),
    });
    let mut observed = vec![6, 10, 13, 14, 18];
    if fraction {
        definitions.push(Lift {
            function: ArgumentAt(1) * Literal(1000) + ArgumentAt(0),
            cells: vec![TopLevel(6), TopLevel(18)],
        });
        observed.push(definitions.len() - 1);
    }
    if effective {
        definitions.push(Lift {
            function: ArgumentAt(0) + ArgumentAt(1),
            cells: vec![TopLevel(18), TopLevel(10)],
        });
        observed.push(definitions.len() - 1);
    }
    driven(3, definitions, observed, schedule)
}

/// Record 0003's instant: heal 100, level_up 100 and damage 50 in one
/// transaction.
const RECORD_0003: &[&[(usize, i64)]] = &[&[(0, 100), (2, 100), (1, 50)]];

/// Research-loop-shapes's long schedule for the slice (shape 2b).
const LONG_RUN: &[&[(usize, i64)]] = &[
    &[(1, 10)],
    &[(0, 100), (1, 50), (2, 100)],
    &[(1, 30)],
    &[(0, 500), (2, 50)],
    &[(0, 100)],
    &[(1, 300)],
    &[(0, 40)],
];

/// The value of a cell after all its steps.
fn final_value(observation: &Observation) -> i64 {
    let Observation::Cell { initial, steps } = observation else {
        panic!("not a cell: {observation:?}");
    };
    match steps.last().map_or(initial, |(_, value)| value) {
        bough_oracle::Datum::Integer(value) => *value,
        other => panic!("not an integer: {other:?}"),
    }
}

#[test]
fn sodium_rust_52_health_is_100_in_every_row() {
    let Some(oracle) = oracle() else { return };
    for (fraction, effective) in [(false, false), (true, false), (false, true), (true, true)] {
        let answer = observe(
            oracle,
            &slice(Drive::Full, fraction, effective, RECORD_0003),
        );
        let row = format!("fraction {fraction}, effective {effective}");
        assert_eq!(answer[0], cell(100, &[(&[1], 200)]), "max_health, {row}");
        assert_eq!(answer[1], cell(30, &[(&[1], 0)]), "shield, {row}");
        assert_eq!(answer[2], stream(&[(&[1], -20)]), "took, {row}");
        assert_eq!(answer[3], stream(&[(&[1], 80)]), "delta, {row}");
        assert_eq!(answer[4], cell(60, &[(&[1], 100)]), "health, {row}");
        assert_eq!(final_value(&answer[4]), 100, "{row}");
    }
    let both = observe(oracle, &slice(Drive::Full, true, true, RECORD_0003));
    // Each lift steps once, although both of its inputs step at [1].
    assert_eq!(both[5], cell(60_100, &[(&[1], 100_200)]), "fraction");
    assert_eq!(both[6], cell(90, &[(&[1], 100)]), "effective");
}

#[test]
fn the_reductions_of_sodium_rust_52() {
    let Some(oracle) = oracle() else { return };
    for (drive, record_0003, long_run) in [
        (
            Drive::NoMaxRead,
            140,
            vec![60, 130, 100, 600, 700, 400, 440],
        ),
        (Drive::NoMerge, 40, vec![60, 30, 0, 0]),
    ] {
        let answer = observe(oracle, &slice(drive, false, false, RECORD_0003));
        assert_eq!(final_value(&answer[4]), record_0003, "{drive:?}");
        let answer = observe(oracle, &slice(drive, false, false, LONG_RUN));
        let Observation::Cell { steps, .. } = &answer[4] else {
            panic!("{answer:?}");
        };
        let values: Vec<_> = steps
            .iter()
            .map(|(_, value)| match value {
                bough_oracle::Datum::Integer(value) => *value,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(values, long_run, "{drive:?}");
    }
}

#[test]
fn the_health_and_shield_slice_on_the_long_schedule() {
    let Some(oracle) = oracle() else { return };
    let answer = observe(oracle, &slice(Drive::Full, true, true, LONG_RUN));
    assert_eq!(
        answer[..5],
        [
            cell(100, &[(&[2], 200), (&[4], 250)]),
            cell(30, &[(&[1], 20), (&[2], 0), (&[3], 0), (&[6], 0)]),
            stream(&[(&[1], 0), (&[2], -30), (&[3], -30), (&[6], -300)]),
            stream(&[
                (&[1], 0),
                (&[2], 70),
                (&[3], -30),
                (&[4], 500),
                (&[5], 100),
                (&[6], -300),
                (&[7], 40),
            ]),
            cell(
                60,
                &[
                    (&[1], 60),
                    (&[2], 100),
                    (&[3], 70),
                    (&[4], 200),
                    (&[5], 250),
                    (&[6], 0),
                    (&[7], 40),
                ],
            ),
        ]
    );
    assert_eq!(
        answer[6],
        cell(
            90,
            &[
                (&[1], 80),
                (&[2], 100),
                (&[3], 70),
                (&[4], 200),
                (&[5], 250),
                (&[6], 0),
                (&[7], 40),
            ],
        ),
        "effective"
    );
    // The research's fractions, from the encoded health and max_health: each
    // is the correctly rounded quotient, so the comparison is exact.
    let Observation::Cell { initial, steps } = &answer[5] else {
        panic!("{answer:?}");
    };
    let fraction = |value: &bough_oracle::Datum| match value {
        bough_oracle::Datum::Integer(value) => (value / 1000) as f64 / (value % 1000) as f64,
        other => panic!("{other:?}"),
    };
    assert_eq!(fraction(initial), 0.6);
    let times: Vec<_> = steps.iter().map(|(time, _)| time[0]).collect();
    let fractions: Vec<_> = steps.iter().map(|(_, value)| fraction(value)).collect();
    assert_eq!(times, [1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(fractions, [0.6, 0.5, 0.35, 0.8, 1.0, 0.0, 0.16]);
}

// ----- the protocol and the pool -----

/// The uncapped counter over twenty thousand transactions. Its loop needs a
/// round per transaction, 20,001 in all, and each round reads the counter's
/// steps at every tick, so the cost of a round grows with the square of the
/// ticks. Measured on one machine: over a thousand ticks the counter answers
/// in nine seconds, nine milliseconds a round; over twenty thousand a round
/// takes about four seconds, and the answer about twenty hours. Its memory
/// stays flat, under ten megabytes after a minute, so the answer cannot
/// become `ERR heap`.
fn never_finishes() -> Program {
    counter(20_000, None)
}

/// A program answered at once.
fn quick(value: i64) -> Program {
    vector(vec![Constant(Literal(value))], vec![0])
}

#[test]
fn a_program_that_never_finishes_answers_timeout() {
    let Some(oracle) = oracle() else { return };
    assert_eq!(oracle.answer(&never_finishes()), Ok(Answer::Timeout));
}

#[test]
fn many_programs_go_through_one_process() {
    let Some(_) = oracle() else { return };
    let pool = own_pool();
    for value in 0..200 {
        assert_eq!(observe(&pool, &quick(value)), vec![cell(value, &[])]);
    }
    assert_eq!(pool.processes_started(), 1);
}

#[test]
fn the_pool_serves_parallel_threads() {
    let Some(_) = oracle() else { return };
    let pool = own_pool();
    thread::scope(|scope| {
        for thread_index in 0..8_i64 {
            let pool = &pool;
            scope.spawn(move || {
                for index in 0..25 {
                    let value = thread_index * 1000 + index;
                    let program = driven(
                        1,
                        vec![
                            Definition::Input(0),
                            Map {
                                function: Argument + Literal(value),
                                source: TopLevel(0),
                            },
                        ],
                        vec![1],
                        &[&[(0, 1)]],
                    );
                    assert_eq!(observe(pool, &program), vec![stream(&[(&[1], value + 1)])]);
                }
            });
        }
    });
    let started = pool.processes_started();
    assert!((1..=8).contains(&started), "{started} processes");
}

#[cfg(unix)]
#[test]
fn a_process_killed_while_idle_is_replaced() {
    let Some(_) = oracle() else { return };
    let pool = own_pool();
    let program = quick(1);
    assert_eq!(observe(&pool, &program), vec![cell(1, &[])]);
    let identifiers = pool.idle_process_ids();
    assert_eq!(identifiers.len(), 1);
    let killed = Command::new("sh")
        .arg("-c")
        .arg(format!("kill -KILL {}", identifiers[0]))
        .status()
        .unwrap();
    assert!(killed.success());
    // The pool either finds the process dead before it sends, and starts
    // another, or finds out while it answers, and names the program.
    match pool.answer(&program) {
        Ok(answer) => assert_eq!(answer, Answer::Observed(vec![cell(1, &[])])),
        Err(Error::Died {
            program: reported, ..
        }) => assert_eq!(reported, program.to_string()),
        other => panic!("{}", brief(&other)),
    }
    assert_eq!(observe(&pool, &program), vec![cell(1, &[])]);
    assert_eq!(pool.processes_started(), 2);
}

#[test]
fn a_process_that_does_not_answer_in_time_is_killed_and_the_program_reported() {
    let Some(_) = oracle() else { return };
    // The program would answer TIMEOUT after five seconds; the watchdog
    // gives up first.
    let pool = own_pool().with_watchdog(Duration::from_secs(2));
    let slow = never_finishes();
    match pool.answer(&slow) {
        Err(Error::NoAnswer {
            program, waited, ..
        }) => {
            assert_eq!(program, slow.to_string());
            assert_eq!(waited, Duration::from_secs(2));
        }
        other => panic!("{}", brief(&other)),
    }
    assert_eq!(observe(&pool, &quick(2)), vec![cell(2, &[])]);
    assert_eq!(pool.processes_started(), 2);
}

#[test]
fn a_heap_overflow_answers_err_and_the_process_lives_on() {
    let Some(_) = oracle() else { return };
    // A stream loop that triples its events every round, in a 32 MB heap.
    let pool = own_pool().with_heap_limit(32);
    let program = driven(
        1,
        vec![
            Definition::Input(0),
            StreamLoop(Type::Integer),
            MapList {
                length: Literal(3),
                element: SecondArgument,
                source: TopLevel(1),
            },
            Split(TopLevel(2)),
            OrElse {
                left: TopLevel(0),
                right: TopLevel(3),
            },
            Close {
                forward: 1,
                definition: TopLevel(4),
            },
        ],
        vec![4],
        &[&[(0, 0)]],
    );
    let message = error_message(&pool, &program);
    assert!(message.starts_with("heap"), "{message}");
    assert_eq!(observe(&pool, &quick(3)), vec![cell(3, &[])]);
    assert_eq!(pool.processes_started(), 1);
}

#[test]
fn extreme_integers_round_trip_and_arithmetic_wraps() {
    let Some(oracle) = oracle() else { return };
    // GHC's Read takes a negative number with or without its parentheses, so
    // this shows that the values survive the trip; the printer's own tests
    // hold it to the parentheses.
    let program = Program {
        window: Window::Everything,
        inputs: vec![Input::new(Type::Integer), Input::new(Type::List)],
        definitions: vec![
            Constant(Literal(i64::MIN)),
            Constant(Literal(i64::MAX)),
            Constant(Literal(i64::MAX) + Literal(1)),
            Constant(Literal(i64::MIN) * Literal(-1)),
            Constant(Literal(-7).modulo(3)),
            Constant(Literal(i64::MIN) - Literal(1)),
            Definition::Literal {
                event_type: Type::Integer,
                events: vec![
                    (vec![1], Value::Integer(i64::MIN)),
                    (vec![2], Value::Integer(i64::MAX)),
                ],
            },
            Definition::Input(0),
            Definition::Input(1),
        ],
        observe: (0..9).collect(),
        schedule: vec![
            vec![
                (0, Value::Integer(i64::MIN)),
                (1, Value::List(vec![i64::MIN, -1, i64::MAX])),
            ],
            vec![(0, Value::Integer(i64::MAX))],
        ],
    };
    let extremes = [(vec![1], i64::MIN), (vec![2], i64::MAX)];
    assert_eq!(
        observe(oracle, &program),
        vec![
            cell(i64::MIN, &[]),
            cell(i64::MAX, &[]),
            cell(i64::MAX.wrapping_add(1), &[]),
            cell(i64::MIN.wrapping_mul(-1), &[]),
            cell((-7_i64).rem_euclid(3), &[]),
            cell(i64::MIN.wrapping_sub(1), &[]),
            Observation::stream(extremes.clone()),
            Observation::stream(extremes),
            Observation::stream([(vec![1], vec![i64::MIN, -1, i64::MAX])]),
        ]
    );
}

#[test]
fn not_if_and_a_sample_at_the_top_level_evaluate_as_documented() {
    let Some(oracle) = oracle() else { return };
    // Input 0 sends 0, 1 and -3; input 1 steps its cell from 7 to 9 at [1].
    let program = driven(
        2,
        vec![
            Definition::Input(0),
            InputCell {
                input: 1,
                initial: Literal(7),
            },
            Map {
                function: !Argument,
                source: TopLevel(0),
            },
            Map {
                function: Expression::if_then_else(Argument, Literal(10), Literal(20)),
                source: TopLevel(0),
            },
            // At the top level a sample reads the cell as it was before the
            // build, [0], at whatever instant the expression runs.
            Map {
                function: Argument + Expression::Sample(TopLevel(1)),
                source: TopLevel(0),
            },
            Constant(Expression::Sample(TopLevel(1)) * Literal(2)),
            Hold {
                initial: Expression::Sample(TopLevel(1)),
                source: TopLevel(0),
            },
        ],
        vec![2, 3, 4, 5, 6],
        &[&[(0, 0), (1, 9)], &[(0, 1)], &[(0, -3)]],
    );
    assert_eq!(
        observe(oracle, &program),
        vec![
            stream(&[(&[1], 1), (&[2], 0), (&[3], 0)]),
            stream(&[(&[1], 20), (&[2], 10), (&[3], 10)]),
            stream(&[(&[1], 7), (&[2], 8), (&[3], 4)]),
            cell(14, &[]),
            cell(7, &[(&[1], 0), (&[2], 1), (&[3], -3)]),
        ]
    );
}

#[test]
fn a_malformed_program_answers_err_naming_the_node() {
    let Some(oracle) = oracle() else { return };
    let program = vector(
        vec![
            Definition::Literal {
                event_type: Type::List,
                events: vec![(vec![0], Value::List(vec![1]))],
            },
            Map {
                function: Argument,
                source: TopLevel(0),
            },
        ],
        vec![1],
    );
    let message = error_message(oracle, &program);
    assert!(
        message.starts_with("node 1 (SMap): N 0 carries TList"),
        "{message}"
    );
    assert_eq!(
        oracle.answer_line("this is not a program").unwrap(),
        Answer::Error(
            "parse: the line is not a Program in the syntax of Oracle.Program".to_owned()
        )
    );
}

// ----- without GHC -----

/// A test that needs GHC and does nothing else. The test below runs it again
/// in a child process, with GHC pointed at nothing.
#[test]
fn a_test_that_needs_ghc() {
    let Some(oracle) = oracle() else { return };
    assert_eq!(observe(oracle, &quick(4)), vec![cell(4, &[])]);
}

#[test]
fn without_ghc_a_test_panics_with_the_install_hint_unless_told_to_skip() {
    let run = |setting: Option<&str>| {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["a_test_that_needs_ghc", "--exact", "--nocapture"])
            .env("BOUGH_GHC", "/nonexistent/bough-oracle-test/ghc")
            .env_remove("BOUGH_ORACLE");
        if let Some(setting) = setting {
            command.env("BOUGH_ORACLE", setting);
        }
        let output = command.output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (output.status.success(), text)
    };

    let (passed, text) = run(None);
    assert!(!passed, "{text}");
    assert!(
        text.contains("apt-get install ghc libghc-hunit-dev"),
        "{text}"
    );
    assert!(text.contains("BOUGH_ORACLE=skip"), "{text}");
    assert!(
        text.contains("/nonexistent/bough-oracle-test/ghc"),
        "{text}"
    );

    let (passed, text) = run(Some("skip"));
    assert!(passed, "{text}");
    assert!(text.contains("skipped: BOUGH_ORACLE=skip"), "{text}");
    assert!(text.contains("1 passed"), "{text}");

    let (passed, text) = run(Some("yes"));
    assert!(!passed, "{text}");
    assert!(text.contains("the one value it takes is `skip`"), "{text}");
}

// ----- the Haskell tests -----

/// The counts a run of an HUnit test prints last: cases, tried, errors,
/// failures.
fn counts(text: &str, from: &str) -> Vec<u64> {
    let last = text.rsplit(from).next().unwrap_or_default();
    last.split(|character: char| !character.is_ascii_digit())
        .filter(|digits| !digits.is_empty())
        .take(4)
        .map(|digits| digits.parse().unwrap())
        .collect()
}

/// The groups of `OracleTests.hs`, and the cases each holds. The first five
/// are the research's tests of every operation (69 cases), the next five its
/// verification's (35). A case added or removed changes a count here.
const HASKELL_GROUPS: &[(&str, usize)] = &[
    ("sodium.hs vectors through Derived", 20),
    ("common-tests SemanticTests.hs, Denotational.hs times", 10),
    ("Bough operations", 28),
    ("loops", 6),
    ("text versus Derived", 5),
    ("verification: creation time", 11),
    ("verification: simultaneous events", 7),
    ("verification: child-transaction times", 7),
    ("verification: loops", 7),
    ("verification: text claims", 3),
    ("patches", 5),
    ("interpreter: sodium.hs vectors", 20),
    ("interpreter: common-tests vectors", 5),
    ("interpreter: derived operations", 9),
    ("interpreter: loops", 13),
    ("interpreter: checks", 15),
    ("interpreter: answers", 7),
];

/// The group of each case the HUnit runner reports as passed. Its lines
/// read `ok   <index>:<label>:…`, the label in quotes when it holds a colon.
fn passed_groups(text: &str) -> Vec<&str> {
    text.lines()
        .filter_map(|line| line.strip_prefix("ok   "))
        .filter_map(|path| {
            let (_, rest) = path.split_once(':')?;
            match rest.strip_prefix('"') {
                Some(quoted) => quoted.split('"').next(),
                None => rest.split(':').next(),
            }
        })
        .collect()
}

#[test]
fn the_haskell_tests_and_the_vendored_vectors_pass() {
    let Some(_) = oracle() else { return };
    let directory = directory();

    let tests = bough_oracle::compile_haskell(&directory, "OracleTests.hs", "oracle-tests", &[])
        .unwrap_or_else(|error| panic!("{error}"));
    let output = Command::new(&tests).output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{text}");
    let [cases, tried, errors, failures] = counts(&text, "Counts {")[..] else {
        panic!("no counts in {text}");
    };
    let groups = passed_groups(&text);
    for (label, expected) in HASKELL_GROUPS {
        let passed = groups.iter().filter(|group| *group == label).count();
        assert_eq!(passed, *expected, "the group {label:?}\n{text}");
    }
    let total = HASKELL_GROUPS.iter().map(|(_, cases)| cases).sum::<usize>();
    assert_eq!(
        groups.len(),
        total,
        "a group this test does not list\n{text}"
    );
    let total = u64::try_from(total).unwrap();
    assert_eq!(
        (cases, tried, errors, failures),
        (total, total, 0, 0),
        "{text}"
    );

    // sodium.hs prints its counts on standard error and exits 0 either way.
    let vectors = bough_oracle::compile_haskell(&directory, "sodium.hs", "sodium-vectors", &[])
        .unwrap_or_else(|error| panic!("{error}"));
    let output = Command::new(&vectors).output().unwrap();
    let text = String::from_utf8_lossy(&output.stderr);
    assert_eq!(counts(&text, "Cases:"), [20, 20, 0, 0], "{text}");
}
