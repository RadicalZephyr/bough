//! The loop shapes RFD 1 and RFD 7 promise as fixed tests: the lift2 shape
//! filed as sodium-rust#52 and its reductions, and bevy-sodium's
//! health-and-shield slice (record 0003). The programs are the research's,
//! ported from its Bough sketch, and the expected values are GHC's: the
//! research ran its `Shapes.hs` over the unchanged `Denotational.hs`, with
//! each loop solved by explicit fixed-point iteration, and a verification
//! checked every value against an independent model. Both are in
//! `bough-oracle/haskell/probes/loop-shapes/`: `loop-shapes.md` (shapes 1,
//! 2a, 2b, 3b and 3c, whose tables are quoted at each test) and
//! `verification.md`. Values the research did not print come from the same
//! program text extended in `probes/stage3/Slice.hs`:
//! the verification's edge schedule, the variant that reads every looped
//! cell through its forward token, and the two-input reduction.
//!
//! Every test runs under the plain order and several shuffle seeds, and
//! each must give GHC's values under all of them. Instant `[k]` is the k-th
//! transaction after the build, and each instant's sends are one
//! transaction.

use std::cell::{Cell as StdCell, RefCell};
use std::fmt::Debug;
use std::rc::Rc;

use bough::{Build, Cell, CellRef, Graph, Input, Lift, Shared, Source, Trace, Tracer};

/// The plain order, then seeds for RFD 1's order shuffle.
const SEEDS: [Option<u64>; 6] = [None, Some(0), Some(1), Some(7), Some(42), Some(1 << 40)];

/// Which purely observational lifts the graph builds: the four rows of
/// sodium-rust#52.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lifts {
    None,
    Fraction,
    Effective,
    Both,
}

/// The form of health's update. `Full` is the program as bevy-sodium's
/// 0003 experiments write it; the others are the reductions (shape 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Drive {
    Full,
    NoMaxRead,
    NoMerge,
    OnlyWhenChanged,
}

/// Where a read of a looped cell from outside its own loop goes: to the
/// held cell, as the reproduction writes it, or to the loop's forward
/// token. A forward becomes its definition at close, so the values are the
/// same; the engine's path is not, since a lift over forwards depends on
/// the loop nodes, which settle after their holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reads {
    Held,
    Forward,
}

/// The edge: whatever build returns is the root set.
struct Slice {
    heal: Input<u32>,
    damage: Input<u32>,
    level_up: Input<u32>,
    max_health: Cell<u32>,
    shield: Cell<u32>,
    health: Cell<u32>,
    fraction: Option<Cell<f32>>,
    effective: Option<Cell<u32>>,
}

// Hand-written until `#[derive(Trace)]` lands.
impl Trace for Slice {
    fn trace(&self, t: &mut Tracer) {
        self.heal.trace(t);
        self.damage.trace(t);
        self.level_up.trace(t);
        self.max_health.trace(t);
        self.shield.trace(t);
        self.health.trace(t);
        self.fraction.trace(t);
        self.effective.trace(t);
    }
}

fn clamp(d: i64, cur: u32, max: u32) -> u32 {
    (cur as i64 + d).clamp(0, max as i64) as u32
}

/// The health-and-shield slice (bevy-sodium 0003 stage 2). With
/// `Drive::Full` it is the sodium-rust#52 reproduction.
fn slice(b: &mut Build, drive: Drive, lifts: Lifts, reads: Reads) -> Slice {
    let (heal_s, heal) = b.input::<u32>();
    let (damage_s, damage) = b.input::<u32>();
    let (level_up_s, level_up) = b.input::<u32>();
    // Two consumers, the shield and `took`, so the input stream is shared.
    let damage_s = damage_s.share(b);

    // max_health: level-ups accumulate, a hold through a loop.
    let (max_fwd, max_loop) = b.cell_loop::<u32>();
    let max_health = level_up_s.snapshot(max_fwd, |e, c| c + e).hold(b, 100);
    max_loop.close(b, max_health);

    // shield: absorbs damage first, a hold through a loop.
    let (shield_fwd, shield_loop) = b.cell_loop::<u32>();
    let shield = damage_s
        .snapshot(shield_fwd, |d, s| s.saturating_sub(d))
        .hold(b, 30);
    shield_loop.close(b, shield);

    let (max_read, shield_read) = match reads {
        Reads::Held => (max_health, shield),
        Reads::Forward => (max_fwd, shield_fwd),
    };

    // The damage that gets past the shield, read before the instant.
    let healed = heal_s.map(|h| h as i64);
    let took = damage_s.snapshot(shield_read, |d, s| -(d.saturating_sub(*s) as i64));

    // health: a hold through a loop whose update reads shield (through
    // took) and max_health. snapshot3 is two snapshot adapters, fused into
    // the hold's node.
    let (health_fwd, health_loop) = b.cell_loop::<u32>();
    let health = match drive {
        Drive::Full => healed
            .merge(b, took, |x, y| x + y)
            .snapshot(health_fwd, |d, cur| (d, *cur))
            .snapshot(max_read, |(d, cur), max| clamp(d, cur, *max))
            .hold(b, 60),
        Drive::NoMaxRead => healed
            .merge(b, took, |x, y| x + y)
            .snapshot(health_fwd, |d, cur| (*cur as i64 + d).max(0) as u32)
            .hold(b, 60),
        Drive::NoMerge => took
            .snapshot(health_fwd, |d, cur| (d, *cur))
            .snapshot(max_read, |(d, cur), max| clamp(d, cur, *max))
            .hold(b, 60),
        // Shape 3c: clamp, and step only when the value changes.
        Drive::OnlyWhenChanged => healed
            .merge(b, took, |x, y| x + y)
            .snapshot(health_fwd, |d, cur| (d, *cur))
            .snapshot(max_read, |(d, cur), max| {
                let new = clamp(d, cur, *max);
                (new != cur).then_some(new)
            })
            .filter_map(|o| o)
            .hold(b, 60),
    };
    health_loop.close(b, health);

    // The derived values: pure functions of two cells, read-through.
    let (lift_max, lift_health, lift_shield) = match reads {
        Reads::Held => (max_health, health, shield),
        Reads::Forward => (max_fwd, health_fwd, shield_fwd),
    };
    let fraction = matches!(lifts, Lifts::Fraction | Lifts::Both)
        .then(|| (lift_max, lift_health).lift(b, |m, h| *h as f32 / *m as f32));
    let effective = matches!(lifts, Lifts::Effective | Lifts::Both)
        .then(|| (lift_health, lift_shield).lift(b, |h, s| h + s));

    Slice {
        heal,
        damage,
        level_up,
        max_health,
        shield,
        health,
        fraction,
        effective,
    }
}

/// One instant of input: (heal, damage, level_up).
type Instant = (Option<u32>, Option<u32>, Option<u32>);

/// Record 0003's instant, the sodium-rust#52 transaction.
const RECORD_0003: [Instant; 1] = [(Some(100), Some(50), Some(100))];

/// The long schedule: instants [1] to [7].
const LONG_RUN: [Instant; 7] = [
    (None, Some(10), None),
    (Some(100), Some(50), Some(100)),
    (None, Some(30), None),
    (Some(500), None, Some(50)),
    (Some(100), None, None),
    (None, Some(300), None),
    (Some(40), None, None),
];

/// The verification's third schedule: a simultaneous level-up and damage
/// without a heal, a level-up of 0 (a step to an equal maximum), and a
/// heal far past the maximum.
const EDGE_RUN: [Instant; 4] = [
    (Some(40), None, None),
    (None, Some(30), Some(10)),
    (Some(1000), Some(5), Some(0)),
    (None, None, Some(5)),
];

/// Steps a cell took, as (instant, value).
type Steps<A> = Rc<RefCell<Vec<(usize, A)>>>;

/// Records a cell's steps with the instant they happened in.
fn record<C>(graph: &mut Graph, cell: C, now: &Rc<StdCell<usize>>) -> Steps<C::Value>
where
    C: CellRef,
    C::Value: Clone,
{
    let steps: Steps<C::Value> = Rc::default();
    let (now, log) = (now.clone(), steps.clone());
    graph
        .listen_steps(cell, move |v| log.borrow_mut().push((now.get(), v.clone())))
        .keep();
    steps
}

/// Drives a schedule, one transaction per instant.
fn drive(graph: &mut Graph, edge: &Slice, schedule: &[Instant], now: &Rc<StdCell<usize>>) {
    for (k, &(heal, damage, level_up)) in schedule.iter().enumerate() {
        now.set(k + 1);
        graph.transaction(|tx| {
            // One external cause, one transaction: all three are one instant.
            if let Some(v) = heal {
                tx.send(edge.heal, v);
            }
            if let Some(v) = damage {
                tx.send(edge.damage, v);
            }
            if let Some(v) = level_up {
                tx.send(edge.level_up, v);
            }
        });
    }
}

/// Everything one run of the slice observed.
struct Observed {
    max_health: Vec<(usize, u32)>,
    shield: Vec<(usize, u32)>,
    health: Vec<(usize, u32)>,
    fraction: Option<Vec<(usize, f32)>>,
    effective: Option<Vec<(usize, u32)>>,
    final_health: u32,
}

/// Builds the slice, sets the shuffle seed, drives the schedule and
/// collects every cell's steps.
fn observe(shape: (Drive, Lifts, Reads), schedule: &[Instant], seed: Option<u64>) -> Observed {
    let (drive_kind, lifts, reads) = shape;
    let (mut graph, edge) = Graph::build(|b| slice(b, drive_kind, lifts, reads));
    graph.set_shuffle_seed(seed);
    let now = Rc::new(StdCell::new(0));
    let max_health = record(&mut graph, edge.max_health, &now);
    let shield = record(&mut graph, edge.shield, &now);
    let health = record(&mut graph, edge.health, &now);
    let fraction = edge.fraction.map(|f| record(&mut graph, f, &now));
    let effective = edge.effective.map(|e| record(&mut graph, e, &now));
    drive(&mut graph, &edge, schedule, &now);
    let take = |s: &Steps<u32>| s.borrow().clone();
    Observed {
        max_health: take(&max_health),
        shield: take(&shield),
        health: take(&health),
        fraction: fraction.map(|f| f.borrow().clone()),
        effective: effective.map(|e| take(&e)),
        final_health: *graph.sample(edge.health),
    }
}

/// `expected` as (instant, value) pairs, for comparing with a recording.
fn at<A: Copy>(expected: &[(usize, A)]) -> Vec<(usize, A)> {
    expected.to_vec()
}

fn assert_steps<A: PartialEq + Debug>(got: &[(usize, A)], expected: &[(usize, A)], what: &str) {
    assert_eq!(got, expected, "{what}");
}

/// Shape 1, sodium-rust#52 (bevy-sodium 0003-lift2-loop-bug.rs): a lift
/// is observation only, so health is 100 in every row. sodium-rust 2.1.3
/// gives 100, 100, 60, 60. GHC (loop-shapes.md, shape 1): max_health `[1]
/// 200`, shield `[1] 0`, health `[1] 100`, fraction `[1] 0.5`, effective
/// `[1] 100`, and each lift steps once in the instant although both of its
/// inputs step there. The clamp reads max_health from before the instant,
/// so health is 100, not 140.
#[test]
fn sodium_rust_52_a_lift_does_not_change_its_inputs() {
    for seed in SEEDS {
        for reads in [Reads::Held, Reads::Forward] {
            for lifts in [Lifts::None, Lifts::Fraction, Lifts::Effective, Lifts::Both] {
                let what = format!("{lifts:?}, {reads:?}, seed {seed:?}");
                let o = observe((Drive::Full, lifts, reads), &RECORD_0003, seed);
                assert_steps(&o.max_health, &[(1, 200)], &what);
                assert_steps(&o.shield, &[(1, 0)], &what);
                assert_steps(&o.health, &[(1, 100)], &what);
                assert_eq!(o.final_health, 100, "{what}");
                if let Some(f) = o.fraction {
                    assert_steps(&f, &[(1, 0.5)], &what);
                }
                if let Some(e) = o.effective {
                    assert_steps(&e, &[(1, 100)], &what);
                }
            }
        }
    }
}

/// Shape 2b, the health-and-shield slice on the seven-instant schedule
/// (RFD 7). GHC (loop-shapes.md, shape 2b; three evaluations):
///
/// | | init | [1] | [2] | [3] | [4] | [5] | [6] | [7] |
/// |---|---|---|---|---|---|---|---|---|
/// | max_health | 100 | | 200 | | 250 | | | |
/// | shield | 30 | 20 | 0 | 0 | | | 0 | |
/// | health | 60 | 60 | 100 | 70 | 200 | 250 | 0 | 40 |
/// | fraction | 0.6 | 0.6 | 0.5 | 0.35 | 0.8 | 1.0 | 0.0 | 0.16 |
/// | effective | 90 | 80 | 100 | 70 | 200 | 250 | 0 | 40 |
///
/// Health steps to an equal value at [1], shield at [3] and [6]; at [4]
/// health is 200, not 250, because the clamp reads the maximum before it
/// steps; each lift steps once per instant, at the union of its inputs'
/// step times. Every fraction is the correctly rounded quotient, so f32
/// equality is exact.
#[test]
fn the_health_and_shield_slice_over_the_seven_instant_schedule() {
    for seed in SEEDS {
        for reads in [Reads::Held, Reads::Forward] {
            let what = format!("{reads:?}, seed {seed:?}");
            let o = observe((Drive::Full, Lifts::Both, reads), &LONG_RUN, seed);
            assert_steps(&o.max_health, &[(2, 200), (4, 250)], &what);
            assert_steps(&o.shield, &[(1, 20), (2, 0), (3, 0), (6, 0)], &what);
            let health = [
                (1, 60),
                (2, 100),
                (3, 70),
                (4, 200),
                (5, 250),
                (6, 0),
                (7, 40),
            ];
            assert_steps(&o.health, &health, &what);
            let fraction = [
                (1, 0.6),
                (2, 0.5),
                (3, 0.35),
                (4, 0.8),
                (5, 1.0),
                (6, 0.0),
                (7, 0.16),
            ];
            assert_steps(&o.fraction.unwrap(), &fraction, &what);
            let effective = [
                (1, 80),
                (2, 100),
                (3, 70),
                (4, 200),
                (5, 250),
                (6, 0),
                (7, 40),
            ];
            assert_steps(&o.effective.unwrap(), &effective, &what);
            assert_eq!(o.final_health, 40, "{what}");
        }
    }
}

/// The slice on the verification's edge schedule. GHC (Slice.hs, `edgeRun
/// Full`, and the same for `ReadsForward`): max_health `[2] 110 [3] 110 [4]
/// 115`, shield `[2] 0 [3] 0`, health `[1] 100 [2] 100 [3] 110`, fraction
/// `[1] 1.0 [2] 100/110 [3] 1.0 [4] 110/115`, effective `[1] 130 [2] 100
/// [3] 110`. At [2] the damage is absorbed and the heal is absent, so
/// health steps to an equal 100 while the maximum steps to 110; at [3] the
/// heal of 1000 clamps to the maximum from before the instant, 110, which
/// the level-up of 0 steps to an equal value.
#[test]
fn the_health_and_shield_slice_over_the_edge_schedule() {
    for seed in SEEDS {
        for reads in [Reads::Held, Reads::Forward] {
            let what = format!("{reads:?}, seed {seed:?}");
            let o = observe((Drive::Full, Lifts::Both, reads), &EDGE_RUN, seed);
            assert_steps(&o.max_health, &[(2, 110), (3, 110), (4, 115)], &what);
            assert_steps(&o.shield, &[(2, 0), (3, 0)], &what);
            assert_steps(&o.health, &[(1, 100), (2, 100), (3, 110)], &what);
            let fraction = at(&[
                (1, 100.0f32 / 100.0),
                (2, 100.0 / 110.0),
                (3, 110.0 / 110.0),
                (4, 110.0 / 115.0),
            ]);
            assert_steps(&o.fraction.unwrap(), &fraction, &what);
            assert_steps(
                &o.effective.unwrap(),
                &[(1, 130), (2, 100), (3, 110)],
                &what,
            );
        }
    }
}

/// Shape 3b, the reductions sodium-rust#52 reports, reconstructed from its
/// prose. GHC (loop-shapes.md, shape 3): at record 0003's instant NoMaxRead
/// gives health 140 and NoMerge 40, in every row; on the long schedule
/// NoMaxRead gives 60, 130, 100, 600, 700, 400, 440 at [1] to [7] (seven
/// evaluations) and NoMerge 60, 30, 0, 0 at [1], [2], [3], [6]. Upstream,
/// NoMaxRead still reproduced the bug and NoMerge did not.
#[test]
fn sodium_rust_52_reductions() {
    for seed in SEEDS {
        for lifts in [Lifts::None, Lifts::Effective, Lifts::Both] {
            for (drive_kind, health) in [(Drive::NoMaxRead, 140), (Drive::NoMerge, 40)] {
                let what = format!("{drive_kind:?}, {lifts:?}, seed {seed:?}");
                let o = observe((drive_kind, lifts, Reads::Held), &RECORD_0003, seed);
                assert_steps(&o.health, &[(1, health)], &what);
                if let Some(e) = o.effective {
                    assert_steps(&e, &[(1, health)], &what);
                }
            }
            let what = format!("{lifts:?}, seed {seed:?}");
            let o = observe((Drive::NoMaxRead, lifts, Reads::Held), &LONG_RUN, seed);
            let expected = at(&[
                (1, 60),
                (2, 130),
                (3, 100),
                (4, 600),
                (5, 700),
                (6, 400),
                (7, 440),
            ]);
            assert_steps(&o.health, &expected, &format!("NoMaxRead long, {what}"));
            let o = observe((Drive::NoMerge, lifts, Reads::Held), &LONG_RUN, seed);
            let expected = [(1, 60), (2, 30), (3, 0), (6, 0)];
            assert_steps(&o.health, &expected, &format!("NoMerge long, {what}"));
        }
    }
}

/// Shape 3c: health steps only when its value changes, a filter inside the
/// loop, so the event spine depends on the loop's values and the lazy
/// knot never returns; the fixed point needs three evaluations. GHC
/// (loop-shapes.md, shape 3c): health has no step at [1] (delta 0) and is
/// otherwise as in 2b; fraction loses its [1] step; effective keeps its
/// [1] step, from the shield. At [1] health and its loop node are marked
/// and do not step (F4), so the lifts over the forwards must not step on
/// health's account either.
#[test]
fn shape_3c_health_steps_only_when_it_changes() {
    for seed in SEEDS {
        for reads in [Reads::Held, Reads::Forward] {
            let what = format!("{reads:?}, seed {seed:?}");
            let o = observe(
                (Drive::OnlyWhenChanged, Lifts::Both, reads),
                &LONG_RUN,
                seed,
            );
            let health = [(2, 100), (3, 70), (4, 200), (5, 250), (6, 0), (7, 40)];
            assert_steps(&o.health, &health, &what);
            let fraction = [(2, 0.5), (3, 0.35), (4, 0.8), (5, 1.0), (6, 0.0), (7, 0.16)];
            assert_steps(&o.fraction.unwrap(), &fraction, &what);
            let effective = [
                (1, 80),
                (2, 100),
                (3, 70),
                (4, 200),
                (5, 250),
                (6, 0),
                (7, 40),
            ];
            assert_steps(&o.effective.unwrap(), &effective, &what);
        }
    }
}

/// Shape 2a, the slice's stage 1 (bevy-sodium commit 20dfa33): health and
/// max_health only, heal and level-up meeting in one instant. GHC
/// (loop-shapes.md, shape 2a): max_health `[1] 200`, health `[1] 100`,
/// fraction `[1] 0.5`: the clamp uses the maximum from before the instant.
#[test]
fn the_slices_first_stage_clamps_by_the_maximum_before_the_instant() {
    for seed in SEEDS {
        let (mut graph, (heal_in, level_up_in, max_health, health, fraction)) = Graph::build(|b| {
            let (heal, heal_in) = b.input::<u32>();
            let (level_up, level_up_in) = b.input::<u32>();
            let (max_fwd, max_loop) = b.cell_loop::<u32>();
            let max_health = level_up.snapshot(max_fwd, |e, c| c + e).hold(b, 100);
            max_loop.close(b, max_health);
            let (health_fwd, health_loop) = b.cell_loop::<u32>();
            let health = heal
                .snapshot(health_fwd, |h, cur| (h, *cur))
                .snapshot(max_health, |(h, cur), max| (cur + h).min(*max))
                .hold(b, 60);
            health_loop.close(b, health);
            let fraction = (max_health, health).lift(b, |m, h| *h as f32 / *m as f32);
            (heal_in, level_up_in, max_health, health, fraction)
        });
        graph.set_shuffle_seed(seed);
        let now = Rc::new(StdCell::new(1));
        let fractions = record(&mut graph, fraction, &now);
        graph.transaction(|tx| {
            tx.send(heal_in, 100);
            tx.send(level_up_in, 100);
        });
        assert_eq!(*graph.sample(max_health), 200, "seed {seed:?}");
        assert_eq!(*graph.sample(health), 100, "seed {seed:?}");
        assert_eq!(*fractions.borrow(), [(1, 0.5)], "seed {seed:?}");
    }
}

/// The verification's two-input reading of #52's "two-sink version":
/// heal and damage only, NoMaxRead's update, and the shield looped or a
/// plain hold. Both readings reproduce the bug on sodium-rust 2.1.3
/// (health 60 with the lift). GHC (Slice.hs, `two sinks`): health `[1]
/// 140` at record 0003's heal and damage, with or without the lift, and
/// on the long schedule 60, 130, 100, 600, 700, 400, 440.
#[test]
fn sodium_rust_52_two_input_reduction() {
    for seed in SEEDS {
        for shield_looped in [true, false] {
            for lifted in [false, true] {
                for (schedule, expected) in [
                    (&[(Some(100), Some(50))][..], &[(1, 140)][..]),
                    (
                        &[
                            (None, Some(10)),
                            (Some(100), Some(50)),
                            (None, Some(30)),
                            (Some(500), None),
                            (Some(100), None),
                            (None, Some(300)),
                            (Some(40), None),
                        ][..],
                        &[
                            (1, 60),
                            (2, 130),
                            (3, 100),
                            (4, 600),
                            (5, 700),
                            (6, 400),
                            (7, 440),
                        ][..],
                    ),
                ] {
                    let what = format!("looped {shield_looped}, lifted {lifted}, seed {seed:?}");
                    let (mut graph, (heal_in, damage_in, health, effective)) = Graph::build(|b| {
                        let (heal, heal_in) = b.input::<u32>();
                        let (damage, damage_in) = b.input::<u32>();
                        let damage = damage.share(b);
                        let shield = if shield_looped {
                            let (shield_fwd, shield_loop) = b.cell_loop::<u32>();
                            let shield = damage
                                .snapshot(shield_fwd, |d, s| s.saturating_sub(d))
                                .hold(b, 30);
                            shield_loop.close(b, shield);
                            shield
                        } else {
                            damage.map(|d| 30u32.saturating_sub(d)).hold(b, 30)
                        };
                        let took = damage.snapshot(shield, |d, s| -(d.saturating_sub(*s) as i64));
                        let (health_fwd, health_loop) = b.cell_loop::<u32>();
                        let health = heal
                            .map(|h| h as i64)
                            .merge(b, took, |x, y| x + y)
                            .snapshot(health_fwd, |d, cur| (*cur as i64 + d).max(0) as u32)
                            .hold(b, 60);
                        health_loop.close(b, health);
                        let effective = lifted.then(|| (health, shield).lift(b, |h, s| h + s));
                        (heal_in, damage_in, health, effective)
                    });
                    graph.set_shuffle_seed(seed);
                    let now = Rc::new(StdCell::new(0));
                    let steps = record(&mut graph, health, &now);
                    for (k, &(heal, damage)) in schedule.iter().enumerate() {
                        now.set(k + 1);
                        graph.transaction(|tx| {
                            if let Some(v) = heal {
                                tx.send(heal_in, v);
                            }
                            if let Some(v) = damage {
                                tx.send(damage_in, v);
                            }
                        });
                    }
                    assert_eq!(*steps.borrow(), expected, "{what}");
                    if let Some(e) = effective {
                        let shield_after = *graph.sample(e) - *graph.sample(health);
                        assert_eq!(shield_after, 0, "{what}");
                    }
                }
            }
        }
    }
}

/// The slice's intermediate streams, shared so they can be listened to:
/// took and delta. GHC (loop-shapes.md, shape 2b): took `[1] 0 [2] -30 [3]
/// -30 [6] -300`, delta `[1] 0 [2] 70 [3] -30 [4] 500 [5] 100 [6] -300
/// [7] 40`; at record 0003's instant took `[1] -20` and delta `[1] 80`.
#[test]
fn the_slices_took_and_delta_events() {
    for seed in SEEDS {
        for (schedule, took_expected, delta_expected) in [
            (&RECORD_0003[..], &[(1, -20)][..], &[(1, 80)][..]),
            (
                &LONG_RUN[..],
                &[(1, 0), (2, -30), (3, -30), (6, -300)][..],
                &[
                    (1, 0),
                    (2, 70),
                    (3, -30),
                    (4, 500),
                    (5, 100),
                    (6, -300),
                    (7, 40),
                ][..],
            ),
        ] {
            let (mut graph, (edge, took, delta)) = Graph::build(|b| {
                let (heal_s, heal) = b.input::<u32>();
                let (damage_s, damage) = b.input::<u32>();
                let (level_up_s, level_up) = b.input::<u32>();
                let damage_s = damage_s.share(b);
                let (max_fwd, max_loop) = b.cell_loop::<u32>();
                let max_health = level_up_s.snapshot(max_fwd, |e, c| c + e).hold(b, 100);
                max_loop.close(b, max_health);
                let (shield_fwd, shield_loop) = b.cell_loop::<u32>();
                let shield = damage_s
                    .snapshot(shield_fwd, |d, s| s.saturating_sub(d))
                    .hold(b, 30);
                shield_loop.close(b, shield);
                let took: Shared<i64> = damage_s
                    .snapshot(shield, |d, s| -(d.saturating_sub(*s) as i64))
                    .share(b);
                let delta: Shared<i64> = heal_s
                    .map(|h| h as i64)
                    .merge(b, took, |x, y| x + y)
                    .share(b);
                let (health_fwd, health_loop) = b.cell_loop::<u32>();
                let health = delta
                    .snapshot(health_fwd, |d, cur| (d, *cur))
                    .snapshot(max_health, |(d, cur), max| clamp(d, cur, *max))
                    .hold(b, 60);
                health_loop.close(b, health);
                let edge = Slice {
                    heal,
                    damage,
                    level_up,
                    max_health,
                    shield,
                    health,
                    fraction: None,
                    effective: None,
                };
                (edge, took, delta)
            });
            graph.set_shuffle_seed(seed);
            let now = Rc::new(StdCell::new(0));
            let events = |graph: &mut Graph, stream: Shared<i64>| {
                let log: Steps<i64> = Rc::default();
                let (now, writer) = (now.clone(), log.clone());
                graph
                    .listen(stream, move |v| writer.borrow_mut().push((now.get(), v)))
                    .keep();
                log
            };
            let took_seen = events(&mut graph, took);
            let delta_seen = events(&mut graph, delta);
            drive(&mut graph, &edge, schedule, &now);
            assert_eq!(*took_seen.borrow(), took_expected, "seed {seed:?}");
            assert_eq!(*delta_seen.borrow(), delta_expected, "seed {seed:?}");
        }
    }
}

/// Diamonds through loops: x and y read each other through snapshots of
/// their forward tokens, and both read `level`. Lifts join them with
/// level, which is upstream of both, and with a hold of the shared ticks,
/// which steps in the same instants; one lift reads the held cells and one
/// the forwards. GHC (Stage3.hs, `diamond`; six evaluations): x `(1,[[1] 3,
/// [3] 8, [4] 25, [6] 24, [7] 56])`, y `(2,[[1] 4, [3] 10, [4] 24, [6] 53,
/// [7] 52])`, the lift of level, x and y `[1] 1003004 [2] 2003004 [3]
/// 3008010 [4] 3025024 [5] 3025024 [6] 1024053 [7] 1056052`, and the lift
/// of x, y and the ticks' hold `[1] 8 [3] 20 [4] 54 [6] 77 [7] 111`. At
/// [3] and [6] level steps in the same instant as the ticks, and both
/// loops read it from before the instant; each lift steps once per
/// instant, so its function runs once per step with a steps view.
#[test]
fn two_loops_that_read_each_other_lifted_with_what_is_upstream_of_both() {
    // (ticks, level) per instant.
    let schedule: [(Option<u32>, Option<u32>); 7] = [
        (Some(1), None),
        (None, Some(2)),
        (Some(2), Some(3)),
        (Some(5), None),
        (None, Some(3)),
        (Some(0), Some(1)),
        (Some(3), None),
    ];
    for seed in SEEDS {
        let calls = Rc::new(StdCell::new(0u32));
        let count = calls.clone();
        let (mut graph, (ticks_in, level_in, cells, views)) = Graph::build(move |b| {
            let (x_fwd, x_loop) = b.cell_loop::<u32>();
            let (y_fwd, y_loop) = b.cell_loop::<u32>();
            let (ticks, ticks_in) = b.input::<u32>();
            let ticks = ticks.share(b);
            let (level, level_in) = b.input_cell(1u32);
            let x = ticks
                .snapshot(y_fwd, |t, y| (t, *y))
                .snapshot(level, |(t, y), l| (y + t * l) % 1000)
                .hold(b, 1u32);
            let y = ticks
                .snapshot(x_fwd, |t, x| (t, *x))
                .snapshot(level, |(t, x), l| (x * 2 + t + l) % 1000)
                .hold(b, 2u32);
            x_loop.close(b, x);
            y_loop.close(b, y);
            let both = (level, x, y).lift(b, |l, x, y| l * 1_000_000 + x * 1000 + y);
            let both_fwd = (level, x_fwd, y_fwd).lift(b, move |l, x, y| {
                count.set(count.get() + 1);
                l * 1_000_000 + x * 1000 + y
            });
            let last = ticks.hold(b, 0u32);
            let total = (x_fwd, y, last).lift(b, |x, y, t| x + y + t);
            let views = both_fwd.steps(b);
            (ticks_in, level_in, [x, y, both, both_fwd, total], views)
        });
        graph.set_shuffle_seed(seed);
        let now = Rc::new(StdCell::new(0));
        let recorded = cells.map(|c| record(&mut graph, c, &now));
        let viewed: Steps<u32> = Rc::default();
        let (clock, writer) = (now.clone(), viewed.clone());
        graph
            .listen(views, move |v| writer.borrow_mut().push((clock.get(), v)))
            .keep();
        for (k, &(ticks, level)) in schedule.iter().enumerate() {
            now.set(k + 1);
            graph.transaction(|tx| {
                if let Some(t) = ticks {
                    tx.send(ticks_in, t);
                }
                if let Some(l) = level {
                    tx.send(level_in, l);
                }
            });
        }
        let what = format!("seed {seed:?}");
        let [x, y, both, both_fwd, total] = recorded.map(|r| r.borrow().clone());
        assert_eq!(x, [(1, 3), (3, 8), (4, 25), (6, 24), (7, 56)], "{what}");
        assert_eq!(y, [(1, 4), (3, 10), (4, 24), (6, 53), (7, 52)], "{what}");
        let joined = [
            (1, 1003004),
            (2, 2003004),
            (3, 3008010),
            (4, 3025024),
            (5, 3025024),
            (6, 1024053),
            (7, 1056052),
        ];
        assert_eq!(both, joined, "{what}");
        assert_eq!(both_fwd, joined, "{what}");
        assert_eq!(*viewed.borrow(), joined, "{what}");
        assert_eq!(
            total,
            [(1, 8), (3, 20), (4, 54), (6, 77), (7, 111)],
            "{what}"
        );
        assert_eq!(calls.get(), 7, "one call per step, {what}");
    }
}
