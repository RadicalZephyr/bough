//! `construct` (RFD 2), the semantics' `Execute`: a closure that runs at
//! each event of a stream, in the middle of that event's transaction, with
//! the build context, so that logic is added after build.
//!
//! The expected values are GHC's. The spike's scratchpad holds the program,
//! stage6-ghc/Stage6.hs, which runs each test's program over an unchanged
//! copy of the vendored Denotational.hs with the oracle's patches F6 and F7
//! and solves each loop by fixed-point iteration, a loop declared in a
//! construct body inside the body. The text's `Execute` has no creation
//! time (F44): a construct built at t0 would also run its body at its
//! source's events before t0, which nothing built at t0 can observe, so
//! the program keeps the events at or after t0. Its `SwitchS` has no
//! creation time either, and a switch_stream built at t0 is compared from
//! t0 on. stage6-ghc/output.txt is its output, and each test quotes the
//! lines it uses. Instant `[0]` is the build and `[k]` the k-th
//! transaction after it; a sample inside a closure at `[k]` reads the
//! value before `[k]`.
//!
//! Every test runs under the plain order and five shuffle seeds, and where
//! the order of sends could matter, with each transaction's sends in both
//! orders; each must give GHC's values under all of them. Cells and logs a
//! closure builds reach the test through a listener on the construct's
//! stream: the test receives them, then samples them after each
//! transaction from the one that built them on.

use std::cell::RefCell;
use std::fmt::Debug;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use bough::{
    Build, Cell, Graph, Input, Local, PoisonedError, SendError, Source, TokenError, Trace,
    Transaction,
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

fn send<A: Clone + 'static>(input: Input<A>, value: A) -> Send {
    Box::new(move |tx| tx.send(input, value.clone()))
}

/// Runs one transaction with `sends` in their order, or reversed.
fn run(graph: &mut Graph, sends: &[Send], reversed: bool) {
    graph.transaction(|tx| {
        if reversed {
            sends.iter().rev().for_each(|s| s(tx));
        } else {
            sends.iter().for_each(|s| s(tx));
        }
    });
}

/// A cell holding every step of `cell`, built beside it, so that it
/// records the step at the instant it is built too.
fn log_steps<A: Clone + Trace + 'static>(b: &mut Build, cell: Cell<A>) -> Cell<Vec<A>> {
    let steps = cell.steps(b);
    log_events(b, steps)
}

/// A cell holding every event of `stream`.
fn log_events<S>(b: &mut Build, stream: S) -> Cell<Vec<S::Event>>
where
    S: Source,
    S::Event: Clone + Trace + 'static,
{
    stream.accumulate(b, Vec::new(), |v, log: &Vec<S::Event>| {
        let mut log = log.clone();
        log.push(v);
        log
    })
}

/// A cell's steps or a stream's events the way GHC prints them: `(k,
/// value)` for instant `[k]`, or for the top-level transaction k of a child
/// instant, which the engine does not show.
type ByInstant<T> = Vec<(usize, T)>;

/// A log sampled after each transaction from transaction `first` on, as
/// the steps or events of each instant.
fn by_instant<T: Clone>(first: usize, logs: &[Vec<T>]) -> ByInstant<T> {
    let mut out = Vec::new();
    let mut seen = 0;
    for (k, log) in logs.iter().enumerate() {
        out.extend(log[seen..].iter().map(|v| (first + k, v.clone())));
        seen = log.len();
    }
    out
}

/// A listener's sink, and the log it writes.
fn recorder<T: 'static>() -> (Rc<RefCell<Vec<T>>>, impl FnMut(T) + 'static) {
    let log = Rc::new(RefCell::new(Vec::new()));
    let writer = log.clone();
    (log, move |v| writer.borrow_mut().push(v))
}

/// Runs `schedule`, one transaction per entry, in `order`, and observes
/// the graph after each.
fn drive<S>(
    graph: &mut Graph,
    order: Order,
    schedule: &[Vec<Send>],
    observe: impl Fn(&Graph) -> S,
) -> Vec<S> {
    graph.set_shuffle_seed(order.seed);
    schedule
        .iter()
        .map(|sends| {
            run(graph, sends, order.reversed);
            observe(graph)
        })
        .collect()
}

/// Observations after transactions 1, 2, ... of the things closures had
/// built by then, in the order they were built, regrouped per thing: the
/// transaction that built it, and its observations from then on.
fn per_item<T: Clone>(observed: &[Vec<T>]) -> Vec<(usize, Vec<T>)> {
    let mut items: Vec<(usize, Vec<T>)> = Vec::new();
    for (k, seen) in observed.iter().enumerate() {
        for (j, v) in seen.iter().enumerate() {
            if j == items.len() {
                items.push((k + 1, Vec::new()));
            }
            items[j].1.push(v.clone());
        }
    }
    items
}

/// The nodes run since the graph was built, in order, pulled out of it, or
/// as new nodes, with the `statistics` feature: the same under every order
/// when each node runs once per instant.
#[cfg(feature = "statistics")]
fn runs(graph: &Graph) -> Option<u64> {
    let s = graph.statistics();
    Some(s.evaluations + s.pulls + s.new_nodes)
}

#[cfg(not(feature = "statistics"))]
fn runs(_: &Graph) -> Option<u64> {
    None
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

/// Every entry reports the poison after a panic escaped a transaction.
fn assert_poisoned<A: Copy + 'static>(
    graph: &mut Graph,
    input: Input<A>,
    value: A,
    cell: Cell<u32>,
) {
    assert_eq!(graph.try_send(input, value), Err(SendError::Poisoned));
    assert_eq!(graph.try_transaction(|_| ()), Err(PoisonedError));
    assert_eq!(graph.try_sample(cell).err(), Some(TokenError::Poisoned));
    assert!(panic_message(|| graph.send(input, value)).contains("poisoned"));
}

// ----------------------------------------------------------- claim 4

/// Claim 4: a construct at [1] builds a hold over `e`, which fires at [1]
/// too. The hold picks e's event up at its creation instant in either send
/// order, and a sample inside the closure reads the value before [1]. The
/// map is fused into the hold, so the closure adds one node. Stage6.hs:
///
/// ```text
/// claim4: sample inside: 0
/// claim4: samples h after [1], [2]: [107,101]
/// ```
#[test]
fn claim4_construct_creates_nodes_while_a_closure_runs_in_both_send_orders() {
    let got = every_order(|order| {
        let (inside, mut on_inside) = recorder::<u32>();
        let (mut graph, (e_in, go_in, made)) = Graph::build(move |b| {
            let (e, e_in) = b.input::<u32>();
            let e = e.share(b);
            let (go, go_in) = b.input::<u32>();
            let zero = b.constant(0u32);
            let made = go
                .construct(b, move |b, k| {
                    let h = e.map(move |v| v + k).hold(b, 0u32);
                    on_inside(*h.sample(b));
                    h
                })
                .hold(b, zero);
            (e_in, go_in, made)
        });
        graph.set_shuffle_seed(order.seed);
        let before = graph.live_nodes();
        run(
            &mut graph,
            &[send(e_in, 7), send(go_in, 100)],
            order.reversed,
        );
        assert_eq!(
            graph.live_nodes(),
            before + 1,
            "one node built mid-transaction"
        );
        let h = *graph.sample(made);
        let first = *graph.sample(h);
        graph.send(e_in, 1);
        (inside.borrow().clone(), [first, *graph.sample(h)])
    });
    assert_eq!(got, (vec![0], [107, 101]));
}

// ----------------------------------------------------------- loops in closures

/// R1 and R1c (review-fidelity/hs/Review.hs): a cell loop declared inside
/// a construct closure, a map_cell over its forward built before the
/// definition, and a snapshot of the map_cell. The closure's nodes run by
/// pull over their dependencies, so the map_cell settles after the loop,
/// and the loop after its definition, which is built last: the map_cell
/// steps at [1] with the loop, and its memo, filled at [1] from the value
/// before [1] for the snapshot, does not survive the commit. A pass in
/// creation order would settle the map_cell first, find nothing stepped,
/// and keep the memo: label 0 after [1], and a snapshot of 0 at [2].
/// Stage6.hs:
///
/// ```text
/// R1: samples total, label, seen after [1], [2]: ([5,12],[50,120],[0,50])
/// R1: steps label: (0,[([1],50),([2],120)])
/// ```
#[test]
fn r1_a_loop_in_a_construct_closure_with_readers_built_before_its_definition() {
    let (values, steps) = every_order(|order| {
        let (mut graph, (s_in, made)) = Graph::build(|b| {
            let (s, s_in) = b.input::<u32>();
            let s = s.share(b);
            let made = s.construct(b, move |b, _| {
                let (total, total_loop) = b.cell_loop::<u32>();
                let label = total.map_cell(b, |t| t * 10);
                let seen = s.snapshot(label, |_, l| *l).hold(b, 99u32);
                let next = s.snapshot(total, |x, t| t + x).hold(b, 0u32);
                total_loop.close(b, next);
                ([total, label, seen], log_steps(b, label))
            });
            (s_in, made)
        });
        let (received, on) = recorder();
        graph.listen(made, on).keep();
        let schedule = [vec![send(s_in, 5)], vec![send(s_in, 7)]];
        let observed = drive(&mut graph, order, &schedule, |graph| {
            let (cells, log) = received.borrow()[0];
            (cells.map(|c| *graph.sample(c)), graph.sample(log).clone())
        });
        let values: Vec<[u32; 3]> = observed.iter().map(|(v, _)| *v).collect();
        let logs: Vec<Vec<u32>> = observed.into_iter().map(|(_, l)| l).collect();
        (values, by_instant(1, &logs))
    });
    assert_eq!(values, [[5, 50, 0], [12, 120, 50]]);
    assert_eq!(steps, [(1, 50), (2, 120)]);
}

/// R2 and R2b: a stream loop declared in a construct closure, with a hold
/// over its forward built before the definition, which fires at the
/// closure's instant: `Hold 0 fwd t0` keeps t >= t0, 5 + 100. In R2 the
/// definition is a chain over the closure's source; in R2b it is a node
/// built after the forward, which the forward's pull runs first. Each
/// event builds a loop of its own. Stage6.hs, where the two are one
/// program:
///
/// ```text
/// R2: samples of each held after [1], [2]: [[105,107],[147]]
/// ```
fn r2(definition_is_a_node: bool) -> Vec<(usize, Vec<u32>)> {
    every_order(|order| {
        let (mut graph, (s_in, made)) = Graph::build(move |b| {
            let (s, s_in) = b.input::<u32>();
            let s = s.share(b);
            let made = s.construct(b, move |b, n| {
                let (forward, forward_loop) = b.stream_loop::<u32>();
                let held = forward.hold(b, 0u32);
                if definition_is_a_node {
                    let definition = s.map(move |x| x + n * 20).node(b);
                    forward_loop.close(b, definition);
                } else {
                    forward_loop.close(b, s.map(move |x| x + n * 20));
                }
                held
            });
            (s_in, made)
        });
        let (received, on) = recorder::<Cell<u32>>();
        graph.listen(made, on).keep();
        let schedule = [vec![send(s_in, 5)], vec![send(s_in, 7)]];
        let observed = drive(&mut graph, order, &schedule, |graph| {
            let held = received.borrow();
            held.iter().map(|h| *graph.sample(*h)).collect::<Vec<_>>()
        });
        per_item(&observed)
    })
}

#[test]
fn r2_a_stream_loop_in_a_construct_closure_with_a_hold_built_before_its_definition() {
    assert_eq!(r2(false), [(1, vec![105, 107]), (2, vec![147])]);
}

#[test]
fn r2b_a_stream_loop_whose_definition_is_a_node_built_after_its_forward() {
    assert_eq!(r2(true), [(1, vec![105, 107]), (2, vec![147])]);
}

// ----------------------------------------------------------- what a closure builds

/// Everything a closure at [2] builds exists from [2]: a hold, an
/// accumulator, a scan and a once over s, which fires at [2], take its
/// event there; steps and steps_with_current of c, which steps at [2],
/// fire there, and so does steps of a map_cell of c; a split and a defer
/// of s have children of [2]; a snapshot of c, like a sample inside the
/// closure, reads c before [2]; and steps_with_current of q, which is
/// quiet at [2], fires there with q's value, as a creation. Stage6.hs:
///
/// ```text
/// creation: sample inside: 11
/// creation: samples after [2], [3]: [[2,3],[2,5],[200,301],[2,2],[12,13],[12,13],[24,26],[22,55],[1002,1003],[211,312],[40,43]]
/// ```
#[test]
fn everything_a_closure_builds_exists_from_its_instant() {
    let (inside, samples) = every_order(|order| {
        let (mut graph, ((s_in, c_in, q_in, go_in), made)) = Graph::build(|b| {
            let (s, s_in) = b.input::<u32>();
            let s = s.share(b);
            let (c, c_in) = b.input_cell(10u32);
            let (q, q_in) = b.input_cell(40u32);
            let (go, go_in) = b.input::<()>();
            let made = go.construct(b, move |b, ()| {
                let held = s.hold(b, 0u32);
                let acc = s.accumulate(b, 0u32, |v, a| a + v);
                let scanned = s.scan(b, 0u32, |v, st| (v * 100 + st, st + 1));
                let scanned = scanned.hold(b, 0u32);
                let first = s.once().hold(b, 0u32);
                let with_current = c.steps_with_current(b).hold(b, 0u32);
                let updates = c.steps(b).hold(b, 0u32);
                let mapped = c.map_cell(b, |v| v * 2).steps(b).hold(b, 0u32);
                let pieces = s.map(|v| [v, v * 10]).split(b);
                let pieces = pieces.accumulate(b, 0u32, |v, a| a + v);
                let later = s.map(|v| v + 1000).defer(b).hold(b, 0u32);
                let snap = s.snapshot(c, |v, w| v * 100 + w).hold(b, 0u32);
                let quiet = q.steps_with_current(b).hold(b, 0u32);
                let cells = [
                    held,
                    acc,
                    scanned,
                    first,
                    with_current,
                    updates,
                    mapped,
                    pieces,
                    later,
                    snap,
                    quiet,
                ];
                (*c.sample(b), cells)
            });
            ((s_in, c_in, q_in, go_in), made)
        });
        let (received, on) = recorder();
        graph.listen(made, on).keep();
        let schedule = [
            vec![send(s_in, 1), send(c_in, 11)],
            vec![send(s_in, 2), send(c_in, 12), send(go_in, ())],
            vec![send(s_in, 3), send(c_in, 13), send(q_in, 43)],
        ];
        let observed = drive(&mut graph, order, &schedule, |graph| {
            let received = received.borrow();
            received
                .iter()
                .map(|(inside, cells)| (*inside, cells.map(|c| *graph.sample(c))))
                .collect::<Vec<_>>()
        });
        let items = per_item(&observed);
        assert_eq!(items.len(), 1);
        let (first, observed) = &items[0];
        assert_eq!(*first, 2, "built at [2]");
        let samples: Vec<[u32; 2]> = (0..11)
            .map(|i| [observed[0].1[i], observed[1].1[i]])
            .collect();
        (observed[0].0, samples)
    });
    assert_eq!(inside, 11);
    assert_eq!(
        samples,
        [
            [2, 3],
            [2, 5],
            [200, 301],
            [2, 2],
            [12, 13],
            [12, 13],
            [24, 26],
            [22, 55],
            [1002, 1003],
            [211, 312],
            [40, 43]
        ]
    );
}

/// A closure may build nodes that depend on its construct's own events,
/// through a stream loop: the event exists only once the closure has
/// returned, and the new nodes run after it, so a hold built at [k] over
/// the construct's events picks up the event of [k]. A switch_cell over the
/// holds reads the new one after the instant, which pulls it, and what it
/// depends on, when the switch's steps view runs before the loop's shared
/// node, as some seeds order it. Stage6.hs:
///
/// ```text
/// own output: outs: [([1],10),([2],20)]
/// own output: steps shown: (0,[([0],0),([1],11),([2],22)])
/// own output: samples of each hold, from its transaction: [[11,21],[22]]
/// ```
#[test]
fn nodes_a_closure_builds_may_depend_on_its_construct_s_own_events() {
    let (outs, shown, holds) = every_order(|order| {
        let (mut graph, (s_in, made, (outs, shown))) = Graph::build(|b| {
            let (s, s_in) = b.input::<u32>();
            let s = s.share(b);
            let (outs, outs_loop) = b.stream_loop::<u32>();
            let outs = outs.share(b);
            let made = s
                .construct(b, move |b, v| {
                    (v * 10, outs.map(move |o| o + v).hold(b, 0u32))
                })
                .share(b);
            outs_loop.close(b, made.map(|(n, _)| n));
            let c0 = b.constant(0u32);
            let shown = made.map(|(_, h)| h).hold(b, c0).switch_cell(b);
            (s_in, made, (log_events(b, outs), log_steps(b, shown)))
        });
        let (received, on) = recorder();
        graph.listen(made, on).keep();
        let at_build = graph.sample(shown).clone();
        let schedule = [vec![send(s_in, 1)], vec![send(s_in, 2)]];
        let observed = drive(&mut graph, order, &schedule, |graph| {
            let received = received.borrow();
            let holds: Vec<u32> = received.iter().map(|(_, h)| *graph.sample(*h)).collect();
            (
                graph.sample(outs).clone(),
                graph.sample(shown).clone(),
                holds,
            )
        });
        let outs: Vec<Vec<u32>> = observed.iter().map(|o| o.0.clone()).collect();
        let mut shown: Vec<Vec<u32>> = vec![at_build];
        shown.extend(observed.iter().map(|o| o.1.clone()));
        let holds: Vec<Vec<u32>> = observed.iter().map(|o| o.2.clone()).collect();
        (
            by_instant(1, &outs),
            by_instant(0, &shown),
            (per_item(&holds), runs(&graph)),
        )
    });
    assert_eq!(outs, [(1, 10), (2, 20)]);
    assert_eq!(shown, [(0, 0), (1, 11), (2, 22)]);
    assert_eq!(holds.0, [(1, vec![11, 21]), (2, vec![22])]);
}

/// An input built by a closure reaches I/O code as data: a listener on
/// the construct's stream receives its token by value, with a cell and a
/// linear stream built over it, and after `send` returns, I/O code
/// listens to them and sends to the input (RFD 2, RFD 4: receive, then
/// wire). Stage6.hs:
///
/// ```text
/// input inside: samples total after [1], [2], [3]: [100,105,111]
/// input inside: doubled: [([2],10),([3],12)]
/// ```
#[test]
fn an_input_built_by_a_closure_is_received_by_a_listener_and_wired_after_send() {
    let got = every_order(|order| {
        let (mut graph, (go_in, made)) = Graph::build(|b| {
            let (go, go_in) = b.input::<u32>();
            let made = go.construct(b, |b, k| {
                let (sends, sends_in) = b.input::<u32>();
                let sends = sends.share(b);
                let total = sends.accumulate(b, k, |x, t| t + x);
                let doubled = sends.map(|x| x * 2).node(b);
                (sends_in, total, doubled)
            });
            (go_in, made)
        });
        graph.set_shuffle_seed(order.seed);
        let (received, on) = recorder();
        graph.listen(made, on).keep();
        graph.send(go_in, 100);
        // Receive: the tokens, a linear stream's among them, by value.
        let (sends_in, total, doubled) =
            received.borrow_mut().pop().expect("the closure ran at [1]");
        // Then wire.
        let (events, on_event) = recorder();
        graph.listen(doubled, on_event).keep();
        let (values, mut on_value) = recorder();
        graph.listen_cell(total, move |t| on_value(*t)).keep();
        graph.send(sends_in, 5);
        graph.send(sends_in, 6);
        (values.borrow().clone(), events.borrow().clone())
    });
    assert_eq!(got, (vec![100, 105, 111], vec![10, 12]));
}

// ----------------------------------------------------------- nesting and children

/// A construct built inside a construct's closure at [k] runs its own
/// closure at [k] too, since its source fires then, in the new-node phase
/// of [k], and in every later instant its source fires, from the
/// evaluation loop. Each body builds a hold from its instant and a switch
/// over the holds; a switch built at [0] follows the latest body's switch.
/// Stage6.hs:
///
/// ```text
/// nested: steps top: (0,[([0],0),([1],111),([2],222),([3],333)])
/// nested: samples of each body's switch, from its instant: [[111,122,133],[222,233],[333]]
/// ```
#[test]
fn a_construct_built_by_a_closure_runs_at_its_creation_instant_and_after() {
    let (top, bodies) = every_order(|order| {
        let (mut graph, (s_in, made, top)) = Graph::build(|b| {
            let (s, s_in) = b.input::<u32>();
            let s = s.share(b);
            let c0 = b.constant(0u32);
            let made = s
                .construct(b, move |b, n| {
                    let inner = s.construct(b, move |b, m| {
                        s.map(move |x| x + 10 * m + 100 * n).hold(b, 0u32)
                    });
                    inner.hold(b, c0).switch_cell(b)
                })
                .share(b);
            let top = made.hold(b, c0).switch_cell(b);
            (s_in, made, log_steps(b, top))
        });
        let (received, on) = recorder::<Cell<u32>>();
        graph.listen(made, on).keep();
        let at_build = graph.sample(top).clone();
        let schedule = [
            vec![send(s_in, 1)],
            vec![send(s_in, 2)],
            vec![send(s_in, 3)],
        ];
        let observed = drive(&mut graph, order, &schedule, |graph| {
            let received = received.borrow();
            let bodies: Vec<u32> = received.iter().map(|sw| *graph.sample(*sw)).collect();
            (graph.sample(top).clone(), bodies)
        });
        let mut tops = vec![at_build];
        tops.extend(observed.iter().map(|o| o.0.clone()));
        let bodies: Vec<Vec<u32>> = observed.into_iter().map(|o| o.1).collect();
        (by_instant(0, &tops), per_item(&bodies))
    });
    assert_eq!(top, [(0, 0), (1, 111), (2, 222), (3, 333)]);
    assert_eq!(
        bodies,
        [
            (1, vec![111, 122, 133]),
            (2, vec![222, 233]),
            (3, vec![333])
        ]
    );
}

/// A split feeding a construct runs its closure in each child instant
/// [k, n], and what it builds exists from that child: a hold over the
/// split's items takes the item of [k, n], and a defer of them fires in
/// [k, n, 0], before [k, n + 1]. A construct built in the build over a
/// split of a steps_with_current of a constant runs its closure in the
/// build's own children, [0, 0] and [0, 1], before `build` returns. The
/// engine does not show child instants, so each step is placed at its
/// top-level transaction. Stage6.hs:
///
/// ```text
/// children: steps top: (0,[([0],0),([1,0],11),([1,1],22),([1,2],33),([2,0],44)])
/// children: steps later: (0,[([0],0),([1,0],0),([1,0,0],101),([1,1],0),([1,1,0],202),([1,2],0),([1,2,0],303),([2,0],0),([2,0,0],404)])
/// children: samples of each body's hold, from its transaction: [[31,41],[32,42],[33,43],[44]]
/// children: build's children: steps top0: (0,[([0],0),([0,0],5),([0,1],6),([1,0],7),([1,1],8),([1,2],9),([2,0],10)])
/// ```
#[test]
fn a_construct_fed_by_a_split_runs_its_closure_in_each_child_instant() {
    let (logs, holds) = every_order(|order| {
        let (mut graph, (lists_in, made, logs)) = Graph::build(|b| {
            let (lists, lists_in) = b.input::<Vec<u32>>();
            let items = lists.split(b).share(b);
            let c0 = b.constant(0u32);
            let made = items
                .construct(b, move |b, v| {
                    let held = items.map(move |x| x * 10 + v).hold(b, 0u32);
                    let later = items.map(move |x| x + v * 100).defer(b).hold(b, 0u32);
                    (held, later)
                })
                .share(b);
            let top = made.map(|(h, _)| h).hold(b, c0).switch_cell(b);
            let later = made.map(|(_, l)| l).hold(b, c0).switch_cell(b);
            let seed = b.constant(vec![5u32, 6]).steps_with_current(b).split(b);
            let made0 = seed.construct(b, move |b, v| items.map(move |x| x + v).hold(b, v));
            let top0 = made0.hold(b, c0).switch_cell(b);
            let logs = [top, later, top0].map(|c| log_steps(b, c));
            (lists_in, made, logs)
        });
        let (received, on) = recorder::<(Cell<u32>, Cell<u32>)>();
        graph.listen(made, on).keep();
        let at_build = logs.map(|l| graph.sample(l).clone());
        let schedule = [
            vec![send(lists_in, vec![1, 2, 3])],
            vec![send(lists_in, vec![4])],
        ];
        let observed = drive(&mut graph, order, &schedule, |graph| {
            let received = received.borrow();
            let holds: Vec<u32> = received.iter().map(|(h, _)| *graph.sample(*h)).collect();
            (logs.map(|l| graph.sample(l).clone()), holds)
        });
        let steps: Vec<ByInstant<u32>> = (0..3)
            .map(|i| {
                let mut sampled = vec![at_build[i].clone()];
                sampled.extend(observed.iter().map(|o| o.0[i].clone()));
                by_instant(0, &sampled)
            })
            .collect();
        let holds: Vec<Vec<u32>> = observed.into_iter().map(|o| o.1).collect();
        (steps, per_item(&holds))
    });
    assert_eq!(logs[0], [(0, 0), (1, 11), (1, 22), (1, 33), (2, 44)]);
    assert_eq!(
        logs[1],
        [
            (0, 0),
            (1, 0),
            (1, 101),
            (1, 0),
            (1, 202),
            (1, 0),
            (1, 303),
            (2, 0),
            (2, 404)
        ]
    );
    assert_eq!(
        logs[2],
        [(0, 0), (0, 5), (0, 6), (1, 7), (1, 8), (1, 9), (2, 10)]
    );
    assert_eq!(
        holds,
        [
            (1, vec![31, 41]),
            (1, vec![32, 42]),
            (1, vec![33, 43]),
            (2, vec![44])
        ]
    );
}

// ----------------------------------------------------------- scopes and refusals

/// Each run of a construct closure is a scope: a loop it declares and
/// leaves open is a panic when it returns, which poisons the graph.
#[test]
fn a_loop_left_open_in_a_construct_closure_panics_and_poisons() {
    let (mut graph, (go_in, latest)) = Graph::build(|b| {
        let (go, go_in) = b.input::<u32>();
        let go = go.share(b);
        let _made = go.construct(b, |b, _| {
            let (forward, _closer) = b.cell_loop::<u32>();
            forward
        });
        (go_in, go.hold(b, 0u32))
    });
    let message = panic_message(|| graph.send(go_in, 1));
    assert!(
        message.contains("a loop declared in this scope was never closed"),
        "{message}"
    );
    assert_poisoned(&mut graph, go_in, 2, latest);
}

/// A closer smuggled out of the build into a construct closure through an
/// `Option` compiles, and the build's scope still panics at its end, since
/// the loop is open when the scope ends (RFD 2).
#[test]
fn a_closer_smuggled_into_a_construct_closure_leaves_its_scope_open() {
    let message = panic_message(|| {
        Graph::build(|b| {
            let (forward, closer) = b.cell_loop::<u32>();
            let mut closer = Some(closer);
            let (go, _go_in) = b.input::<u32>();
            let _made = go.construct(b, move |b, n| {
                if let Some(closer) = closer.take() {
                    let definition = b.constant(n);
                    closer.close(b, definition);
                }
            });
            forward
        })
    });
    assert!(
        message.contains("a loop declared in this scope was never closed"),
        "{message}"
    );
}

/// A cycle built at t is refused where it is made, and the panic names it
/// and poisons the graph: a loop a closure closes with a cell computed
/// from its own forward, at close.
#[test]
fn a_loop_a_closure_closes_into_a_same_instant_cycle_is_refused_at_close() {
    let (mut graph, (go_in, latest)) = Graph::build(|b| {
        let (go, go_in) = b.input::<u32>(); // node 1
        let go = go.share(b); // node 2
        let _made = go.construct(b, |b, _| {
            let (forward, closer) = b.cell_loop::<u32>(); // node 5
            let next = forward.map_cell(b, |v| v + 1); // node 6
            closer.close(b, next);
        }); // node 3
        (go_in, go.hold(b, 0u32)) // node 4
    });
    let message = panic_message(|| graph.send(go_in, 1));
    assert!(
        message.contains(
            "closing this loop makes a same-instant cycle: node 5 (Loop) -> node 6 \
             (ReadThrough) -> node 5"
        ),
        "{message}"
    );
    assert_poisoned(&mut graph, go_in, 2, latest);
}

/// A switch_cell a closure builds whose first link would close a cycle is
/// refused where it links, in the new-node phase of the closure's
/// instant: close could not see the cycle, since the switch had no inner
/// yet.
#[test]
fn a_switch_a_closure_builds_whose_first_link_closes_a_cycle_is_refused() {
    let (mut graph, (go_in, latest)) = Graph::build(|b| {
        let (go, go_in) = b.input::<u32>(); // node 1
        let go = go.share(b); // node 2
        let _made = go.construct(b, |b, _| {
            let (forward, closer) = b.cell_loop::<u32>(); // node 5
            let next = forward.map_cell(b, |v| v + 1); // node 6
            let outer = b.constant(next); // node 7
            let switched = outer.switch_cell(b); // node 8
            closer.close(b, switched);
        }); // node 3
        (go_in, go.hold(b, 0u32)) // node 4
    });
    let message = panic_message(|| graph.send(go_in, 1));
    assert!(
        message.contains(
            "switching closes a same-instant cycle: node 8 (SwitchCell) -> node 5 (Loop) -> \
             node 6 (ReadThrough) -> node 8"
        ),
        "{message}"
    );
    assert_poisoned(&mut graph, go_in, 2, latest);
}

/// A closure builds a stream from a switch_stream's own events, and the
/// construct's event, held, selects it: at commit the switch would follow
/// a stream computed from itself at the same instant. Relink's check
/// refuses the move, naming the cycle, and the graph is poisoned.
#[test]
fn a_switch_moved_onto_a_stream_a_closure_built_from_it_is_refused_at_relink() {
    let (mut graph, (go_in, x_in, total)) = Graph::build(|b| {
        let (x, x_in) = b.input::<u32>(); // node 1
        let x = x.share(b); // node 2
        let (go, go_in) = b.input::<()>(); // node 3
        let (forward, forward_loop) = b.stream_loop::<u32>(); // node 4
        let forward = forward.share(b); // node 5
        let made = go.construct(b, move |b, ()| forward.map(|v| v + 1).share(b)); // node 6
        let out = made.hold(b, x).switch_stream(b).share(b); // nodes 7, 8, 9
        forward_loop.close(b, out);
        let total = out.accumulate(b, 0u32, |v, t| t + v); // node 10
        (go_in, x_in, total)
    });
    graph.send(x_in, 5);
    assert_eq!(*graph.sample(total), 5);
    let message = panic_message(|| graph.send(go_in, ()));
    assert!(
        message.contains(
            "switching closes a same-instant cycle: node 8 (SwitchStream) -> node 9 (Stream) \
             -> node 4 (Stream) -> node 5 (Stream) -> node 11 (Stream) -> node 8"
        ),
        "{message}"
    );
    assert_poisoned(&mut graph, x_in, 1, total);
}

/// `mem::swap` of a closure's build context with a nested graph's is safe
/// code. The nested `build` refuses to finish with the swapped context;
/// if the closure catches that and returns, the construct's own check
/// finds a build context of another graph in place of its own and panics
/// before touching it, and the graph, which now holds the other context,
/// is poisoned.
#[test]
fn a_construct_closure_that_swaps_its_build_context_panics_and_poisons() {
    let (mut graph, (go_in, latest)) = Graph::build(|b| {
        let (go, go_in) = b.input::<u32>();
        let go = go.share(b);
        let _made = go.construct(b, |b, _| {
            let nested = catch_unwind(AssertUnwindSafe(|| {
                Graph::build(|other| std::mem::swap(b, other))
            }));
            assert!(nested.is_err(), "the nested build refused the swap");
        });
        (go_in, go.hold(b, 0u32))
    });
    let message = panic_message(|| graph.send(go_in, 1));
    assert!(
        message.contains("a construct closure swapped its build context for another graph's"),
        "{message}"
    );
    assert_eq!(graph.try_send(go_in, 2), Err(SendError::Poisoned));
    assert_eq!(graph.try_transaction(|_| ()), Err(PoisonedError));
    // The graph holds the nested graph's context, whose id no token of this
    // graph has; the poison is found first.
    assert_eq!(graph.try_sample(latest).err(), Some(TokenError::Poisoned));
}
