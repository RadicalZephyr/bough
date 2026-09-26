//! Switches (RFD 4): `switch_cell`, the semantics' `SwitchC` with the
//! oracle's patch F6, and `switch_stream`, the semantics' `SwitchS`.
//!
//! The expected values are GHC's. The program is
//! `bough-oracle/haskell/probes/stage5/Stage5.hs`, which runs each test's program over an unchanged
//! copy of the vendored Denotational.hs with the oracle's patch F6 (a
//! switch cell created at t0 switches over its outer chopped at t0,
//! bough-oracle's `switchCell`) and solves each loop by fixed-point
//! iteration; `Stage5.out` beside it is its output, and each test quotes the
//! lines it uses. Instant `[0]` is the build and `[k]` the k-th transaction
//! after it; every switch here is built at `[0]`. GHC prints a cell's steps
//! as `(value before [0], [(instant, value)])`. The tests record a cell's
//! steps, or a stream's events, with an accumulator built beside it, so
//! the step at `[0]` is recorded too, and sample it after the build and
//! after each transaction to know each step's instant.
//!
//! Every test runs under the plain order and five shuffle seeds, and where
//! the order of sends could matter, with each transaction's sends in both
//! orders; each must give GHC's values under all of them. With the
//! `statistics` feature the tests also count the out-of-order pulls the
//! read after a switch instant takes, and the relinks.

use std::cell::{Cell as StdCell, RefCell};
use std::fmt::Debug;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use bough::{
    Build, Cell, Input, Lift, Local, PoisonedError, Runtime, SendError, Shared, Source, State,
    Stream, TokenError, Trace, Transaction,
};

/// The plain order, then seeds for RFD 1's order shuffle.
const SEEDS: [Option<u64>; 6] = [None, Some(0), Some(1), Some(7), Some(42), Some(1 << 40)];

/// How one run orders what it can: the shuffle seed, and whether each
/// transaction's sends go in reversed.
#[derive(Clone, Copy, Debug)]
struct Order {
    seed: Option<u64>,
    reversed: bool,
}

/// Runs a program under every seed with each transaction's sends in both
/// orders, checks that every run observed the same, and returns it.
fn every_order<R: PartialEq + Debug>(program: impl Fn(Order) -> R) -> R {
    let plain = program(Order {
        seed: None,
        reversed: false,
    });
    for reversed in [false, true] {
        for seed in SEEDS {
            let order = Order { seed, reversed };
            assert_eq!(program(order), plain, "{order:?}");
        }
    }
    plain
}

/// One send of a transaction, whatever its input's type.
type Send = Box<dyn Fn(&mut Transaction<'_, Local>)>;

fn send<A: Copy + 'static>(input: Input<A>, value: A) -> Send {
    Box::new(move |tx| tx.send(input, value))
}

/// Runs one transaction with `sends` in their order, or reversed.
fn run(graph: &mut Runtime, sends: &[Send], reversed: bool) {
    graph.transaction(|tx| {
        if reversed {
            sends.iter().rev().for_each(|s| s(tx));
        } else {
            sends.iter().for_each(|s| s(tx));
        }
    });
}

/// A cell holding every step of `cell`, built beside it, so that it
/// records the step at `[0]` too.
fn log_steps<A: Clone + Trace + 'static>(b: &mut Build, cell: Cell<A>) -> Cell<Vec<A>> {
    let steps = cell.steps(b);
    log_events(b, steps)
}

/// A cell holding every event of `stream`.
fn log_events<A: Clone + Trace + 'static>(b: &mut Build, stream: Stream<A>) -> Cell<Vec<A>> {
    stream.accumulate(b, Vec::new(), |v, log: &Vec<A>| {
        let mut log = log.clone();
        log.push(v);
        log
    })
}

/// A cell's steps or a stream's events the way GHC prints them: `(k,
/// value)` for instant `[k]`.
type ByInstant<T> = Vec<(usize, T)>;

/// A log sampled after the build and after each transaction, as the steps
/// or events of each instant.
fn by_instant<T: Clone>(logs: &[Vec<T>]) -> ByInstant<T> {
    let mut out = Vec::new();
    let mut seen = 0;
    for (k, log) in logs.iter().enumerate() {
        out.extend(log[seen..].iter().map(|v| (k, v.clone())));
        seen = log.len();
    }
    out
}

/// Runs `schedule`, one transaction per entry, and returns each log's
/// steps by instant and each cell's samples, after the build and after
/// each transaction.
fn drive<L: Clone + 'static, C: Clone + 'static>(
    graph: &mut Runtime,
    order: Order,
    schedule: &[Vec<Send>],
    logs: &[Cell<Vec<L>>],
    cells: &[Cell<C>],
) -> (Vec<ByInstant<L>>, Vec<Vec<C>>) {
    graph.set_shuffle_seed(order.seed);
    let mut logged: Vec<Vec<Vec<L>>> = logs
        .iter()
        .map(|l| vec![graph.sample(*l).clone()])
        .collect();
    let mut sampled: Vec<Vec<C>> = cells
        .iter()
        .map(|c| vec![graph.sample(*c).clone()])
        .collect();
    for sends in schedule {
        run(graph, sends, order.reversed);
        for (log, l) in logged.iter_mut().zip(logs) {
            log.push(graph.sample(*l).clone());
        }
        for (samples, c) in sampled.iter_mut().zip(cells) {
            samples.push(graph.sample(*c).clone());
        }
    }
    (logged.iter().map(|l| by_instant(l)).collect(), sampled)
}

fn recorder<T: 'static>() -> (Rc<RefCell<Vec<T>>>, impl FnMut(T) + 'static) {
    let log = Rc::new(RefCell::new(Vec::new()));
    let writer = log.clone();
    (log, move |v| writer.borrow_mut().push(v))
}

/// A counter a closure can share with the test.
fn counter() -> (Rc<StdCell<u32>>, Rc<StdCell<u32>>) {
    let c = Rc::new(StdCell::new(0));
    (c.clone(), c)
}

/// The message of the panic `f` raises.
fn panic_message<R>(f: impl FnOnce() -> R) -> String {
    let payload = match catch_unwind(AssertUnwindSafe(f)) {
        Ok(_) => panic!("expected a panic"),
        Err(payload) => payload,
    };
    if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else {
        String::new()
    }
}

/// The out-of-order pulls, the evaluations in order and the relinks since
/// the graph was built, with the `statistics` feature.
#[cfg(feature = "statistics")]
fn counts(graph: &Runtime) -> Option<[u64; 3]> {
    let s = graph.statistics();
    Some([s.pulls, s.evaluations, s.relinks])
}

#[cfg(not(feature = "statistics"))]
fn counts(_: &Runtime) -> Option<[u64; 3]> {
    None
}

/// What `drive` did to the counters: pulls, evaluations, relinks.
fn counted(graph: &mut Runtime, drive: impl FnOnce(&mut Runtime)) -> Option<[u64; 3]> {
    let before = counts(graph);
    drive(graph);
    let after = counts(graph)?;
    let before = before?;
    Some([0, 1, 2].map(|i| after[i] - before[i]))
}

// ----------------------------------------------------------- the text's vectors

/// `SwitchS` of sodium.hs with its events moved from `[k]` to `[k + 1]`:
/// the outer selects s2 at [2], where s2's X is not forwarded and s1's b
/// is. Stage5.hs:
///
/// ```text
/// vector SwitchS: [([1],'a'),([2],'b'),([3],'Y'),([4],'Z')]
/// ```
#[test]
fn the_switch_stream_vector_restated_with_inputs() {
    let events = every_order(|order| {
        let (mut graph, edge) = Runtime::build(|b| {
            let (s1, s1_in) = b.input::<char>();
            let (s2, s2_in) = b.input::<char>();
            let (sel, sel_in) = b.input::<()>();
            let (s1, s2) = (s1.share(b), s2.share(b));
            let c = sel.map(move |_| s2).hold(b, s1);
            b.depends(&c, &[&s2]);
            let out = c.switch_stream(b);
            (s1_in, s2_in, sel_in, log_events(b, out))
        });
        let (s1_in, s2_in, sel_in, log) = edge.keep();
        let schedule = [
            vec![send(s1_in, 'a'), send(s2_in, 'W')],
            vec![send(s1_in, 'b'), send(s2_in, 'X'), send(sel_in, ())],
            vec![send(s1_in, 'c'), send(s2_in, 'Y')],
            vec![send(s1_in, 'd'), send(s2_in, 'Z')],
        ];
        let (logs, _) = drive::<char, ()>(&mut graph, order, &schedule, &[log], &[]);
        logs
    });
    assert_eq!(events[0], [(1, 'a'), (2, 'b'), (3, 'Y'), (4, 'Z')]);
}

/// The inputs of one of sodium.hs's `SwitchC` vectors, moved from `[k]` to
/// `[k + 1]`: c1's, c2's and c3's sends and the outer's selections at
/// [1] to [4]. c1 is always 'a' then b, c, d, e.
struct SwitchCVector {
    c2: char,
    c2_sends: [Option<char>; 4],
    c3_sends: [Option<char>; 4],
    selects: [Option<char>; 4],
}

/// The switch's steps by instant and its samples.
fn switch_c_vector(v: &SwitchCVector, order: Order) -> (Vec<(usize, char)>, Vec<char>) {
    let (mut graph, edge) = Runtime::build(|b| {
        let (c1, c1_in) = b.input_cell('a');
        let (c2, c2_in) = b.input_cell(v.c2);
        let (c3, c3_in) = b.input_cell('1');
        let (sel, sel_in) = b.input::<char>();
        let outer = sel.map(move |s| if s == '2' { c2 } else { c3 }).hold(b, c1);
        b.depends(&outer, &[&c2, &c3]);
        let sw = outer.switch_cell(b);
        ([c1_in, c2_in, c3_in], sel_in, log_steps(b, sw), sw)
    });
    let (inputs, sel_in, log, sw) = edge.keep();
    let c1_sends = [Some('b'), Some('c'), Some('d'), Some('e')];
    let schedule: Vec<Vec<Send>> = (0..4)
        .map(|k| {
            let sends = [c1_sends[k], v.c2_sends[k], v.c3_sends[k]];
            let mut sends: Vec<Send> = inputs
                .iter()
                .zip(sends)
                .filter_map(|(input, value)| Some(send(*input, value?)))
                .collect();
            if let Some(s) = v.selects[k] {
                sends.push(send(sel_in, s));
            }
            sends
        })
        .collect();
    let (logs, samples) = drive(&mut graph, order, &schedule, &[log], &[sw]);
    (logs[0].clone(), samples[0].clone())
}

/// `SwitchC 1` to `SwitchC 4` of sodium.hs with their events moved from
/// `[k]` to `[k + 1]`. The switch steps at [0], its creation; the outer
/// switches to c2 at [2], where c2 steps (1), or has stepped before and is
/// quiet (2), or has never stepped and is quiet (3); in 4 it switches to c3
/// at [4], where c3 steps. Stage5.hs, where the text's `SwitchC` agrees
/// with the patched one on all four:
///
/// ```text
/// vector SwitchC 1: ('a',[([0],'a'),([1],'b'),([2],'X'),([3],'Y'),([4],'Z')])
/// vector SwitchC 1 samples: "abXYZ"
/// vector SwitchC 2: ('a',[([0],'a'),([1],'b'),([2],'X'),([3],'Y'),([4],'Z')])
/// vector SwitchC 2 samples: "abXYZ"
/// vector SwitchC 3: ('a',[([0],'a'),([1],'b'),([2],'X'),([3],'Y'),([4],'Z')])
/// vector SwitchC 3 samples: "abXYZ"
/// vector SwitchC 4: ('a',[([0],'a'),([1],'b'),([2],'X'),([3],'Y'),([4],'5')])
/// vector SwitchC 4 samples: "abXY5"
/// ```
#[test]
fn the_switch_cell_vectors_restated_with_inputs() {
    let all = [Some('W'), Some('X'), Some('Y'), Some('Z')];
    let none = [None; 4];
    let at_2 = [None, Some('2'), None, None];
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
            selects: [None, Some('2'), None, Some('3')],
        },
    ];
    let expected = ["abXYZ", "abXYZ", "abXYZ", "abXY5"];
    for (v, expected) in vectors.iter().zip(expected) {
        let (steps, samples) = every_order(|order| switch_c_vector(v, order));
        let steps_expected: Vec<(usize, char)> = expected.chars().enumerate().collect();
        assert_eq!(steps, steps_expected);
        assert_eq!(samples.iter().collect::<String>(), expected);
    }
}

// ----------------------------------------------------------- claim 5

/// Claim 5, the read after the instant through a switch (probe 9): the
/// outer switches from a constant to c2 at the instant x makes c2 step,
/// and the switch's step carries c2's value after the instant, 70, which
/// the steps view reads before the order reaches c2 when x is sent first.
/// So one send order pulls c2 out of order once and the other does not
/// pull, and c2's function runs once either way; each node runs once, in
/// order or pulled. Then c2 steps alone. Stage5.hs:
///
/// ```text
/// claim5: steps sc: (1,[([0],1),([1],70),([2],80)])
/// claim5: samples: [1,70,80]
/// ```
#[test]
fn claim5_a_switch_cell_reads_its_new_inner_after_the_instant_in_both_send_orders() {
    let program = |x_first: bool, seed: Option<u64>| {
        let (calls, c) = counter();
        let (mut graph, edge) = Runtime::build(move |b| {
            let (sel, sel_in) = b.input::<()>();
            let (x, x_in) = b.input::<u32>();
            let a = b.constant(1u32);
            let c2 = x
                .map(move |v| {
                    c.set(c.get() + 1);
                    v * 10
                })
                .hold(b, 2u32);
            let outer = sel.map(move |_| c2).hold(b, a);
            b.depends(&outer, &[&c2]);
            let sc = outer.switch_cell(b);
            (sel_in, x_in, sc, log_steps(b, sc))
        });
        let (sel_in, x_in, sc, log) = edge.keep();
        graph.set_shuffle_seed(seed);
        let (seen, mut on) = recorder();
        graph.listen_steps(sc, move |v| on(*v)).keep();
        let switch = counted(&mut graph, |g| {
            g.transaction(|tx| {
                if x_first {
                    tx.send(x_in, 7);
                    tx.send(sel_in, ());
                } else {
                    tx.send(sel_in, ());
                    tx.send(x_in, 7);
                }
            })
        });
        let at_switch = (calls.get(), *graph.sample(sc));
        graph.send(x_in, 8);
        let steps = graph.sample(log).clone();
        (steps, seen.take(), at_switch, *graph.sample(sc), switch)
    };
    for x_first in [true, false] {
        let (steps, heard, at_switch, last, switch) = program(x_first, None);
        assert_eq!(steps, [1, 70, 80]);
        assert_eq!(heard, [70, 80]);
        assert_eq!(
            at_switch,
            (1, 70),
            "c2's function ran once, x_first = {x_first}"
        );
        assert_eq!(last, 80);
        if let Some([pulls, evaluations, relinks]) = switch {
            // The starts are walked in send order and the order is run from
            // its end, so the start sent last is evaluated first: with x
            // first the steps view runs before c2 and pulls it.
            assert_eq!(pulls, u64::from(x_first), "x_first = {x_first}");
            assert_eq!(
                pulls + evaluations,
                5,
                "each node ran once: c2, outer, the switch, its steps view, the log"
            );
            assert_eq!(relinks, 1);
        }
        for seed in &SEEDS[1..] {
            let (s, h, a, l, _) = program(x_first, *seed);
            assert_eq!(
                (s, h, a, l),
                (steps.clone(), heard.clone(), at_switch, last)
            );
        }
    }
}

// ----------------------------------------------------------- the judges' programs

/// R3: steps of a map_cell over a loop closed with a switch_cell after the
/// readers were created. A flag fixed at materialization gave 3 in one
/// send order. Stage5.hs (review-fidelity/hs/Review.hs with F6):
///
/// ```text
/// R3: steps (map_cell (+1) loop): (2,[([0],2),([1],71)])
/// ```
#[test]
fn r3_steps_of_a_map_cell_over_a_loop_closed_with_a_switch_cell() {
    let steps = every_order(|order| {
        let (mut graph, edge) = Runtime::build(|b| {
            let (c, closer) = b.cell_loop::<u32>();
            let m = c.map_cell(b, |v| v + 1);
            let log = log_steps(b, m);
            let (sel, sel_in) = b.input::<()>();
            let (x, x_in) = b.input::<u32>();
            let a = b.constant(1u32);
            let c2 = x.map(|v| v * 10).hold(b, 2u32);
            let outer = sel.map(move |_| c2).hold(b, a);
            b.depends(&outer, &[&c2]);
            let sw = outer.switch_cell(b);
            closer.close(b, sw);
            (sel_in, x_in, log)
        });
        let (sel_in, x_in, log) = edge.keep();
        let schedule = [vec![send(x_in, 7), send(sel_in, ())]];
        let (logs, _) = drive::<u32, ()>(&mut graph, order, &schedule, &[log], &[]);
        logs
    });
    assert_eq!(steps[0], [(0, 2), (1, 71)]);
}

/// R4: two switch_cells switching in one instant, the innermost new inner
/// stepping then, in all six orders of the three sends. Stage5.hs:
///
/// ```text
/// R4: steps top: (0,[([0],0),([1],70)])
/// R4: samples top: [0,70]
/// ```
#[test]
fn r4_nested_switch_cells_switch_together_in_every_send_order() {
    let permutations = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    for permutation in permutations {
        let (steps, samples) = every_order(|order| {
            let (mut graph, edge) = Runtime::build(|b| {
                let (sa, sa_in) = b.input::<()>();
                let (sb, sb_in) = b.input::<()>();
                let (x, x_in) = b.input::<u32>();
                let c0 = b.constant(0u32);
                let c1 = b.constant(1u32);
                let c2 = x.map(|v| v * 10).hold(b, 2u32);
                let outer_b = sb.map(move |_| c2).hold(b, c1);
                b.depends(&outer_b, &[&c2]);
                let inner_b = outer_b.switch_cell(b);
                let outer_a = sa.map(move |_| inner_b).hold(b, c0);
                b.depends(&outer_a, &[&inner_b]);
                let top = outer_a.switch_cell(b);
                (sa_in, sb_in, x_in, log_steps(b, top), top)
            });
            let (sa_in, sb_in, x_in, log, top) = edge.keep();
            let mut sends = [
                Some(send(sa_in, ())),
                Some(send(sb_in, ())),
                Some(send(x_in, 7)),
            ];
            let schedule = [permutation.map(|i| sends[i].take().unwrap()).into()];
            let (logs, samples) = drive(&mut graph, order, &schedule, &[log], &[top]);
            (logs[0].clone(), samples[0].clone())
        });
        assert_eq!(steps, [(0, 0), (1, 70)], "{permutation:?}");
        assert_eq!(samples, [0, 70]);
    }
}

/// R5: steps of a lift over a switch_cell and a hold at a switch instant,
/// both sides stepping: one step, 70 + 3. Stage5.hs:
///
/// ```text
/// R5: steps lift: (1,[([0],1),([1],73)])
/// ```
#[test]
fn r5_steps_of_a_lift_over_a_switch_cell_at_the_switch_instant() {
    let steps = every_order(|order| {
        let (mut graph, edge) = Runtime::build(|b| {
            let (sel, sel_in) = b.input::<()>();
            let (x, x_in) = b.input::<u32>();
            let x = x.share(b);
            let a = b.constant(1u32);
            let c2 = x.map(|v| v * 10).hold(b, 2u32);
            let other = x.map(|v| v - 4).hold(b, 0u32);
            let outer = sel.map(move |_| c2).hold(b, a);
            b.depends(&outer, &[&c2]);
            let sw = outer.switch_cell(b);
            let l = (sw, other).lift(b, |p, q| p + q);
            (sel_in, x_in, log_steps(b, l))
        });
        let (sel_in, x_in, log) = edge.keep();
        let schedule = [vec![send(x_in, 7), send(sel_in, ())]];
        let (logs, _) = drive::<u32, ()>(&mut graph, order, &schedule, &[log], &[]);
        logs
    });
    assert_eq!(steps[0], [(0, 1), (1, 73)]);
}

/// R8: a switch_cell over a cell loop that is not closed yet. The switch
/// links its inner at its first evaluation, at the end of the build, so
/// creating it reads nothing. Stage5.hs:
///
/// ```text
/// R8: samples cur: [1,2]
/// ```
#[test]
fn r8_a_switch_cell_over_a_loop_cell_that_is_not_closed_yet() {
    let samples = every_order(|order| {
        let (mut graph, edge) = Runtime::build(|b| {
            let (outer, closer) = b.cell_loop::<Cell<u32>>();
            let cur = outer.switch_cell(b);
            let (sel, sel_in) = b.input::<()>();
            let a = b.constant(1u32);
            let c = b.constant(2u32);
            let def = sel.map(move |_| c).hold(b, a);
            b.depends(&def, &[&c]);
            closer.close(b, def);
            (sel_in, cur)
        });
        let (sel_in, cur) = edge.keep();
        let schedule = [vec![send(sel_in, ())]];
        let (_, samples) = drive::<(), u32>(&mut graph, order, &schedule, &[], &[cur]);
        samples
    });
    assert_eq!(samples[0], [1, 2]);
}

/// The program of R10 and R10b: at [1] the outer selects a map_cell over
/// the loop the switch defines, a cell computed from the switch itself at
/// the same instant, which the text cannot evaluate. With `steps`, a steps
/// view of the loop reads the switch after the instant.
fn r10(steps: bool) -> (Runtime, Input<()>, Cell<u32>) {
    let (graph, edge) = Runtime::build(|b| {
        let (sel, sel_in) = b.input::<()>(); // node 1
        let a = b.constant(1u32); // node 2
        let (c, closer) = b.cell_loop::<u32>(); // node 3
        let m = c.map_cell(b, |v| v + 1); // node 4
        let outer = sel.map(move |_| m).hold(b, a); // node 5
        b.depends(&outer, &[&m]);
        let sw = outer.switch_cell(b); // node 6
        closer.close(b, sw);
        // Returned, so that collection keeps the view.
        let steps = steps.then(|| c.steps(b));
        (sel_in, c, steps)
    });
    let (sel_in, c, _steps) = edge.keep();
    (graph, sel_in, c)
}

/// Every entry reports the poison after a panic escaped a transaction.
fn assert_poisoned(graph: &mut Runtime, input: Input<()>, cell: Cell<u32>) {
    assert_eq!(graph.try_send(input, ()), Err(SendError::Poisoned));
    assert_eq!(graph.try_transaction(|_| ()), Err(PoisonedError));
    assert_eq!(graph.try_sample(cell).err(), Some(TokenError::Poisoned));
    assert!(panic_message(|| graph.send(input, ())).contains("poisoned"));
}

/// R10: the steps view prepares the switch, which prepares the map_cell it
/// switches to, which prepares the loop, which prepares the switch again:
/// prepare's re-entry stamp makes the cycle a panic, where plain recursion
/// would overflow the stack, and the panic poisons the graph.
#[test]
fn r10_a_switch_into_a_cell_computed_from_itself_panics_and_poisons() {
    for seed in SEEDS {
        let (mut graph, sel_in, c) = r10(true);
        graph.set_shuffle_seed(seed);
        assert_eq!(*graph.sample(c), 1);
        let message = panic_message(|| graph.send(sel_in, ()));
        assert!(
            message.contains("a same-instant cycle through a read after the instant"),
            "{message}"
        );
        assert_poisoned(&mut graph, sel_in, c);
    }
}

/// R10b: nothing reads the switch after the instant, so the instant runs,
/// and relink's check at commit finds the cycle the move closes, names
/// its nodes in the direction values flow, and poisons the graph.
#[test]
fn r10b_the_relink_check_finds_the_cycle_without_a_reader() {
    for seed in SEEDS {
        let (mut graph, sel_in, c) = r10(false);
        graph.set_shuffle_seed(seed);
        let message = panic_message(|| graph.send(sel_in, ()));
        assert!(
            message.contains(
                "switching closes a same-instant cycle: node 6 (SwitchCell) -> node 3 (Loop) -> \
                 node 4 (ReadThrough) -> node 6"
            ),
            "{message}"
        );
        assert_poisoned(&mut graph, sel_in, c);
    }
}

/// Two switches reverse a dependency between them in one instant. Before
/// [1], B follows p = A + 10, so B depends on A; at [1], A moves to
/// y = B + 100 while B moves to a constant, so A depends on B. The graph
/// the two moves make together is acyclic. A depends on B through its new
/// inner only once B has left p, and A settles first, since B depends on
/// it: a relink that checked each move as it made it would find the cycle
/// y, B, p, the loop, A through B's old inner and refuse a legal program.
/// Relink checks once every switch has moved. Stage5.hs:
///
/// ```text
/// reversal: steps a: (1,[([0],1),([1],102)])
/// reversal: steps b: (11,[([0],11),([1],2)])
/// reversal: samples a, b, p, y: ([1,102,102],[11,2,2],[11,112,112],[111,102,102])
/// ```
#[test]
fn two_switches_may_reverse_a_dependency_between_them_in_one_instant() {
    let (steps, samples) = every_order(|order| {
        let (mut graph, edge) = Runtime::build(|b| {
            let (sel, sel_in) = b.input::<()>();
            let sel = sel.share(b);
            let x = b.constant(1u32);
            let q = b.constant(2u32);
            let (a_forward, a_loop) = b.cell_loop::<u32>();
            let p = a_forward.map_cell(b, |v| v + 10);
            let b_outer = sel.map(move |_| q).hold(b, p);
            b.depends(&b_outer, &[&q]);
            let b_switch = b_outer.switch_cell(b);
            let y = b_switch.map_cell(b, |v| v + 100);
            let a_outer = sel.map(move |_| y).hold(b, x);
            b.depends(&a_outer, &[&y]);
            let a_switch = a_outer.switch_cell(b);
            a_loop.close(b, a_switch);
            let logs = [log_steps(b, a_switch), log_steps(b, b_switch)];
            (sel_in, logs, [a_switch, b_switch, p, y])
        });
        let (sel_in, logs, cells) = edge.keep();
        let schedule = [vec![send(sel_in, ())], vec![]];
        drive(&mut graph, order, &schedule, &logs, &cells)
    });
    assert_eq!(steps[0], [(0, 1), (1, 102)]);
    assert_eq!(steps[1], [(0, 11), (1, 2)]);
    assert_eq!(
        samples,
        [[1, 102, 102], [11, 2, 2], [11, 112, 112], [111, 102, 102]]
    );
}

/// Relink's first pass reads the committed selection of every switch the
/// instant queued, and moves it; its second pass checks the moves for
/// cycles. At [1] the middle of a switch of a switch selects a map_cell
/// computed from the top switch, a cycle the second pass would refuse; the
/// first pass reads the top's new selection through the middle's and goes
/// around it, which recursed until the stack overflowed and aborted the
/// process, in every order. The read's cycle detection over the switches
/// it passes meets the middle one again and panics, which poisons the
/// graph.
#[test]
fn a_read_in_relink_around_a_cycle_its_check_would_refuse_panics_and_poisons() {
    for seed in SEEDS {
        let (mut graph, edge) = Runtime::build(|b| {
            let (sel, sel_in) = b.input::<()>(); // node 1
            let one = b.constant(1u32); // node 2
            let first = b.constant(one); // node 3
            let (forward, closer) = b.cell_loop::<u32>(); // node 4
            let computed = forward.map_cell(b, move |_| one); // node 5
            let outer = sel.map(move |_| computed).hold(b, first); // node 6
            b.depends(&outer, &[&computed]);
            let middle = outer.switch_cell(b); // node 7
            let top = middle.switch_cell(b); // node 8
            closer.close(b, top);
            (sel_in, top)
        });
        let (sel_in, top) = edge.keep();
        graph.set_shuffle_seed(seed);
        assert_eq!(*graph.sample(top), 1);
        let message = panic_message(|| graph.send(sel_in, ()));
        assert!(
            message.contains(
                "a same-instant cycle through a switch_cell read before its first link or its \
                 move at commit, at node 7"
            ),
            "{message}"
        );
        assert_poisoned(&mut graph, sel_in, top);
    }
}

/// A switch whose first link would close a cycle is refused where it links,
/// at the end of the build.
#[test]
fn a_first_link_that_closes_a_cycle_is_refused_in_the_build() {
    let message = panic_message(|| {
        Runtime::build(|b| {
            let (c, closer) = b.cell_loop::<u32>(); // node 1
            let m = c.map_cell(b, |v| v + 1); // node 2
            let outer = b.constant(m); // node 3
            let sw = outer.switch_cell(b); // node 4
            closer.close(b, sw);
        })
    });
    assert!(
        message.contains(
            "switching closes a same-instant cycle: node 4 (SwitchCell) -> node 1 (Loop) -> \
             node 2 (ReadThrough) -> node 4"
        ),
        "{message}"
    );
}

/// A switch_stream that comes to follow a stream computed from its own
/// events: at the instant after the selection its events would be a
/// function of themselves. Its selection is not a dependency, so the
/// selection instant runs, and relink's check at commit refuses the move,
/// naming the cycle, and poisons the graph. A switch_stream built to follow
/// such a stream from the start is refused where it first links, in the
/// build.
#[test]
fn a_switch_stream_that_selects_a_stream_computed_from_itself_is_refused() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (x, x_in) = b.input::<u32>(); // node 1
        let x = x.share(b); // node 2
        let (sel, sel_in) = b.input::<()>(); // node 3
        let (outer, closer) = b.cell_loop::<Shared<u32>>(); // node 4
        let out = outer.switch_stream(b).share(b); // nodes 5 and 6
        let derived = out.map(|v| v + 1).share(b); // node 7
        let definition = sel.map(move |_| derived).hold(b, x); // node 8
        b.depends(&definition, &[&derived]);
        closer.close(b, definition);
        let total = out.accumulate(b, 0u32, |v, t| t + v);
        (x_in, sel_in, total)
    });
    let (x_in, sel_in, total) = edge.keep();
    graph.send(x_in, 5);
    assert_eq!(*graph.sample(total), 5);
    let message = panic_message(|| graph.send(sel_in, ()));
    assert!(
        message.contains(
            "switching closes a same-instant cycle: node 5 (SwitchStream) -> node 6 (Stream) -> \
             node 7 (Stream) -> node 5"
        ),
        "{message}"
    );
    assert_eq!(graph.try_send(x_in, 1), Err(SendError::Poisoned));

    let at_build = panic_message(|| {
        Runtime::build(|b| {
            let (outer, closer) = b.cell_loop::<Shared<u32>>(); // node 1
            let out = outer.switch_stream(b).share(b); // nodes 2 and 3
            let derived = out.map(|v| v + 1).share(b); // node 4
            let definition = b.constant(derived); // node 5
            closer.close(b, definition);
        })
    });
    assert!(
        at_build.contains(
            "switching closes a same-instant cycle: node 2 (SwitchStream) -> node 3 (Stream) -> \
             node 4 (Stream) -> node 2"
        ),
        "{at_build}"
    );
}

// ----------------------------------------------------------- switch_stream probes

/// The selector probe (SwitchS.hs): the outer selects z at [2], where the
/// old inner is quiet, so only the watcher reaches the switch; from [3] on
/// z's events come through. An outer that was only reach would never move
/// the switch. Stage5.hs:
///
/// ```text
/// probe selector: [([1],'a'),([3],'Y'),([4],'Z')]
/// ```
#[test]
fn switch_stream_relinks_on_a_selector_step_while_the_old_inner_is_quiet() {
    let (events, relinks) = every_order(|order| {
        let (mut graph, edge) = Runtime::build(|b| {
            let (a, a_in) = b.input::<char>();
            let (z, z_in) = b.input::<char>();
            let (sel, sel_in) = b.input::<()>();
            let (a, z) = (a.share(b), z.share(b));
            let outer = sel.map(move |_| z).hold(b, a);
            b.depends(&outer, &[&z]);
            let out = outer.switch_stream(b);
            (a_in, z_in, sel_in, log_events(b, out))
        });
        let (a_in, z_in, sel_in, log) = edge.keep();
        let schedule = [
            vec![send(a_in, 'a'), send(z_in, 'X')],
            vec![send(sel_in, ())],
            vec![send(a_in, 'b'), send(z_in, 'Y')],
            vec![send(a_in, 'c'), send(z_in, 'Z')],
        ];
        let mut logs = Vec::new();
        let relinks = counted(&mut graph, |g| {
            logs = drive::<char, ()>(g, order, &schedule, &[log], &[]).0;
        });
        (logs[0].clone(), relinks.map(|[_, _, r]| r))
    });
    assert_eq!(events, [(1, 'a'), (3, 'Y'), (4, 'Z')]);
    if let Some(relinks) = relinks {
        assert_eq!(
            relinks, 1,
            "the selector step at [2] alone moved the switch"
        );
    }
}

/// The loop through the selection (SwitchS.hs): every event of the
/// switch selects, through a hold, the stream it follows from the next
/// instant on. With the outer a dependency this would be a cycle, refused
/// at close; the outer is a watcher, and the loop runs. Without construct
/// (stage 6), the five streams it can select are built beforehand: shared
/// streams held directly, and linear streams in constant cells selected
/// through a switch_cell, so a switch_stream watches a switch_cell. The
/// construct form, SwitchS.hs's own, gives the same values. Stage5.hs:
///
/// ```text
/// probe loop, shared inners held directly: Just ([([1],1),([2],2),([3],3),([4],4)],5)
/// probe loop, linear inners through a switch cell: Just ([([1],1),([2],2),([3],3),([4],4)],5)
/// probe loop with construct (SwitchS.hs): Just ([([1],1),([2],2),([3],3),([4],4)],5)
/// ```
#[test]
fn switch_stream_loop_through_its_selection() {
    for linear in [false, true] {
        let events = every_order(|order| {
            let (mut graph, edge) = Runtime::build(|b| {
                let (fwd, closer) = b.stream_loop::<u32>();
                let out = fwd.share(b);
                let (ticks, ticks_in) = b.input::<u32>();
                let ticks = ticks.share(b);
                let switched = if linear {
                    let cells: Vec<Cell<Stream<u32>>> = (0..5u32)
                        .map(|k| {
                            let s = ticks.map(move |t| t + k).node(b);
                            b.constant(s)
                        })
                        .collect();
                    let first = cells[0];
                    // The closure selects from the table it captures, so the
                    // hold declares the table's cells.
                    let table = cells.clone();
                    let current = out.map(move |v| cells[v as usize]).hold(b, first);
                    b.depends(&current, &[&table]);
                    let sw = current.switch_cell(b);
                    sw.switch_stream(b)
                } else {
                    let streams: Vec<Shared<u32>> = (0..5u32)
                        .map(|k| ticks.map(move |t| t + k).share(b))
                        .collect();
                    let first = streams[0];
                    let table = streams.clone();
                    let current = out.map(move |v| streams[v as usize]).hold(b, first);
                    b.depends(&current, &[&table]);
                    current.switch_stream(b)
                };
                closer.close(b, switched);
                let out = out.node(b);
                (ticks_in, log_events(b, out))
            });
            let (ticks_in, log) = edge.keep();
            let schedule: Vec<Vec<Send>> = (0..4).map(|_| vec![send(ticks_in, 1)]).collect();
            let (logs, _) = drive::<u32, ()>(&mut graph, order, &schedule, &[log], &[]);
            logs
        });
        assert_eq!(
            events[0],
            [(1, 1), (2, 2), (3, 3), (4, 4)],
            "linear = {linear}"
        );
    }
}

/// A switch_stream's outer may be a cell loop's forward, whose definition
/// marking reaches, or a map_cell, which settles; each watches for the
/// switch. The selector probe again, switching back at [4]. Stage5.hs:
///
/// ```text
/// other outers: through a loop forward: [([1],'a'),([3],'Y'),([4],'Z'),([5],'d')]
/// other outers: through a map cell: [([1],'a'),([3],'Y'),([4],'Z'),([5],'d')]
/// ```
#[test]
fn a_switch_stream_over_a_loop_forward_or_a_map_cell_follows_its_selection() {
    for through_loop in [true, false] {
        let events = every_order(|order| {
            let (mut graph, edge) = Runtime::build(|b| {
                let (a, a_in) = b.input::<char>();
                let (z, z_in) = b.input::<char>();
                let (a, z) = (a.share(b), z.share(b));
                let (pick, pick_in) = b.input::<bool>();
                let out = if through_loop {
                    let (forward, closer) = b.cell_loop::<Shared<char>>();
                    let out = forward.switch_stream(b);
                    let def = pick.map(move |p| if p { z } else { a }).hold(b, a);
                    b.depends(&def, &[&z, &a]);
                    closer.close(b, def);
                    out
                } else {
                    let picked = pick.hold(b, false);
                    let outer = picked.map_cell(b, move |p| if *p { z } else { a });
                    b.depends(&outer, &[&z, &a]);
                    outer.switch_stream(b)
                };
                (a_in, z_in, pick_in, log_events(b, out))
            });
            let (a_in, z_in, pick_in, log) = edge.keep();
            let both = |x, y| vec![send(a_in, x), send(z_in, y)];
            let schedule = [
                both('a', 'X'),
                vec![send(pick_in, true)],
                both('b', 'Y'),
                {
                    let mut s = both('c', 'Z');
                    s.push(send(pick_in, false));
                    s
                },
                both('d', 'W'),
            ];
            let (logs, _) = drive::<char, ()>(&mut graph, order, &schedule, &[log], &[]);
            logs
        });
        assert_eq!(
            events[0],
            [(1, 'a'), (3, 'Y'), (4, 'Z'), (5, 'd')],
            "through a loop: {through_loop}"
        );
    }
}

/// A switch_stream whose outer steps at [0], its creation instant: nothing
/// marks in transaction zero, so the relink the switch queues at its first
/// evaluation is what moves it. At [0] it follows the inner selected
/// before [0], whose event there comes through, and from [1] on the new
/// one. Both inners fire at [0] through steps_with_current of a constant.
/// Stage5.hs:
///
/// ```text
/// stream creation: out: [([0],'p'),([1],'X'),([2],'Y')]
/// ```
#[test]
fn a_switch_stream_whose_outer_steps_at_its_creation_moves_at_that_commit() {
    let events = every_order(|order| {
        let (mut graph, edge) = Runtime::build(|b| {
            let p = b.constant('p').steps_with_current(b);
            let (a, a_in) = b.input::<char>();
            let a = p.or_else(b, a).share(b);
            let q = b.constant('q').steps_with_current(b);
            let (z, z_in) = b.input::<char>();
            let z = q.or_else(b, z).share(b);
            let outer = b
                .constant(())
                .steps_with_current(b)
                .map(move |_| z)
                .hold(b, a);
            let out = outer.switch_stream(b);
            (a_in, z_in, log_events(b, out))
        });
        let (a_in, z_in, log) = edge.keep();
        let schedule = [
            vec![send(a_in, 'a'), send(z_in, 'X')],
            vec![send(a_in, 'b'), send(z_in, 'Y')],
        ];
        drive::<char, ()>(&mut graph, order, &schedule, &[log], &[]).0
    });
    assert_eq!(events[0], [(0, 'p'), (1, 'X'), (2, 'Y')]);
}

/// A switch_stream built before the stream it first follows: its outer is
/// a loop's forward, closed with a constant holding a stream built later,
/// which fires at [0]. The switch's first evaluation, in the build's pull
/// of new nodes in creation order, links that stream and runs it at once,
/// since it was no dependency when the switch's were run, so its event at
/// [0] comes through. Stage5.hs:
///
/// ```text
/// later inner: out: [([0],'p'),([1],'x'),([2],'y')]
/// ```
#[test]
fn a_switch_stream_runs_the_inner_it_first_links_at_its_creation() {
    let events = every_order(|order| {
        let (mut graph, edge) = Runtime::build(|b| {
            let (outer, closer) = b.cell_loop::<Shared<char>>();
            let out = outer.switch_stream(b);
            let log = log_events(b, out);
            let (x, x_in) = b.input::<char>();
            let p = b.constant('p').steps_with_current(b);
            let p = p.or_else(b, x).share(b);
            let definition = b.constant(p);
            closer.close(b, definition);
            (x_in, log)
        });
        let (x_in, log) = edge.keep();
        let schedule = [vec![send(x_in, 'x')], vec![send(x_in, 'y')]];
        drive::<char, ()>(&mut graph, order, &schedule, &[log], &[]).0
    });
    assert_eq!(events[0], [(0, 'p'), (1, 'x'), (2, 'y')]);
}

/// A switch_stream whose new inner fires at the switch instant: the old
/// inner's event is forwarded there and the new one's is not; after a
/// switch back at [4], z's Z at [5] is not the switch's. Over shared
/// streams held directly, and over linear streams in constant cells
/// selected through a switch_cell. Stage5.hs:
///
/// ```text
/// new inner fires: out: [([1],'a'),([2],'b'),([3],'Y')]
/// new inner fires: linear, through a switch cell: [([1],'a'),([2],'b'),([3],'Y')]
/// ```
#[test]
fn a_switch_stream_forwards_the_old_inner_at_the_switch_instant() {
    for linear in [false, true] {
        let events = every_order(|order| {
            let (mut graph, edge) = Runtime::build(|b| {
                let (a, a_in) = b.input::<char>();
                let (z, z_in) = b.input::<char>();
                let (pick, pick_in) = b.input::<bool>();
                let out = if linear {
                    let (ca, cz) = (b.constant(a), b.constant(z));
                    let outer = pick.map(move |p| if p { cz } else { ca }).hold(b, ca);
                    b.depends(&outer, &[&cz, &ca]);
                    let sw = outer.switch_cell(b);
                    sw.switch_stream(b)
                } else {
                    let (a, z) = (a.share(b), z.share(b));
                    let outer = pick.map(move |p| if p { z } else { a }).hold(b, a);
                    b.depends(&outer, &[&z, &a]);
                    outer.switch_stream(b)
                };
                (a_in, z_in, pick_in, log_events(b, out))
            });
            let (a_in, z_in, pick_in, log) = edge.keep();
            let schedule = [
                vec![send(a_in, 'a'), send(z_in, 'W')],
                vec![send(a_in, 'b'), send(z_in, 'X'), send(pick_in, true)],
                vec![send(a_in, 'c'), send(z_in, 'Y')],
                vec![send(a_in, 'd'), send(pick_in, false)],
                vec![send(z_in, 'Z')],
            ];
            let (logs, _) = drive::<char, ()>(&mut graph, order, &schedule, &[log], &[]);
            logs
        });
        assert_eq!(
            events[0],
            [(1, 'a'), (2, 'b'), (3, 'Y')],
            "linear = {linear}"
        );
    }
}

// ----------------------------------------------------------- switch_cell steps

/// A switch to a quiet inner steps with the inner's value; the old inner's
/// step at the switch instant is dropped; a switch back to an inner that
/// steps at that instant carries the step. A map_cell over the switch
/// settles exactly: it steps whenever the switch does, quiet inner
/// included. Stage5.hs:
///
/// ```text
/// quiet: steps sw: (1,[([0],1),([1],5),([2],2),([3],30),([4],8)])
/// quiet: samples: [1,5,2,30,8]
/// quiet: steps (map_cell (*10) sw): (10,[([0],10),([1],50),([2],20),([3],300),([4],80)])
/// ```
#[test]
fn a_switch_cell_that_switches_to_a_quiet_inner_steps() {
    let (steps, samples) = every_order(|order| {
        let (mut graph, edge) = Runtime::build(|b| {
            let (hx, x_in) = b.input_cell(1u32);
            let (hy, y_in) = b.input_cell(2u32);
            let (sel, sel_in) = b.input::<bool>();
            let outer = sel.map(move |s| if s { hy } else { hx }).hold(b, hx);
            b.depends(&outer, &[&hy, &hx]);
            let sw = outer.switch_cell(b);
            let tens = sw.map_cell(b, |v| v * 10);
            let logs = [log_steps(b, sw), log_steps(b, tens)];
            (x_in, y_in, sel_in, logs, sw)
        });
        let (x_in, y_in, sel_in, logs, sw) = edge.keep();
        let schedule = [
            vec![send(x_in, 5)],
            vec![send(x_in, 6), send(sel_in, true)],
            vec![send(x_in, 7), send(y_in, 30)],
            vec![send(x_in, 8), send(sel_in, false)],
        ];
        drive(&mut graph, order, &schedule, &logs, &[sw])
    });
    assert_eq!(steps[0], [(0, 1), (1, 5), (2, 2), (3, 30), (4, 8)]);
    assert_eq!(steps[1], [(0, 10), (1, 50), (2, 20), (3, 300), (4, 80)]);
    assert_eq!(samples[0], [1, 5, 2, 30, 8]);
}

/// A switch_cell whose outer is a map_cell of a boolean: at a switch
/// instant the steps view prepares the map_cell, which computes its value
/// after the instant, and reads the new inner through it; commit promotes
/// that value into the map_cell's memo, and the relink reads the memo. So
/// the map_cell's function runs once at the first link and once per step
/// of its input, three times in all. Each new inner steps at its switch
/// instant. Stage5.hs:
///
/// ```text
/// map outer: steps sw: (1,[([0],1),([1],2),([2],20),([3],30),([4],5)])
/// ```
#[test]
fn a_switch_cell_whose_outer_is_a_map_cell_reads_it_after_the_instant() {
    let (steps, calls) = every_order(|order| {
        let (calls, c) = counter();
        let (mut graph, edge) = Runtime::build(move |b| {
            let (pick, pick_in) = b.input_cell(false);
            let (c1, c1_in) = b.input_cell(1u32);
            let (c2, c2_in) = b.input_cell(10u32);
            let outer = pick.map_cell(b, move |p| {
                c.set(c.get() + 1);
                if *p { c2 } else { c1 }
            });
            b.depends(&outer, &[&c2, &c1]);
            let sw = outer.switch_cell(b);
            (pick_in, c1_in, c2_in, log_steps(b, sw))
        });
        let (pick_in, c1_in, c2_in, log) = edge.keep();
        let schedule = [
            vec![send(c1_in, 2)],
            vec![send(c1_in, 3), send(c2_in, 20), send(pick_in, true)],
            vec![send(c1_in, 4), send(c2_in, 30)],
            vec![send(c1_in, 5), send(pick_in, false)],
        ];
        let (logs, _) = drive::<u32, ()>(&mut graph, order, &schedule, &[log], &[]);
        (logs, calls.get())
    });
    assert_eq!(steps[0], [(0, 1), (1, 2), (2, 20), (3, 30), (4, 5)]);
    assert_eq!(calls, 3);
}

/// The outer steps to the inner the switch already follows: still a step
/// of the switch. Stage5.hs:
///
/// ```text
/// same inner: steps sw: (1,[([0],1),([1],5),([2],5),([3],6)])
/// ```
#[test]
fn a_switch_cell_steps_when_its_outer_selects_the_inner_it_follows() {
    let steps = every_order(|order| {
        let (mut graph, edge) = Runtime::build(|b| {
            let (hx, x_in) = b.input_cell(1u32);
            let (sel, sel_in) = b.input::<()>();
            let outer = sel.map(move |_| hx).hold(b, hx);
            let sw = outer.switch_cell(b);
            (x_in, sel_in, log_steps(b, sw))
        });
        let (x_in, sel_in, log) = edge.keep();
        let schedule = [
            vec![send(x_in, 5)],
            vec![send(sel_in, ())],
            vec![send(x_in, 6), send(sel_in, ())],
        ];
        drive::<u32, ()>(&mut graph, order, &schedule, &[log], &[]).0
    });
    assert_eq!(steps[0], [(0, 1), (1, 5), (2, 5), (3, 6)]);
}

/// A switch steps at its creation instant: its steps view fires at [0]
/// with the value it starts with, a hold over the view starts from it, and
/// steps_with_current fires once there. A cell loop's forward closed with
/// the switch steps with it, at [0] too. A switch whose outer steps at
/// [0], a hold over steps_with_current of a constant, starts from the old
/// inner and steps to the new one there. Stage5.hs:
///
/// ```text
/// creation: steps sw: (4,[([0],4),([2],9),([3],5)])
/// creation: value sw [0]: [([0],4),([2],9),([3],5)]
/// creation: hold 99 (updates sw): [4,4,9,5]
/// creation: outer steps at [0]: steps: (4,[([0],6)])
/// creation: outer steps at [0]: samples: [6,6,6,6]
/// ```
#[test]
fn a_switch_cell_steps_at_its_creation() {
    let (steps, samples) = every_order(|order| {
        let (mut graph, edge) = Runtime::build(|b| {
            let (hx, x_in) = b.input_cell(4u32);
            let (sel, sel_in) = b.input::<()>();
            let five = b.constant(5u32);
            let outer = sel.map(move |_| five).hold(b, hx);
            b.depends(&outer, &[&five]);
            let sw = outer.switch_cell(b);
            let (forward, closer) = b.cell_loop::<u32>();
            let forward_log = log_steps(b, forward);
            closer.close(b, sw);
            let current = sw.steps_with_current(b);
            let current_log = log_events(b, current);
            let held = sw.steps(b).hold(b, 99u32);
            let six = b.constant(6u32);
            let early_outer = b
                .constant(())
                .steps_with_current(b)
                .map(move |_| six)
                .hold(b, hx);
            let early = early_outer.switch_cell(b);
            let logs = [
                log_steps(b, sw),
                current_log,
                forward_log,
                log_steps(b, early),
            ];
            (x_in, sel_in, logs, [held, early])
        });
        let (x_in, sel_in, logs, cells) = edge.keep();
        let schedule = [vec![], vec![send(x_in, 9)], vec![send(sel_in, ())]];
        drive(&mut graph, order, &schedule, &logs, &cells)
    });
    assert_eq!(steps[0], [(0, 4), (2, 9), (3, 5)]);
    assert_eq!(steps[1], [(0, 4), (2, 9), (3, 5)], "steps_with_current");
    assert_eq!(steps[2], [(0, 4), (2, 9), (3, 5)], "the loop's forward");
    assert_eq!(steps[3], [(0, 6)], "the outer stepped at [0]");
    assert_eq!(samples[0], [4, 4, 9, 5]);
    assert_eq!(samples[1], [6, 6, 6, 6]);
}

/// Nested switches: a switch_cell between two switch_cells, each between
/// two leaves; and a switch of a switch, over an outer holding cells of
/// cells. Leaves, middles and the top move in one instant and apart, over
/// six transactions with up to seven sends each, in both send orders and
/// every seed. At [3] the top moves to `sb`, which moves to a quiet leaf in
/// the same instant; at [6] everything moves and every leaf steps.
/// Stage5.hs:
///
/// ```text
/// nested: steps top: (10,[([0],10),([1],11),([2],22),([3],40),([4],23),([5],15),([6],36)])
/// nested: samples top: [10,11,22,40,23,15,36]
/// nested: steps twice: (10,[([0],10),([1],11),([2],22),([3],40),([4],23),([5],15),([6],36)])
/// nested: samples twice: [10,11,22,40,23,15,36]
/// ```
#[test]
fn nested_switches_in_every_send_order() {
    let (steps, samples) = every_order(|order| {
        let (mut graph, edge) = Runtime::build(|b| {
            let (l1, l1_in) = b.input_cell(10u32);
            let (l2, l2_in) = b.input_cell(20u32);
            let (l3, l3_in) = b.input_cell(30u32);
            let (l4, l4_in) = b.input_cell(40u32);
            let (pick_a, pick_a_in) = b.input::<bool>();
            let (pick_b, pick_b_in) = b.input::<bool>();
            let (pick_top, pick_top_in) = b.input::<bool>();
            let pick_a = pick_a.share(b);
            let pick_b = pick_b.share(b);
            let pick_top = pick_top.share(b);
            let oa = pick_a.map(move |p| if p { l2 } else { l1 }).hold(b, l1);
            let ob = pick_b.map(move |p| if p { l4 } else { l3 }).hold(b, l3);
            b.depends(&oa, &[&l2, &l1]);
            b.depends(&ob, &[&l4, &l3]);
            let sa = oa.switch_cell(b);
            let sb = ob.switch_cell(b);
            let ot = pick_top.map(move |p| if p { sb } else { sa }).hold(b, sa);
            b.depends(&ot, &[&sb, &sa]);
            let top = ot.switch_cell(b);
            let oo = pick_top.map(move |p| if p { ob } else { oa }).hold(b, oa);
            b.depends(&oo, &[&ob, &oa]);
            let middle = oo.switch_cell(b);
            let twice = middle.switch_cell(b);
            (
                [l1_in, l2_in, l3_in, l4_in],
                [pick_a_in, pick_b_in, pick_top_in],
                [log_steps(b, top), log_steps(b, twice)],
                [top, twice],
            )
        });
        let (leaves, picks, logs, cells) = edge.keep();
        let leaf_sends: [&[(usize, u32)]; 6] = [
            &[(0, 11)],
            &[(1, 22)],
            &[(0, 13), (1, 23)],
            &[(2, 34)],
            &[(0, 15), (3, 45)],
            &[(0, 16), (1, 26), (2, 36), (3, 46)],
        ];
        let pick_sends: [&[(usize, bool)]; 6] = [
            &[],
            &[(0, true)],
            &[(1, true), (2, true)],
            &[(2, false)],
            &[(0, false)],
            &[(0, true), (1, false), (2, true)],
        ];
        let schedule: Vec<Vec<Send>> = leaf_sends
            .iter()
            .zip(pick_sends)
            .map(|(ls, ps)| {
                let mut sends: Vec<Send> = ls.iter().map(|&(i, v)| send(leaves[i], v)).collect();
                sends.extend(ps.iter().map(|&(i, v)| send(picks[i], v)));
                sends
            })
            .collect();
        drive(&mut graph, order, &schedule, &logs, &cells)
    });
    let expected = [
        (0, 10),
        (1, 11),
        (2, 22),
        (3, 40),
        (4, 23),
        (5, 15),
        (6, 36),
    ];
    assert_eq!(steps[0], expected);
    assert_eq!(steps[1], expected, "a switch of a switch");
    assert_eq!(samples[0], [10, 11, 22, 40, 23, 15, 36]);
    assert_eq!(samples[1], samples[0]);
}

/// The read after a switch instant through a memo: the new inner is a
/// map_cell over a hold that steps at that instant. The steps view pulls
/// the map_cell and the hold when x is sent first, computes the map_cell's
/// value after the instant from the hold's, and commit promotes it into
/// the memo, so a sample reads it without calling the function again: one
/// call per step. Stage5.hs:
///
/// ```text
/// through memo: steps sw: (1,[([0],1),([1],70),([2],80),([3],90)])
/// through memo: samples: [1,70,80,90]
/// ```
#[test]
fn a_switch_to_a_map_cell_over_a_hold_stepping_at_the_switch_instant() {
    let program = |x_first: bool, seed: Option<u64>| {
        let (calls, c) = counter();
        let (mut graph, edge) = Runtime::build(move |b| {
            let (x, x_in) = b.input::<u32>();
            let (sel, sel_in) = b.input::<()>();
            let h = x.hold(b, 0u32);
            let m = h.map_cell(b, move |v| {
                c.set(c.get() + 1);
                v * 10
            });
            let one = b.constant(1u32);
            let outer = sel.map(move |_| m).hold(b, one);
            b.depends(&outer, &[&m]);
            let sw = outer.switch_cell(b);
            (x_in, sel_in, log_steps(b, sw), sw)
        });
        let (x_in, sel_in, log, sw) = edge.keep();
        graph.set_shuffle_seed(seed);
        let switch = counted(&mut graph, |g| {
            g.transaction(|tx| {
                if x_first {
                    tx.send(x_in, 7);
                    tx.send(sel_in, ());
                } else {
                    tx.send(sel_in, ());
                    tx.send(x_in, 7);
                }
            })
        });
        let mut samples = vec![*graph.sample(sw)];
        let calls_at_switch = calls.get();
        for v in [8, 9] {
            graph.send(x_in, v);
            samples.push(*graph.sample(sw));
        }
        let steps = graph.sample(log).clone();
        (steps, samples, calls_at_switch, calls.get(), switch)
    };
    for x_first in [true, false] {
        let (steps, samples, at_switch, calls, switch) = program(x_first, None);
        assert_eq!(steps, [1, 70, 80, 90]);
        assert_eq!(samples, [70, 80, 90]);
        assert_eq!((at_switch, calls), (1, 3), "one call per step");
        if let Some([pulls, _, _]) = switch {
            assert_eq!(pulls, if x_first { 2 } else { 0 }, "x_first = {x_first}");
        }
        for seed in &SEEDS[1..] {
            let (s, sa, a, c, _) = program(x_first, *seed);
            assert_eq!(
                (s, sa, a, c),
                (steps.clone(), samples.clone(), at_switch, calls)
            );
        }
    }
}

/// Switches whose outers step in child instants: a hold of a split. Each
/// child is a switch instant, whose commit moves the switch before the
/// next child. The switch_cell steps at [1,0] to c2, which stepped at [1],
/// and at [1,1] to a constant; at [2] c1 steps while deselected, and [2,0]
/// selects it. The switch_stream's selections and inners are splits of
/// one instant too, so child n carries element n of each: at each child it
/// forwards the stream selected before that child. The engine does not
/// show child instants, so the tests place each value at its top-level
/// transaction. Stage5.hs, with F7's sorted Split:
///
/// ```text
/// children: steps sw: (1,[([0],1),([1,0],20),([1,1],3),([2,0],5),([3,0],21)])
/// children: samples: [1,3,5,21]
/// children: switch_stream: [([1,0],'a'),([1,1],'z'),([2,0],'c')]
/// ```
#[test]
fn switches_move_at_each_child_instant() {
    let (steps, samples) = every_order(|order| {
        let (mut graph, edge) = Runtime::build(|b| {
            let (c1, x1_in) = b.input_cell(1u32);
            let (c2, x2_in) = b.input_cell(2u32);
            let c3 = b.constant(3u32);
            let (lists, lists_in) = b.input::<Vec<Cell<u32>>>();
            let outer = lists.split(b).hold(b, c1);
            let sw = outer.switch_cell(b);
            (x1_in, x2_in, lists_in, log_steps(b, sw), sw, [c1, c2, c3])
        });
        let (x1_in, x2_in, lists_in, log, sw, cells) = edge.keep();
        let [c1, c2, c3] = cells;
        let lists = move |cells: Vec<Cell<u32>>| -> Send {
            Box::new(move |tx| tx.send(lists_in, cells.clone()))
        };
        let schedule = [
            vec![send(x2_in, 20), lists(vec![c2, c3])],
            vec![send(x1_in, 5), lists(vec![c1])],
            vec![send(x2_in, 21), lists(vec![c2])],
        ];
        let (logs, samples) = drive(&mut graph, order, &schedule, &[log], &[sw]);
        (logs[0].clone(), samples[0].clone())
    });
    assert_eq!(steps, [(0, 1), (1, 20), (1, 3), (2, 5), (3, 21)]);
    assert_eq!(samples, [1, 3, 5, 21]);

    let events = every_order(|order| {
        let (mut graph, edge) = Runtime::build(|b| {
            let (a, a_in) = b.input::<Vec<char>>();
            let (z, z_in) = b.input::<Vec<char>>();
            let a = a.split(b).share(b);
            let z = z.split(b).share(b);
            let (picks, picks_in) = b.input::<Vec<Shared<char>>>();
            let outer = picks.split(b).hold(b, a);
            let out = outer.switch_stream(b);
            ([a_in, z_in], picks_in, log_events(b, out), [a, z])
        });
        let (inputs, picks_in, log, streams) = edge.keep();
        let [a, z] = streams;
        let chars = move |i: usize, text: &'static str| -> Send {
            let input = inputs[i];
            Box::new(move |tx| tx.send(input, text.chars().collect()))
        };
        let picks = move |streams: Vec<Shared<char>>| -> Send {
            Box::new(move |tx| tx.send(picks_in, streams.clone()))
        };
        let schedule = [
            vec![chars(0, "ab"), chars(1, "yz"), picks(vec![z, a])],
            vec![chars(0, "c"), chars(1, "x"), picks(vec![z])],
        ];
        drive::<char, ()>(&mut graph, order, &schedule, &[log], &[]).0
    });
    assert_eq!(events[0], [(1, 'a'), (1, 'z'), (2, 'c')]);
}

/// A switch_cell over states: a State with no stream view, whose listeners
/// run on every step, at switch instants included, and a map_cell over it
/// is a State too. The step at [0] has no reader: a State has no steps
/// view, and a listener registers after the build. Stage5.hs:
///
/// ```text
/// state switch: steps current: ([],[([0],[]),([1],["ada"]),([2],["ada","grace"]),([3],["ada","bo"]),([4],["ada","bo","x"]),([5],["ada","grace","bo","x","eve"])])
/// state switch: lengths: [0,1,2,2,3,5]
/// ```
#[test]
fn a_switch_cell_over_states_steps_as_a_state() {
    let (heard, lengths) = every_order(|order| {
        let (mut graph, edge) = Runtime::build(|b| {
            let (names, names_in) = b.input::<String>();
            let names = names.share(b);
            let push = |n: String, v: &mut Vec<String>| v.push(n);
            let all = names.accumulate_mut(b, Vec::new(), push);
            let short = names
                .filter(|n| n.len() < 4)
                .accumulate_mut(b, Vec::new(), push);
            let (pick, pick_in) = b.input::<bool>();
            let outer = pick.map(move |p| if p { short } else { all }).hold(b, all);
            b.depends(&outer, &[&short, &all]);
            let current: State<Vec<String>> = outer.switch_cell(b);
            let lengths: State<usize> = current.map_cell(b, |v| v.len());
            (names_in, pick_in, current, lengths)
        });
        let (names_in, pick_in, current, lengths) = edge.keep();
        graph.set_shuffle_seed(order.seed);
        let instant = Rc::new(StdCell::new(0usize));
        let (heard, mut on) = recorder();
        let now = instant.clone();
        graph
            .listen_steps(current, move |v| on((now.get(), v.join(" "))))
            .keep();
        let mut seen_lengths = vec![*graph.sample(lengths)];
        let names = ["ada", "grace", "bo", "x", "eve"];
        let picks = [None, None, Some(true), None, Some(false)];
        for (k, (name, pick)) in names.iter().zip(picks).enumerate() {
            instant.set(k + 1);
            let mut sends = vec![send_string(names_in, name)];
            if let Some(p) = pick {
                sends.push(send(pick_in, p));
            }
            run(&mut graph, &sends, order.reversed);
            seen_lengths.push(*graph.sample(lengths));
        }
        (heard.take(), seen_lengths)
    });
    assert_eq!(
        heard,
        [
            (1, "ada".to_string()),
            (2, "ada grace".to_string()),
            (3, "ada bo".to_string()),
            (4, "ada bo x".to_string()),
            (5, "ada grace bo x eve".to_string())
        ]
    );
    assert_eq!(lengths, [0, 1, 2, 2, 3, 5]);
}

fn send_string(input: Input<String>, value: &'static str) -> Send {
    Box::new(move |tx| tx.send(input, value.to_string()))
}

// ----------------------------------------------------------- one switch per cell of linear streams

/// A linear stream has one consumer, so a cell holding linear streams may
/// have one switch_stream: a second over the same cell panics where it is
/// built.
#[test]
fn a_second_switch_stream_over_a_cell_of_linear_streams_is_refused() {
    let message = panic_message(|| {
        Runtime::build(|b| {
            let (clicks, _clicks_in) = b.input::<u32>();
            let current = b.constant(clicks);
            let _first = current.switch_stream(b);
            let _second = current.switch_stream(b);
        })
    });
    assert!(
        message.contains("a cell holding linear streams may have exactly one switch_stream"),
        "{message}"
    );
}

/// A cell loop's forward and its definition are one cell: a switch_stream
/// on each is refused where the loop closes, or where the second is built
/// when the loop is already closed, through a forward closed with another
/// forward too.
#[test]
fn a_switch_stream_on_a_loop_forward_and_one_on_its_definition_are_refused() {
    let at_close = panic_message(|| {
        Runtime::build(|b| {
            let (forward, closer) = b.cell_loop::<Stream<u32>>();
            let _on_forward = forward.switch_stream(b);
            let (clicks, _clicks_in) = b.input::<u32>();
            let definition = b.constant(clicks);
            let _on_definition = definition.switch_stream(b);
            closer.close(b, definition);
        })
    });
    assert!(at_close.contains("two switch_streams"), "{at_close}");
    let after_close = panic_message(|| {
        Runtime::build(|b| {
            let (f1, closer1) = b.cell_loop::<Stream<u32>>();
            let (f2, closer2) = b.cell_loop::<Stream<u32>>();
            let _on_f1 = f1.switch_stream(b);
            closer1.close(b, f2);
            let (clicks, _clicks_in) = b.input::<u32>();
            let definition = b.constant(clicks);
            closer2.close(b, definition);
            let _on_definition = definition.switch_stream(b);
        })
    });
    assert!(
        after_close.contains("exactly one switch_stream"),
        "{after_close}"
    );
}

/// Shared streams may have any number of switches over them.
#[test]
fn a_cell_of_shared_streams_may_have_several_switch_streams() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (clicks, clicks_in) = b.input::<u32>();
        let clicks = clicks.share(b);
        let current = b.constant(clicks);
        (
            clicks_in,
            current.switch_stream(b),
            current.switch_stream(b),
        )
    });
    let (clicks_in, first, second) = edge.keep();
    let (heard_first, on_first) = recorder();
    let (heard_second, on_second) = recorder();
    graph.listen(first, on_first).keep();
    graph.listen(second, on_second).keep();
    graph.send(clicks_in, 4);
    assert_eq!(*heard_first.borrow(), [4]);
    assert_eq!(*heard_second.borrow(), [4]);
}

/// The run-time backstop: a switch_cell selects, at [2], the cell whose
/// linear stream another switch_stream takes from, which no build-time
/// check can see. The switch_stream over the switch_cell links it at
/// commit and panics, which poisons the graph.
#[test]
fn a_switch_cell_selecting_a_linear_stream_that_has_a_switch_stream_panics_and_poisons() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (s1, s1_in) = b.input::<u32>(); // node 1
        let c1 = b.constant(s1); // node 2
        let direct = c1.switch_stream(b); // node 3
        let (s2, _s2_in) = b.input::<u32>();
        let c2 = b.constant(s2);
        let (sel, sel_in) = b.input::<()>();
        let outer = sel.map(move |_| c1).hold(b, c2);
        let via = outer.switch_cell(b);
        let via = via.switch_stream(b); // node 9
        let taken = log_events(b, via);
        (s1_in, sel_in, taken, direct)
    });
    let (s1_in, sel_in, taken, direct) = edge.keep();
    let (heard, on) = recorder();
    graph.listen(direct, on).keep();
    graph.send(s1_in, 1);
    assert_eq!(*heard.borrow(), [1]);
    let message = panic_message(|| graph.send(sel_in, ()));
    assert!(
        message.contains(
            "a linear stream selected by a second switch_stream: the switch_stream at node 9 \
             selected the stream at node 1, which the switch_stream at node 3 takes from"
        ),
        "{message}"
    );
    assert_eq!(graph.try_send(s1_in, 2), Err(SendError::Poisoned));
    assert_eq!(graph.try_sample(taken).err(), Some(TokenError::Poisoned));
}

/// The backstop at a first link: the switch_cell selects that cell from
/// the start, and the build panics.
#[test]
fn a_first_link_to_a_linear_stream_that_has_a_switch_stream_panics_in_the_build() {
    let message = panic_message(|| {
        Runtime::build(|b| {
            let (s1, _s1_in) = b.input::<u32>();
            let c1 = b.constant(s1);
            let _direct = c1.switch_stream(b);
            let outer = b.constant(c1);
            let via = outer.switch_cell(b);
            let _via = via.switch_stream(b);
        })
    });
    assert!(
        message.contains("a linear stream selected by a second switch_stream"),
        "{message}"
    );
}

/// Two switch_streams trade linear streams in one instant: one moves off
/// s1 as the other moves onto it. Relink gives up every claim before it
/// makes any, so the trade is accepted in whatever order the switches were
/// queued, which each seed and send order changes, and each forwards its
/// new stream from the next instant. Stage5.hs:
///
/// ```text
/// trade: a: [([1],11),([2],21),([3],32)]
/// trade: b: [([1],13),([2],23),([3],31)]
/// ```
#[test]
fn two_switch_streams_may_trade_linear_streams_in_one_instant() {
    let (first, second) = every_order(|order| {
        let (mut graph, edge) = Runtime::build(|b| {
            let (s1, s1_in) = b.input::<u32>();
            let (s2, s2_in) = b.input::<u32>();
            let (s3, s3_in) = b.input::<u32>();
            let (c1, c2, c3) = (b.constant(s1), b.constant(s2), b.constant(s3));
            let (sel, sel_in) = b.input::<()>();
            let sel = sel.share(b);
            let outer_a = sel.map(move |_| c2).hold(b, c1);
            let outer_b = sel.map(move |_| c1).hold(b, c3);
            b.depends(&outer_a, &[&c2]);
            b.depends(&outer_b, &[&c1]);
            let a = outer_a.switch_cell(b);
            let a = a.switch_stream(b);
            let bb = outer_b.switch_cell(b);
            let bb = bb.switch_stream(b);
            let logs = [log_events(b, a), log_events(b, bb)];
            ([s1_in, s2_in, s3_in], sel_in, logs)
        });
        let (inputs, sel_in, logs) = edge.keep();
        let all = |k: u32| -> Vec<Send> {
            inputs
                .iter()
                .enumerate()
                .map(|(i, input)| send(*input, k * 10 + i as u32 + 1))
                .collect()
        };
        let mut switch = all(2);
        switch.push(send(sel_in, ()));
        let schedule = [all(1), switch, all(3)];
        let (logs, _) = drive::<u32, ()>(&mut graph, order, &schedule, &logs, &[]);
        (logs[0].clone(), logs[1].clone())
    });
    assert_eq!(first, [(1, 11), (2, 21), (3, 32)]);
    assert_eq!(second, [(1, 13), (2, 23), (3, 31)]);
}
