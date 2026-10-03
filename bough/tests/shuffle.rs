//! Order independence (RFD 1's shuffle affordance): the same program under
//! every shuffle seed gives the same values and the same events per node.
//! Only the interleaving of different nodes' listeners may move, and the
//! test checks that it does, so the affordance is known to shuffle.

use std::cell::{Cell as StdCell, RefCell};
use std::rc::Rc;

use bough::{CollectionPolicy, Input, Lift, Runtime, Source, State};

/// Everything one run observed: each listener's events in order, the
/// cells' final values, and the global interleaving of listener calls.
#[derive(Debug, PartialEq)]
struct Run {
    per_listener: Vec<Vec<String>>,
    samples: Vec<u64>,
    interleaving: Vec<usize>,
}

/// Sends of one transaction: which input, and the value.
type Sends = Vec<(usize, u64)>;

fn schedule() -> Vec<Sends> {
    let mut transactions = Vec::new();
    for k in 0..40u64 {
        let sends = match k % 5 {
            0 => vec![(0, k)],
            1 => vec![(1, k)],
            2 => vec![(0, k), (1, k + 1)],
            3 => vec![(1, k), (0, k + 7), (2, k)],
            _ => vec![(2, k), (2, 1), (0, k * 3)],
        };
        transactions.push(sends);
    }
    transactions
}

fn run(seed: Option<u64>) -> Run {
    let (mut graph, edge) = Runtime::build(|b| {
        let (a, a_in) = b.input::<u64>();
        let (c, c_in) = b.input::<u64>();
        let (bump, bump_in) = b.input_coalescing(|x: u64, y| x * 10 + y);
        let a = a.share(b);
        let c = c.share(b);
        let bump = bump.share(b);
        let (limit, _limit_in) = b.input_cell(50u64);
        let (open, _open_in) = b.input_cell(true);
        // A diamond from `a`, a merge across inputs, a snapshot of a hold
        // fed by the diamond, and a gate: independent nodes at every level.
        let left = a.map(|x| x + 1).share(b);
        let right = a.filter(|x| x % 2 == 0).map(|x| x * 100).share(b);
        let diamond = left.merge(b, right, |l, r| l + r).share(b);
        let across = diamond.or_else(b, c.map(|x| x + 1000)).share(b);
        let held = across.hold(b, 0u64);
        let clipped = c
            .snapshot(held, |x, h| x.min(*h))
            .snapshot(limit, |x, l| x.max(*l))
            .gate(open)
            .hold(b, 0u64);
        let first = a.once().map_to(7u64).hold(b, 0u64);
        let bumped = bump
            .merge(b, c.filter_map(|x| (x % 3 == 0).then_some(x)), |l, r| l + r)
            .hold(b, 0u64);
        let inputs: [Input<u64>; 3] = [a_in, c_in, bump_in];
        (
            inputs,
            [a, c, bump, left, right, diamond, across],
            [held, clipped, first, bumped],
        )
    });
    let (inputs, streams, cells) = edge.keep();
    graph.set_shuffle_seed(seed);

    let log: Rc<RefCell<Vec<Vec<String>>>> = Rc::new(RefCell::new(Vec::new()));
    let order: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
    let add = |log: &Rc<RefCell<Vec<Vec<String>>>>| {
        log.borrow_mut().push(Vec::new());
        log.borrow().len() - 1
    };
    for stream in streams {
        // Two listeners per stream, so ties within a node are shuffled too.
        for _ in 0..2 {
            let id = add(&log);
            let (log, order) = (log.clone(), order.clone());
            graph
                .listen(stream, move |v| {
                    log.borrow_mut()[id].push(format!("{v}"));
                    order.borrow_mut().push(id);
                })
                .keep();
        }
    }
    for cell in cells {
        let id = add(&log);
        let (log_c, order_c) = (log.clone(), order.clone());
        graph
            .listen_cell(cell, move |v| {
                log_c.borrow_mut()[id].push(format!("cell {v}"));
                order_c.borrow_mut().push(id);
            })
            .keep();
        let id = add(&log);
        let (log, order) = (log.clone(), order.clone());
        graph
            .listen_steps(cell, move |v| {
                log.borrow_mut()[id].push(format!("step {v}"));
                order.borrow_mut().push(id);
            })
            .keep();
    }
    for sends in schedule() {
        graph.transaction(|tx| {
            for (input, value) in sends {
                tx.send(inputs[input], value);
            }
        });
    }
    let samples = cells.iter().map(|c| *graph.sample(*c)).collect();
    let per_listener = log.borrow().clone();
    let interleaving = order.borrow().clone();
    Run {
        per_listener,
        samples,
        interleaving,
    }
}

#[test]
fn every_shuffle_seed_gives_the_same_values_and_events_per_node() {
    let plain = run(None);
    assert!(plain.per_listener.iter().all(|events| !events.is_empty()));
    let mut interleavings = std::collections::BTreeSet::new();
    interleavings.insert(plain.interleaving.clone());
    for seed in 0..24 {
        let shuffled = run(Some(seed));
        assert_eq!(shuffled.per_listener, plain.per_listener, "seed {seed}");
        assert_eq!(shuffled.samples, plain.samples, "seed {seed}");
        interleavings.insert(shuffled.interleaving);
    }
    // Every seed gives its own interleaving today; allow a few collisions.
    assert!(
        interleavings.len() >= 20,
        "the shuffle moved dispatch order ({} distinct interleavings of 25)",
        interleavings.len()
    );
}

#[test]
fn a_seed_reproduces_its_order() {
    assert_eq!(run(Some(7)), run(Some(7)));
    assert_eq!(run(None), run(None));
}

#[test]
fn a_merge_of_simultaneous_sends_calls_its_function_once_under_every_seed() {
    for seed in [None, Some(0), Some(1), Some(2), Some(3), Some(99)] {
        let calls = Rc::new(std::cell::Cell::new(0u32));
        let counter = calls.clone();
        let (mut graph, edge) = Runtime::build(move |b| {
            let (left, left_in) = b.input::<u32>();
            let (right, right_in) = b.input::<u32>();
            let left = left.share(b);
            let merged = left
                .map(|l| l + 1)
                .merge(b, right, move |l, r| {
                    counter.set(counter.get() + 1);
                    l * 100 + r
                })
                .merge(b, left.filter(|l| l % 2 == 0), |m, l| m * 10 + l)
                .hold(b, 0u32);
            (left_in, right_in, merged)
        });
        let (left_in, right_in, merged) = edge.keep();
        graph.set_shuffle_seed(seed);
        graph.transaction(|tx| {
            tx.send(right_in, 7);
            tx.send(left_in, 2);
        });
        assert_eq!(
            *graph.sample(merged),
            (3 * 100 + 7) * 10 + 2,
            "seed {seed:?}"
        );
        assert_eq!(calls.get(), 1, "seed {seed:?}");
    }
}

#[test]
fn snapshots_and_gates_read_the_value_before_the_instant_under_every_seed() {
    for seed in [None, Some(0), Some(5), Some(17), Some(1 << 40)] {
        let (mut graph, edge) = Runtime::build(|b| {
            let (numbers, numbers_in) = b.input::<u32>();
            let numbers = numbers.share(b);
            let (limit, limit_in) = b.input_cell(10u32);
            let (open, open_in) = b.input_cell(false);
            let clipped = numbers.snapshot(limit, |n, l| n.min(*l)).hold(b, 0u32);
            let gated = numbers.gate(open).hold(b, 0u32);
            (numbers_in, limit_in, open_in, clipped, gated)
        });
        let (numbers_in, limit_in, open_in, clipped, gated) = edge.keep();
        graph.set_shuffle_seed(seed);
        graph.transaction(|tx| {
            tx.send(open_in, true);
            tx.send(numbers_in, 50);
            tx.send(limit_in, 20);
        });
        assert_eq!(*graph.sample(clipped), 10, "seed {seed:?}");
        assert_eq!(*graph.sample(gated), 0, "seed {seed:?}");
        graph.send(numbers_in, 50);
        assert_eq!(*graph.sample(clipped), 20, "seed {seed:?}");
        assert_eq!(*graph.sample(gated), 50, "seed {seed:?}");
    }
}

/// The stage 2 cell operations under one seed: read-through cells, lifts
/// with inputs that step together, accumulators, an in-place accumulator
/// read by a map_cell, a lift, a snapshot and a gate, a scan, and steps
/// views, with listeners on all of them. Besides the events and values,
/// the run counts the calls of each read-through function, which a
/// promotion that depended on evaluation order would change.
fn run_cells(seed: Option<u64>) -> (Run, [u32; 5]) {
    let calls: Rc<[StdCell<u32>; 5]> = Rc::new(Default::default());
    let counted = |k: usize| {
        let calls = calls.clone();
        move || calls[k].set(calls[k].get() + 1)
    };
    let (c0, c1, c2, c3, c4) = (counted(0), counted(1), counted(2), counted(3), counted(4));
    let (mut graph, edge) = Runtime::build(move |b| {
        let (a, a_in) = b.input::<u64>();
        let (c, c_in) = b.input::<u64>();
        let (d, d_in) = b.input_coalescing(|x: u64, y| x * 10 + y);
        let a = a.share(b);
        let c = c.share(b);
        let d = d.share(b);
        let held_a = a.hold(b, 1u64);
        let held_c = c.filter(|x| x % 3 != 0).hold(b, 2u64);
        let held_d = d.hold(b, 3u64);
        let doubled = held_a.map_cell(b, move |x| {
            c0();
            x * 2
        });
        let sum = (doubled, held_c).lift(b, move |x, y| {
            c1();
            x + y
        });
        let three = (held_a, held_c, held_d).lift(b, move |x, y, z| {
            c2();
            x * 100 + y * 10 + z
        });
        let chained = (sum, three).lift(b, move |s, t| {
            c3();
            s + t
        });
        let total = a.accumulate(b, 0u64, |x, t| t + x);
        let recent = c.accumulate_mut(b, [0u64; 3], |x, r: &mut [u64; 3]| {
            r.rotate_left(1);
            r[2] = x;
        });
        let odd = a.accumulate_mut(b, false, |x, odd: &mut bool| *odd = x % 2 == 1);
        let recent_sum: State<u64> = recent.map_cell(b, move |r| {
            c4();
            r.iter().sum()
        });
        let mixed: State<u64> = (recent_sum, total).lift(b, |r, t| r * 1000 + t);
        let scanned = a.scan(b, 0u64, |x, n| (x * 10 + n, n + 1)).share(b);
        let sum_steps = sum.steps(b).share(b);
        let chained_steps = chained.steps(b).share(b);
        let three_current = three.steps_with_current(b).share(b);
        let read = d
            .snapshot(sum, |x, s| x + s)
            .snapshot(mixed, |x, m| x + m)
            .gate(odd)
            .share(b);
        let inputs: [Input<u64>; 3] = [a_in, c_in, d_in];
        (
            inputs,
            [scanned, sum_steps, chained_steps, three_current, read],
            [doubled, sum, three, chained, total],
            [recent_sum, mixed],
        )
    });
    let (inputs, streams, cells, states) = edge.keep();
    graph.set_shuffle_seed(seed);

    let log: Rc<RefCell<Vec<Vec<String>>>> = Rc::new(RefCell::new(Vec::new()));
    let order: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
    let add = |log: &Rc<RefCell<Vec<Vec<String>>>>| {
        log.borrow_mut().push(Vec::new());
        log.borrow().len() - 1
    };
    let listener = |id: usize, tag: &'static str| {
        let (log, order) = (log.clone(), order.clone());
        move |v: &u64| {
            log.borrow_mut()[id].push(format!("{tag} {v}"));
            order.borrow_mut().push(id);
        }
    };
    for stream in streams {
        for _ in 0..2 {
            let on = listener(add(&log), "event");
            graph.listen(stream, move |v| on(&v)).keep();
        }
    }
    for cell in cells {
        graph.listen_cell(cell, listener(add(&log), "cell")).keep();
        graph.listen_steps(cell, listener(add(&log), "step")).keep();
    }
    for state in states {
        graph.listen_cell(state, listener(add(&log), "cell")).keep();
        graph
            .listen_steps(state, listener(add(&log), "step"))
            .keep();
    }
    for sends in schedule() {
        graph.transaction(|tx| {
            for (input, value) in sends {
                tx.send(inputs[input], value);
            }
        });
    }
    let mut samples: Vec<u64> = cells.iter().map(|c| *graph.sample(*c)).collect();
    samples.extend(states.iter().map(|s| *graph.sample(*s)));
    let per_listener = log.borrow().clone();
    let interleaving = order.borrow().clone();
    let counts = [0, 1, 2, 3, 4].map(|k| calls[k].get());
    (
        Run {
            per_listener,
            samples,
            interleaving,
        },
        counts,
    )
}

#[test]
fn every_shuffle_seed_gives_the_same_cells_steps_and_function_calls() {
    let (plain, plain_calls) = run_cells(None);
    assert!(plain.per_listener.iter().all(|events| !events.is_empty()));
    let mut interleavings = std::collections::BTreeSet::new();
    interleavings.insert(plain.interleaving.clone());
    for seed in 0..24 {
        let (shuffled, calls) = run_cells(Some(seed));
        assert_eq!(shuffled.per_listener, plain.per_listener, "seed {seed}");
        assert_eq!(shuffled.samples, plain.samples, "seed {seed}");
        assert_eq!(calls, plain_calls, "seed {seed}");
        interleavings.insert(shuffled.interleaving);
    }
    assert!(
        interleavings.len() >= 20,
        "the shuffle moved dispatch order ({} distinct interleavings of 25)",
        interleavings.len()
    );
}

/// Stage 3's loops under one seed: a counter through a snapshot of its
/// forward with a steps view of the forward; two loops that read each other
/// through snapshots, over a cell upstream of both, lifted over their
/// forwards with a steps view; an accumulator reading itself through a
/// read-through cell over its forward; a stream loop through a hold read
/// by snapshot; and a state loop whose in-place accumulator reads its own
/// forward, with a State over it. Listeners on every stream, cell and
/// State. Besides the events and values, the run counts the calls of the
/// lift over the loops' forwards, which settles once per instant.
fn run_loops(seed: Option<u64>) -> (Run, u32) {
    let calls = Rc::new(StdCell::new(0u32));
    let count = calls.clone();
    let (mut graph, edge) = Runtime::build(move |b| {
        let (a, a_in) = b.input::<u64>();
        let (c, c_in) = b.input::<u64>();
        let (d, d_in) = b.input_coalescing(|x: u64, y| x * 10 + y);
        let a = a.share(b);
        let c = c.share(b);
        let d = d.share(b);

        let (counted, counted_loop) = b.cell_loop::<u64>();
        let counted_steps = counted.steps(b).share(b);
        let next = a.snapshot(counted, |_, n| n + 1).hold(b, 0u64);
        counted_loop.close(b, next);

        let (x_fwd, x_loop) = b.cell_loop::<u64>();
        let (y_fwd, y_loop) = b.cell_loop::<u64>();
        let level = c.hold(b, 1u64);
        let x = a
            .snapshot(y_fwd, |t, y| (t, *y))
            .snapshot(level, |(t, y), l| (y + t * l) % 1000)
            .hold(b, 1u64);
        let y = d
            .snapshot(x_fwd, |t, x| (t, *x))
            .snapshot(level, |(t, x), l| (x * 2 + t + l) % 1000)
            .hold(b, 2u64);
        x_loop.close(b, x);
        y_loop.close(b, y);
        let joined = (level, x_fwd, y_fwd).lift(b, move |l, x, y| {
            count.set(count.get() + 1);
            l * 1_000_000 + x * 1000 + y
        });
        let joined_steps = joined.steps(b).share(b);

        let (acc_fwd, acc_loop) = b.cell_loop::<u64>();
        let halved = acc_fwd.map_cell(b, |s| s / 2);
        let acc = c
            .snapshot(halved, |t, h| t + h)
            .accumulate(b, 1u64, |x, s| (s + x) % 100_000);
        acc_loop.close(b, acc);

        let (sums, sums_loop) = b.stream_loop::<u64>();
        let sums = sums.share(b);
        let last = sums.hold(b, 0u64);
        sums_loop.close(b, d.snapshot(last, |t, l| (t + l) % 100_000));

        let (recent, recent_loop) = b.state_loop::<[u64; 3]>();
        let definition = a.snapshot(recent, |t, r| t + r[2]).accumulate_mut(
            b,
            [0u64; 3],
            |x, r: &mut [u64; 3]| {
                r.rotate_left(1);
                r[2] = x % 1000;
            },
        );
        recent_loop.close(b, definition);
        let recent_sum: State<u64> = recent.map_cell(b, |r| r.iter().sum());

        let inputs: [Input<u64>; 3] = [a_in, c_in, d_in];
        (
            inputs,
            [counted_steps, joined_steps, sums],
            [counted, next, x_fwd, y, joined, acc_fwd, halved, last],
            [recent_sum],
        )
    });
    let (inputs, streams, cells, states) = edge.keep();
    graph.set_shuffle_seed(seed);

    let log: Rc<RefCell<Vec<Vec<String>>>> = Rc::new(RefCell::new(Vec::new()));
    let order: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
    let add = |log: &Rc<RefCell<Vec<Vec<String>>>>| {
        log.borrow_mut().push(Vec::new());
        log.borrow().len() - 1
    };
    let listener = |id: usize, tag: &'static str| {
        let (log, order) = (log.clone(), order.clone());
        move |v: &u64| {
            log.borrow_mut()[id].push(format!("{tag} {v}"));
            order.borrow_mut().push(id);
        }
    };
    for stream in streams {
        for _ in 0..2 {
            let on = listener(add(&log), "event");
            graph.listen(stream, move |v| on(&v)).keep();
        }
    }
    for cell in cells {
        graph.listen_cell(cell, listener(add(&log), "cell")).keep();
        graph.listen_steps(cell, listener(add(&log), "step")).keep();
    }
    for state in states {
        graph.listen_cell(state, listener(add(&log), "cell")).keep();
        graph
            .listen_steps(state, listener(add(&log), "step"))
            .keep();
    }
    for sends in schedule() {
        graph.transaction(|tx| {
            for (input, value) in sends {
                tx.send(inputs[input], value);
            }
        });
    }
    let mut samples: Vec<u64> = cells.iter().map(|c| *graph.sample(*c)).collect();
    samples.extend(states.iter().map(|s| *graph.sample(*s)));
    let per_listener = log.borrow().clone();
    let interleaving = order.borrow().clone();
    (
        Run {
            per_listener,
            samples,
            interleaving,
        },
        calls.get(),
    )
}

#[test]
fn every_shuffle_seed_gives_the_same_loops_values_and_events() {
    let (plain, plain_calls) = run_loops(None);
    assert!(plain.per_listener.iter().all(|events| !events.is_empty()));
    // Listener 15 is listen_steps on the lift over the forwards: six stream
    // listeners, then two per cell. Its function ran once for listen_cell
    // at registration and once per step, for the steps view, whose value
    // commit promoted into the memo the cell listeners read.
    let joined_steps = plain.per_listener[15].len() as u32;
    assert!(joined_steps > 10);
    assert_eq!(plain_calls, 1 + joined_steps);
    let mut interleavings = std::collections::BTreeSet::new();
    interleavings.insert(plain.interleaving.clone());
    for seed in 0..24 {
        let (shuffled, calls) = run_loops(Some(seed));
        assert_eq!(shuffled.per_listener, plain.per_listener, "seed {seed}");
        assert_eq!(shuffled.samples, plain.samples, "seed {seed}");
        assert_eq!(calls, plain_calls, "seed {seed}");
        interleavings.insert(shuffled.interleaving);
    }
    assert!(
        interleavings.len() >= 20,
        "the shuffle moved dispatch order ({} distinct interleavings of 25)",
        interleavings.len()
    );
}

/// Stage 4's child transactions under one seed: two splits and a defer
/// that share child indices, merged; a split fed by its own children
/// through a stream loop, three levels deep; a countdown loop through a
/// defer; a hold stepping in the children, read by a snapshot in later
/// children and by a map_cell with a steps view; and an accumulator over
/// the countdown. Listeners on every stream and cell. Each child instant is
/// shuffled as any transaction is, by its own serial.
fn run_children(seed: Option<u64>) -> Run {
    let (mut graph, edge) = Runtime::build(|b| {
        let (a, a_in) = b.input::<u64>();
        let (c, c_in) = b.input::<u64>();
        let (d, d_in) = b.input_coalescing(|x: u64, y| x * 10 + y);
        let a = a.share(b);
        let c = c.share(b);
        let d = d.share(b);

        let ones = a.map(|x| [x, x + 1]).split(b);
        let twos = c.map(|x| [x % 7, x % 5, x % 3]).split(b);
        let pairs = ones.merge(b, twos, |o, t| o * 100 + t).share(b);
        let later = d.defer(b);
        let joined = pairs.merge(b, later, |p, l| p * 1000 + l).share(b);

        let (fwd, fwd_loop) = b.stream_loop::<Vec<u64>>();
        let items = fwd.split(b).share(b);
        let again = items.filter(|n| *n < 100).map(|n| vec![n * 10, n * 10 + 1]);
        let definition = a.map(|x| vec![x % 3 + 1, x % 3 + 2]).or_else(b, again);
        fwd_loop.close(b, definition);

        let (down, down_loop) = b.stream_loop::<u64>();
        let back = down.filter(|n| *n > 1).map(|n| n - 1).defer(b);
        let countdown = c.map(|x| x % 5).or_else(b, back).share(b);
        down_loop.close(b, countdown);

        let held = joined.hold(b, 0u64);
        let seen = items.snapshot(held, |i, h| i + h).share(b);
        let doubled = held.map_cell(b, |h| h * 2);
        let doubled_steps = doubled.steps(b).share(b);
        let total = countdown.accumulate(b, 0u64, |n, t| t + n);
        let inputs: [Input<u64>; 3] = [a_in, c_in, d_in];
        (
            inputs,
            [pairs, joined, items, countdown, seen, doubled_steps],
            [held, doubled, total],
        )
    });
    let (inputs, streams, cells) = edge.keep();
    graph.set_shuffle_seed(seed);

    let log: Rc<RefCell<Vec<Vec<String>>>> = Rc::new(RefCell::new(Vec::new()));
    let order: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
    let add = |log: &Rc<RefCell<Vec<Vec<String>>>>| {
        log.borrow_mut().push(Vec::new());
        log.borrow().len() - 1
    };
    let listener = |id: usize, tag: &'static str| {
        let (log, order) = (log.clone(), order.clone());
        move |v: &u64| {
            log.borrow_mut()[id].push(format!("{tag} {v}"));
            order.borrow_mut().push(id);
        }
    };
    for stream in streams {
        for _ in 0..2 {
            let on = listener(add(&log), "event");
            graph.listen(stream, move |v| on(&v)).keep();
        }
    }
    for cell in cells {
        graph.listen_cell(cell, listener(add(&log), "cell")).keep();
        graph.listen_steps(cell, listener(add(&log), "step")).keep();
    }
    for sends in schedule() {
        graph.transaction(|tx| {
            for (input, value) in sends {
                tx.send(inputs[input], value);
            }
        });
    }
    let samples = cells.iter().map(|c| *graph.sample(*c)).collect();
    let per_listener = log.borrow().clone();
    let interleaving = order.borrow().clone();
    Run {
        per_listener,
        samples,
        interleaving,
    }
}

#[test]
fn every_shuffle_seed_gives_the_same_children_and_events() {
    let plain = run_children(None);
    assert!(plain.per_listener.iter().all(|events| !events.is_empty()));
    let mut interleavings = std::collections::BTreeSet::new();
    interleavings.insert(plain.interleaving.clone());
    for seed in 0..24 {
        let shuffled = run_children(Some(seed));
        assert_eq!(shuffled.per_listener, plain.per_listener, "seed {seed}");
        assert_eq!(shuffled.samples, plain.samples, "seed {seed}");
        interleavings.insert(shuffled.interleaving);
    }
    assert!(
        interleavings.len() >= 20,
        "the shuffle moved dispatch order ({} distinct interleavings of 25)",
        interleavings.len()
    );
}

/// Stage 5's switches under one seed: a switch_cell among a hold, a
/// map_cell and a lift, selected by the input that steps two of them, with
/// a steps view that reads the new inner after the instant and pulls it
/// when the order has not reached it; a switch_cell over that switch and a
/// hold; a switch_stream among shared streams; a switch_stream among linear
/// streams in constant cells, selected through a switch_cell; and a loop
/// through a switch_stream's selection. Listeners on every stream and
/// cell. Besides the events and values, the run counts the calls of the
/// read-through functions, which a pull that ran a node twice, or a
/// promotion that depended on the order, would change; and with the
/// `statistics` feature the nodes run, in order or pulled out of it, and
/// the relinks.
fn run_switches(seed: Option<u64>) -> (Run, [u32; 2], Option<[u64; 2]>) {
    let calls = [Rc::new(StdCell::new(0u32)), Rc::new(StdCell::new(0u32))];
    let counted = calls.clone();
    let (mut graph, edge) = Runtime::build(move |b| {
        let (a, a_in) = b.input::<u64>();
        let (c, c_in) = b.input::<u64>();
        let (d, d_in) = b.input_coalescing(|x: u64, y| x * 10 + y);
        let a = a.share(b);
        let c = c.share(b);
        let d = d.share(b);
        let ha = a.hold(b, 0u64);
        let hc = c.hold(b, 0u64);
        let hd = d.hold(b, 0u64);
        let [on_m, on_sum] = counted;
        let m = hc.map_cell(b, move |x| {
            on_m.set(on_m.get() + 1);
            x * 3
        });
        let sum = (hc, hd).lift(b, move |x, y| {
            on_sum.set(on_sum.get() + 1);
            x + y
        });

        let picked = c.map(move |x| [ha, m, sum][(x % 3) as usize]).hold(b, ha);
        b.depends(&picked, &[&ha, &m, &sum]);
        let sw = picked.switch_cell(b);
        let sw_steps = sw.steps(b).share(b);
        let nested = a.map(move |x| if x % 2 == 0 { sw } else { hd }).hold(b, hd);
        b.depends(&nested, &[&sw, &hd]);
        let top = nested.switch_cell(b);
        let top_steps = top.steps(b).share(b);

        let shared = [a, c, d];
        let following = d.map(move |x| shared[(x % 3) as usize]).hold(b, a);
        b.depends(&following, &[&a, &c, &d]);
        let followed = following.switch_stream(b).share(b);

        let la = a.map(|x| x + 1000).node(b);
        let la = b.constant(la);
        let lc = c.map(|x| x + 2000).node(b);
        let lc = b.constant(lc);
        let lines = a.map(move |x| if x % 2 == 0 { lc } else { la }).hold(b, la);
        b.depends(&lines, &[&lc, &la]);
        let taken = lines.switch_cell(b).switch_stream(b).share(b);

        let (fwd, fwd_loop) = b.stream_loop::<u64>();
        let out = fwd.share(b);
        let inners = [a.map(|x| x + 1).share(b), c, d.map(|x| x * 2).share(b)];
        let first = inners[0];
        let [i0, i1, i2] = inners;
        let selecting = out.map(move |v| inners[(v % 3) as usize]).hold(b, first);
        b.depends(&selecting, &[&i0, &i1, &i2]);
        let selected = selecting.switch_stream(b);
        fwd_loop.close(b, selected);

        let inputs: [Input<u64>; 3] = [a_in, c_in, d_in];
        (
            inputs,
            [sw_steps, top_steps, followed, taken, out],
            [sw, top, m, sum],
        )
    });
    let (inputs, streams, cells) = edge.keep();
    graph.set_shuffle_seed(seed);

    let log: Rc<RefCell<Vec<Vec<String>>>> = Rc::new(RefCell::new(Vec::new()));
    let order: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
    let add = |log: &Rc<RefCell<Vec<Vec<String>>>>| {
        log.borrow_mut().push(Vec::new());
        log.borrow().len() - 1
    };
    let listener = |id: usize, tag: &'static str| {
        let (log, order) = (log.clone(), order.clone());
        move |v: &u64| {
            log.borrow_mut()[id].push(format!("{tag} {v}"));
            order.borrow_mut().push(id);
        }
    };
    for stream in streams {
        for _ in 0..2 {
            let on = listener(add(&log), "event");
            graph.listen(stream, move |v| on(&v)).keep();
        }
    }
    for cell in cells {
        graph.listen_cell(cell, listener(add(&log), "cell")).keep();
        graph.listen_steps(cell, listener(add(&log), "step")).keep();
    }
    for sends in schedule() {
        graph.transaction(|tx| {
            for (input, value) in sends {
                tx.send(inputs[input], value);
            }
        });
    }
    let samples = cells.iter().map(|c| *graph.sample(*c)).collect();
    let per_listener = log.borrow().clone();
    let interleaving = order.borrow().clone();
    let run = Run {
        per_listener,
        samples,
        interleaving,
    };
    (
        run,
        [calls[0].get(), calls[1].get()],
        runs_and_relinks(&graph),
    )
}

/// The nodes the evaluation loop ran plus those pulled out of order, and
/// the relinks, since the graph was built.
#[cfg(feature = "statistics")]
fn runs_and_relinks(graph: &Runtime) -> Option<[u64; 2]> {
    let s = graph.statistics();
    Some([s.evaluations + s.pulls, s.relinks])
}

#[cfg(not(feature = "statistics"))]
fn runs_and_relinks(_: &Runtime) -> Option<[u64; 2]> {
    None
}

#[test]
fn every_shuffle_seed_gives_the_same_switches_values_and_events() {
    let (plain, plain_calls, plain_runs) = run_switches(None);
    assert!(plain.per_listener.iter().all(|events| !events.is_empty()));
    let mut interleavings = std::collections::BTreeSet::new();
    interleavings.insert(plain.interleaving.clone());
    for seed in 0..24 {
        let (shuffled, calls, runs) = run_switches(Some(seed));
        assert_eq!(shuffled.per_listener, plain.per_listener, "seed {seed}");
        assert_eq!(shuffled.samples, plain.samples, "seed {seed}");
        assert_eq!(calls, plain_calls, "seed {seed}");
        // Pulls move with the order; every node still runs once per
        // instant, and the switches move as often.
        assert_eq!(runs, plain_runs, "seed {seed}");
        interleavings.insert(shuffled.interleaving);
    }
    assert!(
        interleavings.len() >= 20,
        "the shuffle moved dispatch order ({} distinct interleavings of 25)",
        interleavings.len()
    );
}

/// Stage 6's constructs under the shuffle: a construct whose closure builds
/// a hold over another input and a lift over it, which a switch_cell with
/// a steps view follows; a construct whose closure builds a linear merge
/// of two inputs, which a switch_stream takes from; a construct whose
/// closure declares and closes a counter loop; a construct built by a
/// closure, over the input that runs the outer one, so that it runs at its
/// creation instant; and a closure that builds a hold over its construct's
/// own events, through a stream loop, which a switch_cell with a steps
/// view follows: at the instant the hold is built, the view reads it after
/// the instant, which pulls what it depends on when the view runs first.
/// Listeners on every stream and cell.
/// Besides the events and values, the run counts the growth of the live
/// nodes, which under the manual policy is what the closures built, and
/// with the `statistics` feature the nodes run, in order, pulled out of it
/// or new: each node runs once per instant whatever the order.
fn run_constructs(seed: Option<u64>, policy: CollectionPolicy) -> (Run, usize, Option<u64>) {
    let (mut graph, edge) = Runtime::build(|b| {
        let (a, a_in) = b.input::<u64>();
        let (c, c_in) = b.input::<u64>();
        let (d, d_in) = b.input_coalescing(|x: u64, y| x * 10 + y);
        let a = a.share(b);
        let c = c.share(b);
        let d = d.share(b);
        let hc = c.hold(b, 0u64);
        let c0 = b.constant(0u64);

        let made = a.construct(b, move |b, x| {
            let h = c.map(move |v| v + x).hold(b, x);
            (h, hc).lift(b, |h, c| h * 2 + c)
        });
        b.depends(&made, &[&c, &hc]);
        let shown = made.hold(b, c0).switch_cell(b);
        let shown_steps = shown.steps(b).share(b);

        let first = a.map(|x| x + 1).node(b);
        let lines = d.construct(b, move |b, k| a.merge(b, c, move |l, r| l * k + r));
        b.depends(&lines, &[&a, &c]);
        let followed = lines.hold(b, first).switch_stream(b).share(b);

        let counters = c.filter(|v| v % 4 == 0).construct(b, move |b, _| {
            let (count, count_loop) = b.cell_loop::<u64>();
            let next = a.snapshot(count, |_, n| n + 1).hold(b, 0u64);
            count_loop.close(b, next);
            count
        });
        b.depends(&counters, &[&a]);
        let counted = counters.hold(b, c0).switch_cell(b);

        let bodies = d.construct(b, move |b, k| {
            let inner = d.construct(b, move |b, j| a.map(move |x| x + j * k).hold(b, j));
            b.depends(&inner, &[&a]);
            inner.hold(b, c0).switch_cell(b)
        });
        b.depends(&bodies, &[&a, &c0]);
        let nested = bodies.hold(b, c0).switch_cell(b);

        let (outs, outs_loop) = b.stream_loop::<u64>();
        let outs = outs.share(b);
        let own = c.construct(b, move |b, v| {
            (v + 1, outs.map(move |o| o * 2 + v).hold(b, v))
        });
        // `outs` is downstream of the construct, through the loop.
        b.depends(&own, &[&outs]);
        let own = own.share(b);
        outs_loop.close(b, own.map(|(n, _)| n));
        let own = own.map(|(_, h)| h).hold(b, c0).switch_cell(b);
        let own_steps = own.steps(b).share(b);

        let inputs: [Input<u64>; 3] = [a_in, c_in, d_in];
        (
            inputs,
            [shown_steps, followed, outs, own_steps],
            [shown, counted, nested, own, hc],
        )
    });
    let (inputs, streams, cells) = edge.keep();
    graph.set_shuffle_seed(seed);
    graph.set_collection_policy(policy);

    let log: Rc<RefCell<Vec<Vec<String>>>> = Rc::new(RefCell::new(Vec::new()));
    let order: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
    let add = |log: &Rc<RefCell<Vec<Vec<String>>>>| {
        log.borrow_mut().push(Vec::new());
        log.borrow().len() - 1
    };
    let listener = |id: usize, tag: &'static str| {
        let (log, order) = (log.clone(), order.clone());
        move |v: &u64| {
            log.borrow_mut()[id].push(format!("{tag} {v}"));
            order.borrow_mut().push(id);
        }
    };
    for stream in streams {
        for _ in 0..2 {
            let on = listener(add(&log), "event");
            graph.listen(stream, move |v| on(&v)).keep();
        }
    }
    for cell in cells {
        graph.listen_cell(cell, listener(add(&log), "cell")).keep();
        graph.listen_steps(cell, listener(add(&log), "step")).keep();
    }
    let before = graph.live_nodes();
    for sends in schedule() {
        graph.transaction(|tx| {
            for (input, value) in sends {
                tx.send(inputs[input], value);
            }
        });
    }
    let samples = cells.iter().map(|c| *graph.sample(*c)).collect();
    let per_listener = log.borrow().clone();
    let interleaving = order.borrow().clone();
    let run = Run {
        per_listener,
        samples,
        interleaving,
    };
    (run, graph.live_nodes() - before, node_runs(&graph))
}

/// The nodes run since the graph was built: in order, pulled out of it,
/// or as nodes new in their instant.
#[cfg(feature = "statistics")]
fn node_runs(graph: &Runtime) -> Option<u64> {
    let s = graph.statistics();
    Some(s.evaluations + s.pulls + s.new_nodes)
}

#[cfg(not(feature = "statistics"))]
fn node_runs(_: &Runtime) -> Option<u64> {
    None
}

/// Under the manual policy nothing is collected, so the growth of the
/// live nodes counts what the closures built. Under the automatic policy,
/// collections run between the transactions, free the bodies the switches
/// have left, and change no value and no event, under any seed.
#[test]
fn every_shuffle_seed_gives_the_same_constructs_values_and_events() {
    let manual = CollectionPolicy::Manual;
    let (plain, plain_built, plain_runs) = run_constructs(None, manual);
    assert!(plain.per_listener.iter().all(|events| !events.is_empty()));
    assert!(plain_built > 100, "the closures built {plain_built} nodes");
    let automatic = CollectionPolicy::Automatic;
    let (collected, collected_live, collected_runs) = run_constructs(None, automatic);
    assert_eq!(collected.per_listener, plain.per_listener);
    assert_eq!(collected.samples, plain.samples);
    assert!(
        collected_live < plain_built / 2,
        "collection freed the bodies left behind: {collected_live} of {plain_built} live"
    );
    let mut interleavings = std::collections::BTreeSet::new();
    interleavings.insert(plain.interleaving.clone());
    for seed in 0..24 {
        let (shuffled, built, runs) = run_constructs(Some(seed), manual);
        assert_eq!(shuffled.per_listener, plain.per_listener, "seed {seed}");
        assert_eq!(shuffled.samples, plain.samples, "seed {seed}");
        assert_eq!(built, plain_built, "seed {seed}");
        // Pulls move with the order; every node, new ones included, still
        // runs once per instant.
        assert_eq!(runs, plain_runs, "seed {seed}");
        interleavings.insert(shuffled.interleaving);
        let (shuffled, live, runs) = run_constructs(Some(seed), automatic);
        assert_eq!(shuffled.per_listener, plain.per_listener, "seed {seed}");
        assert_eq!(shuffled.samples, plain.samples, "seed {seed}");
        assert_eq!(live, collected_live, "seed {seed}");
        assert_eq!(runs, collected_runs, "seed {seed}");
    }
    assert!(
        interleavings.len() >= 20,
        "the shuffle moved dispatch order ({} distinct interleavings of 25)",
        interleavings.len()
    );
}
