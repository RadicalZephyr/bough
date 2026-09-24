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

use bough::{Cell, Graph, Input, Local, PoisonedError, SendError, Source, TokenError, Transaction};

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

/// A listener's sink, and the log it writes.
fn recorder<T: 'static>() -> (Rc<RefCell<Vec<T>>>, impl FnMut(T) + 'static) {
    let log = Rc::new(RefCell::new(Vec::new()));
    let writer = log.clone();
    (log, move |v| writer.borrow_mut().push(v))
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

// ----------------------------------------------------------- what a closure builds

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
