//! Order independence (RFD 1's shuffle affordance): the same program under
//! every shuffle seed gives the same values and the same events per node.
//! Only the interleaving of different nodes' listeners may move, and the
//! test checks that it does, so the affordance is known to shuffle.

use std::cell::RefCell;
use std::rc::Rc;

use bough::{Graph, Input, Source};

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
    let (mut graph, (inputs, streams, cells)) = Graph::build(|b| {
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
