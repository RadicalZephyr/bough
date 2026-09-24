//! Child transactions: the children `t ++ [n]` of an instant t, which
//! `split` starts, run after t's listeners, depth first, each a whole
//! transaction, and poison across them.
//!
//! The expected values are GHC's. The spike's scratchpad holds the program,
//! stage4-ghc/Stage4.hs, which runs each test's program over an unchanged
//! copy of the vendored Denotational.hs with the oracle's patch F7, Split's
//! output sorted stably by time, and solves each loop by fixed-point
//! iteration; stage4-ghc/output.txt is its output, and each test quotes the
//! lines it uses. Instant [k] is the k-th transaction after the build, and
//! each instant's sends are one transaction.
//!
//! The engine does not show child indices, so the tests compare values and
//! their order, and with the `statistics` feature the instants each
//! transaction ran, its own and its children's, which pins how the elements
//! were grouped into children. Every test runs under the plain order and
//! several shuffle seeds, and each must give GHC's values under all of them.

use std::cell::RefCell;
use std::fmt::Debug;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use bough::{Graph, SendError, Source, TokenError};

/// The plain order, then seeds for RFD 1's order shuffle.
const SEEDS: [Option<u64>; 6] = [None, Some(0), Some(1), Some(7), Some(42), Some(1 << 40)];

/// Runs a program under the plain order and every seed, checks that every
/// run observed the same, and returns what the plain order observed.
fn every_seed<R: PartialEq + Debug>(program: impl Fn(Option<u64>) -> R) -> R {
    let plain = program(None);
    for seed in &SEEDS[1..] {
        assert_eq!(program(*seed), plain, "seed {seed:?}");
    }
    plain
}

fn recorder<T: 'static>() -> (Rc<RefCell<Vec<T>>>, impl FnMut(T) + 'static) {
    let log = Rc::new(RefCell::new(Vec::new()));
    let writer = log.clone();
    (log, move |v| writer.borrow_mut().push(v))
}

/// The instants run since the graph was built, with the `statistics`
/// feature.
#[cfg(feature = "statistics")]
fn instants(graph: &Graph) -> Option<u64> {
    Some(graph.statistics().transactions)
}

#[cfg(not(feature = "statistics"))]
fn instants(_: &Graph) -> Option<u64> {
    None
}

/// Drives the graph once and returns the instants that ran: the
/// transaction's own and one per child transaction, at any depth.
fn counted(graph: &mut Graph, drive: impl FnOnce(&mut Graph)) -> Option<u64> {
    let before = instants(graph);
    drive(graph);
    Some(instants(graph)? - before?)
}

/// Checks the instants each transaction ran, where the feature counts them.
fn assert_instants<const N: usize>(seen: [Option<u64>; N], expected: [u64; N]) {
    if seen.iter().all(Option::is_some) {
        assert_eq!(
            seen.map(Option::unwrap),
            expected,
            "instants per transaction"
        );
    }
}

/// Child n takes element n of every split that fired at the parent
/// instant, so a merge sees the elements at one index as simultaneous and
/// combines them, left first; `f` is not commutative. An empty list takes
/// no part. Stage4.hs, `twoSplits`:
///
/// ```text
/// two splits: merged: [([1,0],130),([1,1],240),([1,2],50),([2,0],7),([2,1],8),([3,0],9),([4,0],5)]
/// ```
#[test]
fn two_splits_in_one_instant_share_child_indices() {
    let (events, last, counts) = every_seed(|seed| {
        let (mut graph, (l_in, r_in, merged, held)) = Graph::build(|b| {
            let (l, l_in) = b.input::<Vec<u32>>();
            let (r, r_in) = b.input::<Vec<u32>>();
            let right = r.split(b);
            let merged = l.split(b).merge(b, right, |l, r| l * 100 + r).share(b);
            let held = merged.hold(b, 0u32);
            (l_in, r_in, merged, held)
        });
        graph.set_shuffle_seed(seed);
        let (seen, on) = recorder();
        graph.listen(merged, on).keep();
        let counts = [
            counted(&mut graph, |g| {
                g.transaction(|tx| {
                    tx.send(l_in, vec![1, 2]);
                    tx.send(r_in, vec![30, 40, 50]);
                })
            }),
            counted(&mut graph, |g| g.send(r_in, vec![7, 8])),
            counted(&mut graph, |g| {
                g.transaction(|tx| {
                    tx.send(l_in, vec![]);
                    tx.send(r_in, vec![9]);
                })
            }),
            counted(&mut graph, |g| {
                g.transaction(|tx| {
                    tx.send(l_in, vec![5]);
                    tx.send(r_in, vec![]);
                })
            }),
        ];
        (seen.take(), *graph.sample(held), counts)
    });
    assert_eq!(events, [130, 240, 50, 7, 8, 9, 5]);
    assert_eq!(last, 5);
    // One child instant per index, not per element.
    assert_instants(counts, [1 + 3, 1 + 2, 1 + 1, 1 + 1]);
}

/// A split fed by its own children, the program of finding F7: each
/// element below 10 comes back as two elements one level down, and they
/// run before the next element of the level above. The text's Split gives
/// 1, 2, 10, 11, 20, 21, out of time order; the sorted Split and the engine
/// give the order below. A second split, outside the loop, fires in the
/// same transaction and shares the top level's indices, and only those.
/// Stage4.hs, `depthFirst`:
///
/// ```text
/// depth first: items (F7 sorted): [([1,0],1),([1,0,0],10),([1,0,1],11),([1,1],2),([1,1,0],20),([1,1,1],21),([2,0],3),([2,0,0],30),([2,0,1],31)]
/// depth first: items (the text's Split): [([1,0],1),([1,1],2),([1,0,0],10),([1,0,1],11),([1,1,0],20),([1,1,1],21),([2,0],3),([2,0,0],30),([2,0,1],31)]
/// depth first: merged with a split outside the loop: [([1,0],1007),([1,0,0],10),([1,0,1],11),([1,1],2008),([1,1,0],20),([1,1,1],21),([1,2],9),([2,0],3),([2,0,0],30),([2,0,1],31)]
/// ```
///
/// Closing the loop through the split passes the cycle check: the split's
/// output does not depend on its input.
#[test]
fn children_run_depth_first_and_a_split_may_fire_inside_its_own_children() {
    let (items, both, counts) = every_seed(|seed| {
        let (mut graph, (lists_in, others_in, items, both)) = Graph::build(|b| {
            let (fwd, fwd_loop) = b.stream_loop::<Vec<u32>>();
            let items = fwd.split(b).share(b);
            let (lists, lists_in) = b.input::<Vec<u32>>();
            let again = items.filter(|n| *n < 10).map(|n| vec![n * 10, n * 10 + 1]);
            let definition = lists.or_else(b, again);
            fwd_loop.close(b, definition);
            let (others, others_in) = b.input::<Vec<u32>>();
            let others = others.split(b);
            let both = items.merge(b, others, |i, o| i * 1000 + o);
            (lists_in, others_in, items, both)
        });
        graph.set_shuffle_seed(seed);
        let (items_seen, on_item) = recorder();
        graph.listen(items, on_item).keep();
        let (both_seen, on_both) = recorder();
        graph.listen(both, on_both).keep();
        let counts = [
            counted(&mut graph, |g| {
                g.transaction(|tx| {
                    tx.send(lists_in, vec![1, 2]);
                    tx.send(others_in, vec![7, 8, 9]);
                })
            }),
            counted(&mut graph, |g| g.send(lists_in, vec![3])),
        ];
        (items_seen.take(), both_seen.take(), counts)
    });
    assert_eq!(items, [1, 10, 11, 2, 20, 21, 3, 30, 31]);
    assert_eq!(both, [1007, 10, 11, 2008, 20, 21, 9, 3, 30, 31]);
    // [1,0], [1,0,0], [1,0,1], [1,1], [1,1,0], [1,1,1], [1,2]; then
    // [2,0], [2,0,0], [2,0,1].
    assert_instants(counts, [1 + 7, 1 + 3]);
}

/// R7: each element below 3 comes back as the one-element list [n + 1], so
/// the loop runs each round one level inside the one before. Stage4.hs,
/// `r7`:
///
/// ```text
/// r7: items: [([1,0],0),([1,0,0],1),([1,0,0,0],2),([1,0,0,0,0],3)]
/// ```
#[test]
fn r7_a_loop_through_split_feeds_back_at_nested_child_instants() {
    let (items, counts) = every_seed(|seed| {
        let (mut graph, (lists_in, items)) = Graph::build(|b| {
            let (fwd, fwd_loop) = b.stream_loop::<Vec<u32>>();
            let items = fwd.split(b).share(b);
            let (lists, lists_in) = b.input::<Vec<u32>>();
            let again = items.filter(|n| *n < 3).map(|n| vec![n + 1]);
            let definition = lists.or_else(b, again);
            fwd_loop.close(b, definition);
            (lists_in, items)
        });
        graph.set_shuffle_seed(seed);
        let (seen, on) = recorder();
        graph.listen(items, on).keep();
        let counts = [counted(&mut graph, |g| g.send(lists_in, vec![0]))];
        (seen.take(), counts)
    });
    assert_eq!(items, [0, 1, 2, 3]);
    assert_instants(counts, [1 + 4]);
}

/// Nested splits with no loop: the inner split fires in each child of the
/// outer one, and its own children run there, before the outer's next
/// child. A row's length, tagged 100 + n, marks each outer child in the
/// merge. An empty row fires the inner split with nothing to emit.
/// Stage4.hs, `nested`:
///
/// ```text
/// nested: merged: [([1,0],102),([1,0,0],1),([1,0,1],2),([1,1],101),([1,1,0],3),([2,0],100),([2,1],101),([2,1,0],4)]
/// ```
#[test]
fn nested_splits_run_the_inner_children_before_the_next_outer_child() {
    let (merged, counts) = every_seed(|seed| {
        let (mut graph, (lists_in, merged)) = Graph::build(|b| {
            let (lists, lists_in) = b.input::<Vec<Vec<u32>>>();
            let rows = lists.split(b).share(b);
            let items = rows.split(b);
            let merged = rows
                .map(|r| 100 + r.len() as u32)
                .merge(b, items, |l, i| l * 1000 + i);
            (lists_in, merged)
        });
        graph.set_shuffle_seed(seed);
        let (seen, on) = recorder();
        graph.listen(merged, on).keep();
        let counts = [
            counted(&mut graph, |g| g.send(lists_in, vec![vec![1, 2], vec![3]])),
            counted(&mut graph, |g| g.send(lists_in, vec![vec![], vec![4]])),
        ];
        (seen.take(), counts)
    });
    assert_eq!(merged, [102, 1, 2, 101, 3, 100, 101, 4]);
    assert_instants(counts, [1 + 5, 1 + 3]);
}

/// Every child is a whole transaction: a hold steps and commits in each, so
/// a snapshot in the next child reads it, and so does an accumulator; a
/// map_cell over the hold steps in each child, and its steps view carries
/// the value after that child. A sample after `send` returns reads what the
/// last child committed. Stage4.hs, `holdInChildren`:
///
/// ```text
/// hold in children: seen: [([1,0],50),([1,1],65),([1,2],76),([2,0],87)]
/// hold in children: steps h: [([1,0],5),([1,1],6),([1,2],7),([2,0],8)]
/// hold in children: steps (map_cell h (*2)): [([1,0],10),([1,1],12),([1,2],14),([2,0],16)]
/// hold in children: sample h after [1], after [2]: (7,8)
/// hold in children: sample total after [1], after [2]: (18,26)
/// ```
#[test]
fn a_hold_stepping_in_a_child_is_read_by_a_snapshot_in_the_next() {
    let run = every_seed(|seed| {
        let (mut graph, (lists_in, seen, held, doubled, total)) = Graph::build(|b| {
            let (lists, lists_in) = b.input::<Vec<u32>>();
            let items = lists.split(b).share(b);
            let held = items.hold(b, 0u32);
            let seen = items.snapshot(held, |i, p| i * 10 + p).node(b);
            let doubled = held.map_cell(b, |h| h * 2).steps(b);
            let total = items.accumulate(b, 0u32, |i, t| t + i);
            (lists_in, seen, held, doubled, total)
        });
        graph.set_shuffle_seed(seed);
        let (seen_log, on_seen) = recorder();
        graph.listen(seen, on_seen).keep();
        let (steps_log, mut on_step) = recorder();
        graph.listen_steps(held, move |h| on_step(*h)).keep();
        let (doubled_log, on_doubled) = recorder();
        graph.listen(doubled, on_doubled).keep();
        let mut samples = Vec::new();
        graph.send(lists_in, vec![5, 6, 7]);
        samples.push((*graph.sample(held), *graph.sample(total)));
        graph.send(lists_in, vec![8]);
        samples.push((*graph.sample(held), *graph.sample(total)));
        (
            seen_log.take(),
            steps_log.take(),
            doubled_log.take(),
            samples,
        )
    });
    let (seen, steps, doubled, samples) = run;
    assert_eq!(seen, [50, 65, 76, 87]);
    assert_eq!(steps, [5, 6, 7, 8]);
    assert_eq!(doubled, [10, 12, 14, 16]);
    assert_eq!(samples, [(7, 18), (8, 26)]);
}

/// listen_steps on a cell that steps in two children of one transaction
/// fires twice, in order, each after its child's commit; listen_cell fires
/// at registration and then twice. Stage4.hs, `twoSteps`:
///
/// ```text
/// two steps: steps h: [([1,0],5),([1,1],6)]
/// two steps: sample after [1], after [2]: (6,6)
/// ```
#[test]
fn listen_steps_on_a_cell_stepping_in_two_children_fires_twice_in_order() {
    let run = every_seed(|seed| {
        let (mut graph, (lists_in, held)) = Graph::build(|b| {
            let (lists, lists_in) = b.input::<Vec<u32>>();
            (lists_in, lists.split(b).hold(b, 0u32))
        });
        graph.set_shuffle_seed(seed);
        let (steps, mut on_step) = recorder();
        graph.listen_steps(held, move |h| on_step(*h)).keep();
        let (cells, mut on_cell) = recorder();
        graph.listen_cell(held, move |h| on_cell(*h)).keep();
        let counts = [
            counted(&mut graph, |g| g.send(lists_in, vec![5, 6])),
            counted(&mut graph, |g| g.send(lists_in, vec![])),
        ];
        let last = *graph.sample(held);
        (steps.take(), cells.take(), last, counts)
    });
    let (steps, cells, last, counts) = run;
    assert_eq!(steps, [5, 6]);
    assert_eq!(cells, [0, 5, 6]);
    assert_eq!(last, 6);
    assert_instants(counts, [1 + 2, 1]);
}

/// Transaction zero has children (finding F11): a steps_with_current built
/// in the build fires at [0], a split of it fires its elements at [0,0],
/// [0,1] and [0,2], and Graph::build runs them before it returns. The seed
/// is set after the build, so it moves only the transaction after it.
/// Stage4.hs, `txZero`:
///
/// ```text
/// tx zero: split items: [([0,0],1),([0,1],2),([0,2],3),([1,0],4),([1,1],5)]
/// tx zero: sum, last at [1]: (6,3)
/// tx zero: sum, last after [1]: (15,5)
/// ```
#[test]
fn transaction_zero_runs_its_children_before_build_returns() {
    let run = every_seed(|seed| {
        let (mut graph, (rows_in, sum, last)) = Graph::build(|b| {
            let (rows, rows_in) = b.input_cell(vec![1u32, 2, 3]);
            let items = rows.steps_with_current(b).split(b).share(b);
            let sum = items.accumulate(b, 0u32, |i, s| s + i);
            (rows_in, sum, items.hold(b, 0u32))
        });
        let built = (*graph.sample(sum), *graph.sample(last), instants(&graph));
        graph.set_shuffle_seed(seed);
        graph.send(rows_in, vec![4, 5]);
        (built, (*graph.sample(sum), *graph.sample(last)))
    });
    let ((sum, last, built), after) = run;
    assert_eq!((sum, last), (6, 3));
    if let Some(built) = built {
        assert_eq!(built, 1 + 3, "transaction zero and its three children");
    }
    // [1] steps the input cell, whose steps_with_current fires the split.
    assert_eq!(after, (15, 5));
}

/// A split of an empty list emits nothing, and the transaction runs no
/// child instant. Stage4.hs, `emptyList`:
///
/// ```text
/// empty list: items: [([2,0],4)]
/// ```
#[test]
fn a_split_of_an_empty_list_emits_nothing() {
    let (items, counts) = every_seed(|seed| {
        let (mut graph, (lists_in, items)) = Graph::build(|b| {
            let (lists, lists_in) = b.input::<Vec<u32>>();
            (lists_in, lists.split(b))
        });
        graph.set_shuffle_seed(seed);
        let (seen, on) = recorder();
        graph.listen(items, on).keep();
        let counts = [
            counted(&mut graph, |g| g.send(lists_in, vec![])),
            counted(&mut graph, |g| g.send(lists_in, vec![4])),
        ];
        (seen.take(), counts)
    });
    assert_eq!(items, [4]);
    assert_instants(counts, [1, 1 + 1]);
}

/// An iterator that is not fused: after its first `None` it yields again.
struct Restarts(u32);

impl Iterator for Restarts {
    type Item = u32;
    fn next(&mut self) -> Option<u32> {
        self.0 += 1;
        match self.0 {
            1 => Some(1),
            3 => Some(99),
            _ => None,
        }
    }
}

/// An event's elements are what its iterator yields before its first
/// `None`, as a `for` loop or `collect` sees them, which is the list the
/// semantics splits. The scheduler asks each capture for one element per
/// child, and drops an iterator at its end, so a split beside a longer one
/// does not ask it again: 99 never appears.
#[test]
fn a_split_ends_at_its_iterators_first_none() {
    let (events, counts) = every_seed(|seed| {
        let (mut graph, (starts_in, others_in, merged)) = Graph::build(|b| {
            let (starts, starts_in) = b.input::<()>();
            let (others, others_in) = b.input::<Vec<u32>>();
            let others = others.split(b);
            let merged = starts
                .map(|()| Restarts(0))
                .split(b)
                .merge(b, others, |r, o| r * 100 + o);
            (starts_in, others_in, merged)
        });
        graph.set_shuffle_seed(seed);
        let (seen, on) = recorder();
        graph.listen(merged, on).keep();
        let counts = [counted(&mut graph, |g| {
            g.transaction(|tx| {
                tx.send(starts_in, ());
                tx.send(others_in, vec![7, 8, 9]);
            })
        })];
        (seen.take(), counts)
    });
    assert_eq!(events, [107, 8, 9]);
    assert_instants(counts, [1 + 3]);
}

/// A split is two nodes: the one that takes the event and the one that
/// emits the elements.
#[test]
fn a_split_is_two_nodes() {
    let (graph, _) = Graph::build(|b| {
        let (lists, lists_in) = b.input::<Vec<u32>>();
        (lists_in, lists.map(|l| l).split(b))
    });
    assert_eq!(graph.live_nodes(), 3);
}

/// The transaction-in-progress flag stays set across every child, so a
/// panic in a child's listener poisons the graph. The child before it ran
/// whole; the panicking child had committed; the child after it never ran.
#[test]
fn a_panic_in_a_childs_listener_poisons_the_graph() {
    let (mut graph, (lists_in, items, held)) = Graph::build(|b| {
        let (lists, lists_in) = b.input::<Vec<u32>>();
        let items = lists.split(b).share(b);
        (lists_in, items, items.hold(b, 0u32))
    });
    let (seen, on) = recorder();
    graph.listen(items, on).keep();
    graph
        .listen(items, |n| {
            assert_ne!(n, 2, "a listener panics in child [1,1]")
        })
        .keep();
    let result = catch_unwind(AssertUnwindSafe(|| graph.send(lists_in, vec![1, 2, 3])));
    assert!(result.is_err());
    assert_eq!(*seen.borrow(), [1, 2]);
    assert_eq!(graph.try_send(lists_in, vec![4]), Err(SendError::Poisoned));
    assert_eq!(graph.try_sample(held), Err(TokenError::Poisoned));
    assert!(graph.try_transaction(|_| ()).is_err());
    assert_eq!(*seen.borrow(), [1, 2], "nothing ran after the panic");
}

/// A split's iterator runs between child instants, as user code inside the
/// transaction, so a panic in it poisons the graph too.
#[test]
fn a_panic_in_a_splits_iterator_poisons_the_graph() {
    let (mut graph, (lists_in, items)) = Graph::build(|b| {
        let (lists, lists_in) = b.input::<Vec<u32>>();
        // The event is a lazy iterator, whose `next` the scheduler calls.
        let items = lists
            .map(|l| {
                l.into_iter()
                    .inspect(|&n| assert_ne!(n, 0, "the iterator panics at its second element"))
            })
            .split(b);
        (lists_in, items)
    });
    let (seen, on) = recorder();
    graph.listen(items, on).keep();
    let result = catch_unwind(AssertUnwindSafe(|| graph.send(lists_in, vec![1, 0, 2])));
    assert!(result.is_err());
    assert_eq!(*seen.borrow(), [1]);
    assert_eq!(graph.try_send(lists_in, vec![4]), Err(SendError::Poisoned));
}
