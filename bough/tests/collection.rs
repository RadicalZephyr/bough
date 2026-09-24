//! RFD 3's memory model: a node is alive while a root reaches it, and
//! collection frees the rest.
//!
//! The roots are the build closure's return value, every live listener
//! and every live anchor. A node reaches its dependencies, the tokens in a
//! stateful cell's committed value, the cells its chain reads, and what
//! `depends` declares. A token I/O code keeps without rooting it, which a
//! build closure hands out here through a side channel or a listener
//! hands out as data, names a node that is collected when nothing else
//! reaches it, and its next use is a stale-token error.

use std::any::Any;
use std::cell::{Cell as StdCell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use bough::{
    Build, Cell, CollectionPolicy, Graph, Input, Lift, Listener, PoisonedError, SendError, Shared,
    Source, State, Stream, TokenError, TokenRef, Trace, Tracer, TransactionSendError,
};

// ----------------------------------------------------------- helpers

/// A shared log and a closure that appends to it.
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
    text(&*payload)
}

fn text(payload: &(dyn Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else {
        String::new()
    }
}

/// One end of a side channel.
type Channel<T> = Rc<RefCell<Option<T>>>;

/// A side channel out of a build closure: the tokens I/O code holds
/// without rooting them, since what the closure returns is a root.
fn side_channel<T>() -> (Channel<T>, Channel<T>) {
    let channel = Rc::new(RefCell::new(None));
    (channel.clone(), channel)
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

// ----------------------------------------------------------- the collection shape

/// The brief's collection shape. A hold named only inside a constant's
/// value survives, since the constant is a root and its value is traced;
/// a hold nothing names is collected, and the token I/O code kept of it is
/// stale; and a send through the shared node whose dependents named the
/// collected hold works, since collection pruned it from that list.
/// Without the pruning, marking would reach the freed slot and run a node
/// that has no program.
#[test]
fn a_hold_named_in_a_constant_s_value_survives_and_an_unrooted_hold_is_collected() {
    let (lost_out, lost_in) = side_channel::<Cell<u32>>();
    let (mut graph, (x_in, named)) = Graph::build(move |b| {
        let (x, x_in) = b.input::<u32>();
        let x = x.share(b);
        let kept = x.hold(b, 0u32);
        let named = b.constant(kept);
        let lost = x.map(|v| v * 2).hold(b, 0u32);
        *lost_in.borrow_mut() = Some(lost);
        (x_in, named)
    });
    graph.set_collection_policy(CollectionPolicy::Manual);
    let lost = lost_out.borrow().expect("the build ran");
    assert_eq!(graph.live_nodes(), 5);
    assert_eq!(*graph.sample(lost), 0, "alive until a collection");
    graph.collect_garbage();
    assert_eq!(graph.live_nodes(), 4);
    assert_eq!(graph.try_sample(lost).err(), Some(TokenError::Stale));
    let kept = *graph.sample(named);
    graph.send(x_in, 5);
    assert_eq!(*graph.sample(kept), 5);
    graph.collect_garbage();
    assert_eq!(graph.live_nodes(), 4, "nothing more to collect");
}

// ----------------------------------------------------------- build, drive, drop, collect, compare

/// The live nodes at three points: after the build, with the listeners
/// attached after driving and a collection, and after the listeners were
/// dropped and a collection ran.
#[derive(Debug, PartialEq)]
struct Counts {
    built: usize,
    listened: usize,
    dropped: usize,
}

/// Build, drive, drop, collect, compare (RFD 3). The build closure returns
/// its inputs alone, which are permanent roots, and hands the rest of its
/// logic out through a side channel; listeners root it; after twenty
/// drives the handles are dropped and a collection must leave exactly the
/// inputs' nodes, and the tokens of what it freed stale.
fn build_drive_drop<L: 'static>(
    build: impl FnOnce(&mut Build) -> (Vec<Input<u32>>, L) + 'static,
    listen: impl FnOnce(&mut Graph, &L) -> Vec<Listener>,
    drive: impl Fn(&mut Graph, &[Input<u32>], u32),
    stale: impl FnOnce(&mut Graph, &L) -> bool,
) -> Counts {
    let (logic_out, logic_in) = side_channel::<L>();
    let (mut graph, inputs) = Graph::build(move |b| {
        let (inputs, logic) = build(b);
        *logic_in.borrow_mut() = Some(logic);
        inputs
    });
    let logic = logic_out.borrow_mut().take().expect("the build ran");
    let built = graph.live_nodes();
    let handles = listen(&mut graph, &logic);
    for k in 1..=20 {
        drive(&mut graph, &inputs, k);
    }
    graph.collect_garbage();
    let listened = graph.live_nodes();
    drop(handles);
    graph.collect_garbage();
    assert_eq!(graph.live_nodes(), inputs.len(), "only the inputs are left");
    assert!(
        stale(&mut graph, &logic),
        "the freed nodes' tokens are stale"
    );
    Counts {
        built,
        listened,
        dropped: graph.live_nodes(),
    }
}

/// A flat graph: a fused chain with a snapshot, a merge, a hold, a lift
/// and a coalescing input. Nine nodes: two inputs, their shares, two
/// holds, the merge and its share, and the lift.
#[test]
fn a_flat_graph_is_collected_once_its_listeners_are_dropped() {
    let (heard, on) = recorder::<u32>();
    let on = Rc::new(RefCell::new(on));
    let counts = build_drive_drop(
        |b| {
            let (a, a_in) = b.input::<u32>();
            let (c, c_in) = b.input_coalescing(|x: u32, y| x + y);
            let a = a.share(b);
            let c = c.share(b);
            let level = c.hold(b, 1u32);
            let scaled = a
                .map(|v| v + 1)
                .filter(|v| v % 2 == 0)
                .snapshot(level, |v, l| v * l)
                .hold(b, 0u32);
            let both = a.merge(b, c, |x, y| x + y).share(b);
            let total = (scaled, level).lift(b, |s, l| s + l);
            (vec![a_in, c_in], (both, total))
        },
        |graph, (both, total)| {
            let (on_both, on_total) = (on.clone(), on.clone());
            vec![
                graph.listen(*both, move |v| (on_both.borrow_mut())(v)),
                graph.listen_cell(*total, move |v| (on_total.borrow_mut())(*v)),
            ]
        },
        |graph, inputs, k| {
            graph.send(inputs[0], k);
            graph.send(inputs[1], k);
        },
        |graph, (_, total)| graph.try_sample(*total).err() == Some(TokenError::Stale),
    );
    assert_eq!(
        counts,
        Counts {
            built: 9,
            listened: 9,
            dropped: 2
        }
    );
    assert!(heard.borrow().len() > 40);
}

/// A cell loop, a stream loop and a state loop: each a cycle through a
/// read of a cell's value, which reach follows and dependency does not.
/// Eight nodes: the input and its share, and two per loop, the forward
/// and its definition.
#[test]
fn loops_are_collected_once_their_listeners_are_dropped() {
    let counts = build_drive_drop(
        |b| {
            let (ticks, ticks_in) = b.input::<u32>();
            let ticks = ticks.share(b);
            let (count, count_loop) = b.cell_loop::<u32>();
            let next = ticks.snapshot(count, |_, n| n + 1).hold(b, 0u32);
            count_loop.close(b, next);
            let (sums, sums_loop) = b.stream_loop::<u32>();
            let running = sums.hold(b, 0u32);
            sums_loop.close(b, ticks.snapshot(running, |t, r| t + r));
            let (seen, seen_loop) = b.state_loop::<Vec<u32>>();
            let appended = ticks
                .snapshot(seen, |t, s| t + s.len() as u32)
                .accumulate_mut(b, Vec::new(), |v, s: &mut Vec<u32>| s.push(v));
            seen_loop.close(b, appended);
            (vec![ticks_in], (count, running, seen))
        },
        |graph, (count, running, seen)| {
            vec![
                graph.listen_cell(*count, |_| ()),
                graph.listen_steps(*running, |_| ()),
                graph.listen_steps(*seen, |_| ()),
            ]
        },
        |graph, inputs, k| graph.send(inputs[0], k),
        |graph, (count, running, seen)| {
            graph.try_sample(*count).err() == Some(TokenError::Stale)
                && graph.try_sample(*running).err() == Some(TokenError::Stale)
                && graph.try_sample(*seen).err() == Some(TokenError::Stale)
        },
    );
    assert_eq!(
        counts,
        Counts {
            built: 8,
            listened: 8,
            dropped: 1
        }
    );
}

/// A split and a defer, each two nodes, and a countdown loop through a
/// defer: the output keeps its capture in reach, so both go together.
/// Fourteen nodes: the input and its share, three captures and their
/// outputs, the merge and its share, the loop, the or_else and its share,
/// and the accumulator.
#[test]
fn splits_and_defers_are_collected_once_their_listeners_are_dropped() {
    let counts = build_drive_drop(
        |b| {
            let (n, n_in) = b.input::<u32>();
            let n = n.share(b);
            let items = n.map(|v| [v, v + 1, v + 2]).split(b);
            let later = n.defer(b);
            let children = items.merge(b, later, |i, l| i * 100 + l).share(b);
            let (down, down_loop) = b.stream_loop::<u32>();
            let again = down.filter(|v| *v > 1).map(|v| v - 1).defer(b);
            let countdown = n.map(|v| v % 4).or_else(b, again).share(b);
            down_loop.close(b, countdown);
            let total = countdown.accumulate(b, 0u32, |v, t| t + v);
            (vec![n_in], (children, total))
        },
        |graph, (children, total)| {
            vec![
                graph.listen(*children, |_| ()),
                graph.listen_cell(*total, |_| ()),
            ]
        },
        |graph, inputs, k| graph.send(inputs[0], k),
        |graph, (_, total)| graph.try_sample(*total).err() == Some(TokenError::Stale),
    );
    assert_eq!(
        counts,
        Counts {
            built: 14,
            listened: 14,
            dropped: 1
        }
    );
}

/// A switch_cell, a switch_stream between shared streams, and a
/// switch_stream between linear streams in constant cells, selected
/// through a switch_cell, each selecting among inners its selector's
/// closure captured and declared. Twenty nodes, two of them inputs.
#[test]
fn switches_are_collected_once_their_listeners_are_dropped() {
    let counts = build_drive_drop(
        |b| {
            let (n, n_in) = b.input::<u32>();
            let (pick, pick_in) = b.input::<u32>();
            let n = n.share(b);
            let pick = pick.share(b);
            let latest = n.hold(b, 0u32);
            let doubled = latest.map_cell(b, |v| v * 2);
            let chosen = pick
                .map(move |p| if p == 1 { doubled } else { latest })
                .hold(b, latest);
            b.depends(&chosen, &[&doubled, &latest]);
            let shown = chosen.switch_cell(b);
            let evens = n.filter(|v| v % 2 == 0).share(b);
            let followed = pick.map(move |p| if p == 1 { evens } else { n }).hold(b, n);
            b.depends(&followed, &[&evens, &n]);
            let followed = followed.switch_stream(b).share(b);
            let tens = n.map(|v| v * 10).node(b);
            let tens = b.constant(tens);
            let hundreds = n.map(|v| v * 100).node(b);
            let hundreds = b.constant(hundreds);
            let lines = pick
                .map(move |p| if p == 1 { hundreds } else { tens })
                .hold(b, tens);
            b.depends(&lines, &[&hundreds, &tens]);
            let taken = lines.switch_cell(b).switch_stream(b).share(b);
            (vec![n_in, pick_in], (shown, followed, taken))
        },
        |graph, (shown, followed, taken)| {
            vec![
                graph.listen_cell(*shown, |_| ()),
                graph.listen(*followed, |_| ()),
                graph.listen(*taken, |_| ()),
            ]
        },
        |graph, inputs, k| {
            graph.send(inputs[0], k);
            graph.send(inputs[1], u32::from(k % 3 == 0));
        },
        |graph, (shown, _, _)| graph.try_sample(*shown).err() == Some(TokenError::Stale),
    );
    assert_eq!(
        counts,
        Counts {
            built: 20,
            listened: 20,
            dropped: 2
        }
    );
}

/// What a screen shows at a click: the screen, the click, and how many
/// clicks the screen has counted.
type Shown = (u32, u32, u32);

/// A screen of stage 6's navigation: a counter over the clicks, built with
/// it, and a linear stream of what the screen shows, two nodes.
fn screen(b: &mut Build, clicks: Shared<u32>, n: u32) -> Stream<Shown> {
    let count = clicks.accumulate(b, 0u32, |_, c| c + 1);
    clicks
        .snapshot(count, move |click, k| (n, click, k + 1))
        .node(b)
}

/// Stage 6's navigation loop, RFD 4's dynamic pattern: every navigation
/// builds a screen, a hold keeps the current one, and one switch_stream
/// takes its events; a click of 0 navigates. The build returns the clicks'
/// input and the log, and the rest is reached from the log.
fn navigation() -> (Graph, Input<u32>, Cell<Vec<Shown>>) {
    let (graph, (clicks_in, log)) = Graph::build(|b| {
        let (clicks, clicks_in) = b.input::<u32>();
        let clicks = clicks.share(b);
        let (navigate, navigate_loop) = b.stream_loop::<u32>();
        let first = screen(b, clicks, 0);
        let screens = navigate.construct(b, move |b, n| screen(b, clicks, n));
        let current = screens.hold(b, first);
        let events = current.switch_stream(b).share(b);
        navigate_loop.close(
            b,
            events.filter_map(|(n, click, _)| (click == 0).then_some(n + 1)),
        );
        (clicks_in, log_events(b, events))
    });
    (graph, clicks_in, log)
}

/// Build, drive, drop, collect, compare for the navigation loop, rooted by
/// a listener on its events rather than by the build's return value:
/// once the listener is dropped, the loop, the construct, the switch and
/// the current screen go, and only the clicks' input is left.
#[test]
fn the_navigation_loop_is_collected_once_its_listener_is_dropped() {
    let (heard, on) = recorder::<Shown>();
    let on = Rc::new(RefCell::new(on));
    let counts = build_drive_drop(
        |b| {
            let (clicks, clicks_in) = b.input::<u32>();
            let clicks = clicks.share(b);
            let (navigate, navigate_loop) = b.stream_loop::<u32>();
            let first = screen(b, clicks, 0);
            let screens = navigate.construct(b, move |b, n| screen(b, clicks, n));
            let current = screens.hold(b, first);
            let events = current.switch_stream(b).share(b);
            navigate_loop.close(
                b,
                events.filter_map(|(n, click, _)| (click == 0).then_some(n + 1)),
            );
            (vec![clicks_in], events)
        },
        |graph, events| {
            let on = on.clone();
            vec![graph.listen(*events, move |e| (on.borrow_mut())(e))]
        },
        |graph, inputs, k| graph.send(inputs[0], if k % 3 == 0 { 0 } else { k }),
        |graph, events| graph.try_listen(*events, |_| ()).err() == Some(TokenError::Stale),
    );
    // Built: the input and its share, the loop, the first screen's two
    // nodes, the construct, the hold, the switch and its share. After six
    // navigations and a collection, the current screen replaces the first.
    assert_eq!(
        counts,
        Counts {
            built: 9,
            listened: 9,
            dropped: 1
        }
    );
    assert_eq!(heard.borrow().len(), 20);
}

/// The navigation loop grows by two nodes a screen without collection
/// (stage 6). The screens the switch has left are named by nothing, so the
/// automatic policy collects them: the live nodes stay under twice what
/// the last collection left, plus the screen that triggers the next, and
/// an explicit collection comes back to what the first one left. Every
/// event is the same under both policies.
#[test]
fn the_navigation_loop_stays_bounded_under_the_automatic_policy() {
    let run = |policy: CollectionPolicy| {
        let (mut graph, clicks_in, log) = navigation();
        graph.set_collection_policy(policy);
        graph.collect_garbage();
        let settled = graph.live_nodes();
        let mut peak = settled;
        for k in 0..300u32 {
            graph.send(clicks_in, if k % 3 == 2 { 0 } else { k + 1 });
            peak = peak.max(graph.live_nodes());
        }
        let grown = graph.live_nodes();
        graph.collect_garbage();
        (
            settled,
            peak,
            grown,
            graph.live_nodes(),
            graph.sample(log).clone(),
        )
    };
    let (settled, _, grown, _, manual_log) = run(CollectionPolicy::Manual);
    assert_eq!(grown, settled + 2 * 100, "two nodes a screen, never freed");
    let (settled, peak, _, collected, log) = run(CollectionPolicy::Automatic);
    assert!(peak <= 2 * settled + 2, "peak {peak} from {settled}");
    assert_eq!(collected, settled);
    assert_eq!(log, manual_log);
    assert_eq!(log.len(), 300);
    assert_eq!(
        log[299],
        (99, 0, 4),
        "screen 99 navigated on its fourth click"
    );
}

// ----------------------------------------------------------- cycles and deselected inners

/// A cycle through a value (RFD 3). A constant holds a map_cell computed
/// from a loop the constant defines, so the constant's value names a node
/// that depends on the constant. Counted references would keep the three
/// alive for ever, which is how sodium-rust leaks; tracing from the roots
/// collects them once the anchor that roots them is dropped. The same
/// cycle made at run time goes the same way: each `go` builds a map_cell
/// over a hold, and a loop feeds it into that hold.
#[test]
fn a_cycle_through_a_value_is_collected_once_unrooted() {
    let (out, inp) = side_channel();
    let (mut graph, ()) = Graph::build(move |b| {
        let (forward, forward_loop) = b.cell_loop::<Cell<u32>>();
        let seven = forward.map_cell(b, |_| 7u32);
        let holder = b.constant(seven);
        forward_loop.close(b, holder);
        *inp.borrow_mut() = Some((holder, seven));
    });
    graph.set_collection_policy(CollectionPolicy::Manual);
    let (holder, seven) = out.borrow().expect("the build ran");
    let anchor = graph.anchor(&holder);
    graph.collect_garbage();
    assert_eq!(graph.live_nodes(), 3);
    let named = *graph.sample(holder);
    assert_eq!(
        named, seven,
        "the value names a node downstream of its cell"
    );
    assert_eq!(*graph.sample(named), 7);
    drop(anchor);
    graph.collect_garbage();
    assert_eq!(graph.live_nodes(), 0);
    assert_eq!(graph.try_sample(holder).err(), Some(TokenError::Stale));
    assert_eq!(graph.try_sample(seven).err(), Some(TokenError::Stale));

    let (out, inp) = side_channel();
    let (mut graph, go_in) = Graph::build(move |b| {
        let (go, go_in) = b.input::<u32>();
        let (fed, fed_loop) = b.stream_loop::<Cell<u32>>();
        let zero = b.constant(0u32);
        let holder = fed.hold(b, zero);
        let made = go.construct(b, move |b, k| holder.map_cell(b, move |_| k));
        b.depends(&made, &[&holder]);
        fed_loop.close(b, made);
        *inp.borrow_mut() = Some(holder);
        go_in
    });
    graph.set_collection_policy(CollectionPolicy::Manual);
    let holder = out.borrow().expect("the build ran");
    let anchor = graph.anchor(&holder);
    for k in 1..=3 {
        graph.send(go_in, k);
    }
    graph.collect_garbage();
    let newest = *graph.sample(holder);
    assert_eq!(*graph.sample(newest), 3);
    // The input, the loop, the hold, the construct and the newest map_cell;
    // the older ones and the constant nothing names any more.
    assert_eq!(graph.live_nodes(), 5);
    drop(anchor);
    graph.collect_garbage();
    assert_eq!(graph.live_nodes(), 1, "the input, a permanent root");
    assert_eq!(graph.try_sample(newest).err(), Some(TokenError::Stale));
}

/// A deselected inner that something still names is alive, so it keeps
/// accumulating and is observed again when reselected (RFD 3). Three
/// counters, named by a constant's value that a snapshot selects from, so
/// no closure captures them; every transaction opens with a collection,
/// and each counter counts every click, selected or not.
#[test]
fn a_deselected_inner_that_a_cell_names_keeps_accumulating() {
    let (mut graph, (clicks_in, pick_in, shown)) = Graph::build(|b| {
        let (clicks, clicks_in) = b.input::<()>();
        let clicks = clicks.share(b);
        let counters: Vec<Cell<u32>> = (0..3)
            .map(|k| clicks.accumulate(b, k * 100, |_, n| n + 1))
            .collect();
        let registry = b.constant(counters.clone());
        let (pick, pick_in) = b.input::<usize>();
        let current = pick.snapshot(registry, |i, r| r[i]).hold(b, counters[0]);
        (clicks_in, pick_in, current.switch_cell(b))
    });
    graph.set_collect_after_every_transaction(true);
    graph.send(clicks_in, ());
    graph.send(clicks_in, ());
    assert_eq!(*graph.sample(shown), 2);
    graph.send(pick_in, 1);
    assert_eq!(*graph.sample(shown), 102, "counted while deselected");
    graph.send(clicks_in, ());
    graph.send(pick_in, 0);
    assert_eq!(*graph.sample(shown), 3, "counted while deselected");
    graph.send(pick_in, 2);
    assert_eq!(*graph.sample(shown), 203);
}

/// A deselected inner that nothing names can never be observed again, so
/// it is collected (RFD 3). Each `open` builds a counter, and a hold keeps
/// the current one. I/O code receives each counter's token and does not
/// anchor it, so once the switch has left a counter, the collection that
/// opens the next transaction frees it and the token is stale; the freed
/// slot is reused by the next counter.
#[test]
fn a_deselected_inner_that_nothing_names_is_collected() {
    let (mut graph, (clicks_in, open_in, made, shown)) = Graph::build(|b| {
        let (clicks, clicks_in) = b.input::<()>();
        let clicks = clicks.share(b);
        let (open, open_in) = b.input::<u32>();
        let made = open.construct(b, move |b, start| clicks.accumulate(b, start, |_, n| n + 1));
        b.depends(&made, &[&clicks]);
        let made = made.share(b);
        let zero = b.constant(0u32);
        let shown = made.hold(b, zero).switch_cell(b);
        (clicks_in, open_in, made, shown)
    });
    let (received, on) = recorder::<Cell<u32>>();
    graph.listen(made, on).keep();
    graph.set_collect_after_every_transaction(true);
    graph.send(open_in, 100);
    graph.send(clicks_in, ());
    let first = received.borrow()[0];
    assert_eq!(*graph.sample(first), 101);
    let live = graph.live_nodes();
    graph.send(open_in, 200);
    graph.send(clicks_in, ());
    let second = received.borrow()[1];
    assert_eq!(graph.try_sample(first).err(), Some(TokenError::Stale));
    assert_eq!(*graph.sample(second), 201);
    assert_eq!(*graph.sample(shown), 201);
    assert_eq!(graph.live_nodes(), live, "one counter for another");
}

// ----------------------------------------------------------- what the user declares

/// A closure that captures a token that is not upstream of its own node
/// declares it (RFD 3). A map captures `total`, a hold over a stream loop
/// whose definition is the map's own node, so `total` is downstream of
/// it: a backward capture, which the map emits into a hold I/O code
/// reads. Undeclared, nothing reaches `total` and the loop until the map
/// has emitted it, so under the stress setting the first transaction
/// opens with a collection that frees them, and the token the map emits
/// is stale where I/O code uses it. Declared, the loop runs.
#[test]
fn an_undeclared_backward_capture_is_a_stale_token_on_the_first_transaction() {
    let program = |declare: bool| {
        Graph::build(move |b| {
            let (clicks, clicks_in) = b.input::<u32>();
            let (sums, sums_loop) = b.stream_loop::<u32>();
            let total = sums.hold(b, 0u32);
            let bumps = clicks.map(move |c| (c, total)).share(b);
            if declare {
                b.depends(&bumps, &[&total]);
            }
            sums_loop.close(b, bumps.map(|(c, _)| c).snapshot(total, |c, t| c + t));
            let zero = b.constant(0u32);
            let reported = bumps.map(|(_, t)| t).hold(b, zero);
            (clicks_in, reported)
        })
    };
    let (mut graph, (clicks_in, reported)) = program(false);
    graph.set_collect_after_every_transaction(true);
    graph.send(clicks_in, 5);
    let total = *graph.sample(reported);
    assert_eq!(graph.try_sample(total).err(), Some(TokenError::Stale));

    let (mut graph, (clicks_in, reported)) = program(true);
    graph.set_collect_after_every_transaction(true);
    graph.send(clicks_in, 5);
    graph.send(clicks_in, 6);
    let total = *graph.sample(reported);
    assert_eq!(*graph.sample(total), 11);
}

/// RFD 3's case for a declaration on any closure rather than on
/// `construct` alone: a map that emits a captured token after the node it
/// names has become unreachable otherwise. `back` returns to the first
/// panel, which the navigation hold names until a construct opens
/// another; from then on only the map's closure names it. Undeclared, the
/// next collection frees it, and going back selects a stale token, which
/// the switch's move refuses with a panic that poisons the graph.
/// Declared, the first panel lives and keeps counting while deselected.
#[test]
fn a_map_that_emits_a_token_after_its_node_became_unreachable_needs_a_declaration() {
    for declare in [false, true] {
        let (mut graph, (clicks_in, open_in, back_in, shown)) = Graph::build(move |b| {
            let (clicks, clicks_in) = b.input::<()>();
            let clicks = clicks.share(b);
            let (open, open_in) = b.input::<u32>();
            let (back, back_in) = b.input::<()>();
            let first = clicks.accumulate(b, 0u32, |_, n| n + 1);
            let returns = back.map(move |_| first).node(b);
            if declare {
                b.depends(&returns, &[&first]);
            }
            let opened =
                open.construct(b, move |b, start| clicks.accumulate(b, start, |_, n| n + 1));
            b.depends(&opened, &[&clicks]);
            let shown = opened.or_else(b, returns).hold(b, first).switch_cell(b);
            (clicks_in, open_in, back_in, shown)
        });
        graph.set_collect_after_every_transaction(true);
        graph.send(clicks_in, ());
        assert_eq!(*graph.sample(shown), 1);
        graph.send(open_in, 100);
        graph.send(clicks_in, ());
        assert_eq!(*graph.sample(shown), 101);
        if declare {
            graph.send(back_in, ());
            assert_eq!(*graph.sample(shown), 2, "the first panel kept counting");
        } else {
            let message = panic_message(|| graph.send(back_in, ()));
            assert!(message.contains("a stale token"), "{message}");
            assert_eq!(graph.try_send(clicks_in, ()), Err(SendError::Poisoned));
        }
    }
}

/// RFD 3 says a capture upstream of the closure's own node needs no
/// declaration, since the node reaches it anyway. Through a switch,
/// upstream changes at run time. The navigation construct captures
/// `clicks`, which is upstream of it, through the loop and the switch,
/// while the current page consumes clicks; a quiet page does not, and
/// while one is selected nothing reaches `clicks`' shared node. The next
/// collection frees it, and the construct's next run, which a timer
/// starts, builds from a stale token: a panic that poisons the graph. A
/// declaration keeps it.
#[test]
fn a_capture_upstream_only_through_a_switch_s_selection_needs_a_declaration() {
    for declare in [false, true] {
        let (mut graph, (clicks_in, timer_in, log)) = Graph::build(move |b| {
            let (clicks, clicks_in) = b.input::<u32>();
            let clicks = clicks.share(b);
            let (timer, timer_in) = b.input::<u32>();
            // Odd pages consume the clicks; even pages are quiet.
            let page = move |b: &mut Build, n: u32| -> Shared<(u32, u32)> {
                if n % 2 == 1 {
                    clicks.map(move |c| (n, c)).share(b)
                } else {
                    b.never::<(u32, u32)>().share(b)
                }
            };
            let (navigate, navigate_loop) = b.stream_loop::<u32>();
            let first = page(b, 1);
            let pages = navigate.construct(b, move |b, n| page(b, n));
            if declare {
                b.depends(&pages, &[&clicks]);
            }
            let events = pages.hold(b, first).switch_stream(b).share(b);
            let next = events
                .filter_map(|(n, c)| (c == 0).then_some(n + 1))
                .or_else(b, timer);
            navigate_loop.close(b, next);
            (clicks_in, timer_in, log_events(b, events))
        });
        graph.set_collect_after_every_transaction(true);
        graph.send(clicks_in, 4);
        graph.send(clicks_in, 0); // to page 2, which is quiet
        if declare {
            graph.send(timer_in, 3);
            graph.send(clicks_in, 7);
            assert_eq!(*graph.sample(log), [(1, 4), (1, 0), (3, 7)]);
        } else {
            let message = panic_message(|| graph.send(timer_in, 3));
            assert!(message.contains("a stale token"), "{message}");
            assert_eq!(graph.try_send(clicks_in, 1), Err(SendError::Poisoned));
        }
    }
}

/// RFD 2's receive, then wire, under the stress setting. An input a
/// construct closure built reaches I/O code as data; anchored after the
/// send that delivered it, it survives the collection that opens the
/// next transaction. A collection right after the transaction, as a
/// policy that collected at the end of each transaction would run, frees
/// it before I/O code can anchor it.
#[test]
fn a_token_a_listener_delivers_can_be_anchored_before_the_next_collection() {
    let program = || {
        let (mut graph, (open_in, opened)) = Graph::build(|b| {
            let (open, open_in) = b.input::<u32>();
            let opened = open.construct(b, |b, start| {
                let (bumps, bumps_in) = b.input::<u32>();
                (bumps_in, bumps.accumulate(b, start, |n, c| c + n))
            });
            (open_in, opened)
        });
        graph.set_collect_after_every_transaction(true);
        let (received, on) = recorder::<(Input<u32>, Cell<u32>)>();
        graph.listen(opened, on).keep();
        graph.send(open_in, 10);
        let counter = received.borrow()[0];
        (graph, counter)
    };
    let (mut graph, counter) = program();
    let _anchor = graph.anchor(&counter);
    let (bumps_in, count) = counter;
    graph.send(bumps_in, 5);
    assert_eq!(*graph.sample(count), 15);

    let (mut graph, (bumps_in, count)) = program();
    graph.collect_garbage();
    assert_eq!(graph.try_send(bumps_in, 5), Err(SendError::Stale));
    assert_eq!(graph.try_anchor(&count).err(), Some(TokenError::Stale));
}

/// `Trace` is a safe trait (RFD 3): an implementation that misses a token
/// lets its node be collected early, and the next use of the token is a
/// stale-token error, never a read of another node.
#[test]
fn a_trace_that_misses_a_token_makes_it_stale() {
    #[derive(Clone, Copy)]
    struct Forgetful(Cell<u32>);
    impl Trace for Forgetful {
        fn trace(&self, _tracer: &mut Tracer) {}
    }
    let (mut graph, (forgetful, honest)) = Graph::build(|b| {
        let one = b.constant(1u32);
        let two = b.constant(2u32);
        (b.constant(Forgetful(one)), b.constant((two,)))
    });
    graph.collect_garbage();
    let missed = graph.sample(forgetful).0;
    let (traced,) = *graph.sample(honest);
    assert_eq!(graph.try_sample(missed).err(), Some(TokenError::Stale));
    assert_eq!(*graph.sample(traced), 2);
}

// ----------------------------------------------------------- generations

/// A freed slot is reused under a new generation: the old token names the
/// same slot, as its index shows, and fails the generation check, while
/// the token of the node that took the slot works.
#[test]
fn a_slot_reused_after_collection_rejects_the_old_token() {
    let (out, inp) = side_channel::<Cell<u32>>();
    let (mut graph, (go_in, made)) = Graph::build(move |b| {
        let (go, go_in) = b.input::<u32>();
        *inp.borrow_mut() = Some(b.constant(1u32));
        let made = go.construct(b, |b, n| b.constant(n)).share(b);
        (go_in, made)
    });
    graph.set_collection_policy(CollectionPolicy::Manual);
    let lost = out.borrow().expect("the build ran");
    let (received, on) = recorder::<Cell<u32>>();
    graph.listen(made, on).keep();
    graph.collect_garbage();
    graph.send(go_in, 7);
    let reused = received.borrow()[0];
    assert_eq!(format!("{reused:?}"), format!("{lost:?}"), "the same slot");
    assert_ne!(reused, lost, "a new generation");
    assert_eq!(graph.try_sample(lost).err(), Some(TokenError::Stale));
    assert_eq!(*graph.sample(reused), 7);
}

// ----------------------------------------------------------- handles

/// A listener dropped inside another listener's call stops at once (RFD
/// 3: the drop only clears a flag), and the node only it rooted is
/// collected at the next collection, while the other listener's stays.
#[test]
fn a_listener_dropped_inside_a_listener_lets_its_node_be_collected() {
    let (out, inp) = side_channel();
    let (mut graph, n_in) = Graph::build(move |b| {
        let (n, n_in) = b.input::<u32>();
        let n = n.share(b);
        let kept = n.map(|v| v + 1).share(b);
        let victim = n.map(|v| v * 10).share(b);
        *inp.borrow_mut() = Some((kept, victim));
        n_in
    });
    graph.set_collection_policy(CollectionPolicy::Manual);
    let (kept, victim) = out.borrow().expect("the build ran");
    let handle: Rc<RefCell<Option<Listener>>> = Rc::new(RefCell::new(None));
    let taken = handle.clone();
    graph
        .listen(kept, move |v| {
            if v == 3 {
                taken.borrow_mut().take();
            }
        })
        .keep();
    let (seen, on) = recorder();
    *handle.borrow_mut() = Some(graph.listen(victim, on));
    graph.collect_garbage();
    assert_eq!(graph.live_nodes(), 4);
    graph.send(n_in, 1);
    graph.send(n_in, 2); // the kept listener drops the victim's handle
    let heard = seen.borrow().len();
    graph.send(n_in, 3);
    assert_eq!(seen.borrow().len(), heard, "stopped at once");
    assert!(seen.borrow().starts_with(&[10]));
    graph.collect_garbage();
    assert_eq!(graph.live_nodes(), 3, "the victim's node alone is freed");
    assert_eq!(
        graph.try_listen(victim, |_| ()).err(),
        Some(TokenError::Stale)
    );
    assert!(graph.try_listen(kept, |_| ()).is_ok());
}

/// `keep` leaves a listener or an anchor live for the graph's life with no
/// handle to hold, so their nodes survive every collection; a dropped or
/// unanchored anchor roots nothing from the next collection on.
#[test]
fn kept_handles_are_roots_for_the_graph_s_life() {
    let (out, inp) = side_channel();
    let (mut graph, ()) = Graph::build(move |b| {
        let cells = [1u32, 2, 3, 4].map(|v| b.constant(v));
        *inp.borrow_mut() = Some(cells);
    });
    let [listened, anchored, dropped, unanchored] = out.borrow().expect("the build ran");
    graph.listen_cell(listened, |_| ()).keep();
    graph.anchor(&anchored).keep();
    drop(graph.anchor(&dropped));
    graph.anchor(&unanchored).unanchor();
    for _ in 0..3 {
        graph.collect_garbage();
    }
    assert_eq!(graph.live_nodes(), 2);
    assert_eq!(*graph.sample(listened), 1);
    assert_eq!(*graph.sample(anchored), 2);
    assert_eq!(graph.try_sample(dropped).err(), Some(TokenError::Stale));
    assert_eq!(graph.try_sample(unanchored).err(), Some(TokenError::Stale));
}

/// Garbage is made by unrooting as much as by allocating (RFD 3). A graph
/// built once that then only drops listeners allocates no node, and the
/// automatic policy still collects it: a dropped handle counts as a
/// released root, through its flag, with no graph access, and when the
/// handles released since the last collection exceed the live count it
/// left, the next transaction opens with a collection.
#[test]
fn the_automatic_policy_collects_a_graph_that_only_drops_listeners() {
    let (out, inp) = side_channel::<Vec<Cell<u32>>>();
    let (mut graph, (n_in, n)) = Graph::build(move |b| {
        let (n, n_in) = b.input::<u32>();
        let n = n.share(b);
        let cells = (0..4)
            .map(|k| n.map(move |v| v + k).hold(b, 0u32))
            .collect();
        *inp.borrow_mut() = Some(cells);
        (n_in, n)
    });
    let cells = out.borrow_mut().take().expect("the build ran");
    let handles: Vec<Listener> = cells
        .iter()
        .map(|c| graph.listen_cell(*c, |_| ()))
        .collect();
    graph.send(n_in, 1); // the first transaction collects what the build did not root
    assert_eq!(graph.live_nodes(), 6);
    drop(handles);
    for released in 5..=7 {
        graph.send(n_in, released);
        assert_eq!(graph.live_nodes(), 6, "{} released, 6 live", released - 1);
        graph.listen(n, |_| ()).unlisten();
    }
    graph.send(n_in, 8); // 7 released: the transaction opens with a collection
    assert_eq!(graph.live_nodes(), 2);
    for c in cells {
        assert_eq!(graph.try_sample(c).err(), Some(TokenError::Stale));
    }
}

// ----------------------------------------------------------- operations on collected nodes

/// Operations on a collected node (RFD 3, RFD 5). The `try_` forms return
/// `Stale` in every build. Sending to a collected input, listening to a
/// collected node and anchoring one have no effect the semantics can
/// observe, so the panicking forms panic in a debug build, before any
/// transaction opens, and in a release build do nothing and count it;
/// `Transaction::send` panics inside the transaction, which poisons the
/// graph. Sampling must return a value, so it panics in both builds.
#[test]
fn operations_on_collected_nodes_follow_the_debug_release_rule() {
    let (out, inp) = side_channel();
    let (mut graph, other_in) = Graph::build(move |b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let numbers = numbers.share(b);
        let held = numbers.hold(b, 0u32);
        *inp.borrow_mut() = Some((numbers_in, numbers, held));
        b.input::<u32>().1
    });
    graph.set_collection_policy(CollectionPolicy::Manual);
    let (numbers_in, numbers, held) = out.borrow().expect("the build ran");
    graph.collect_garbage();
    assert_eq!(graph.live_nodes(), 1);

    assert_eq!(graph.try_send(numbers_in, 1), Err(SendError::Stale));
    assert_eq!(
        graph.try_listen(numbers, |_| ()).err(),
        Some(TokenError::Stale)
    );
    assert_eq!(
        graph.try_listen_cell(held, |_| ()).err(),
        Some(TokenError::Stale)
    );
    assert_eq!(
        graph.try_listen_steps(held, |_| ()).err(),
        Some(TokenError::Stale)
    );
    assert_eq!(graph.try_anchor(&held).err(), Some(TokenError::Stale));
    assert_eq!(graph.try_sample(held).err(), Some(TokenError::Stale));
    let sent = graph.transaction(|tx| tx.try_send(numbers_in, 1));
    assert_eq!(sent, Err(TransactionSendError::Stale));
    assert!(panic_message(|| *graph.sample(held)).contains("a stale token"));
    assert_eq!(
        graph.stale_operations(),
        0,
        "an error or a panic is not a no-op"
    );

    type Operation = Box<dyn Fn(&mut Graph)>;
    let unobservable: [(&str, Operation); 5] = [
        (
            "a send to a collected input",
            Box::new(move |g| g.send(numbers_in, 1)),
        ),
        (
            "a listener on a collected node",
            Box::new(move |g| drop(g.listen(numbers, |_| ()))),
        ),
        (
            "a listener on a collected node",
            Box::new(move |g| drop(g.listen_cell(held, |_| ()))),
        ),
        (
            "a listener on a collected node",
            Box::new(move |g| drop(g.listen_steps(held, |_| ()))),
        ),
        (
            "an anchor on a collected node",
            Box::new(move |g| drop(g.anchor(&held))),
        ),
    ];
    for (k, (what, operation)) in unobservable.iter().enumerate() {
        if cfg!(debug_assertions) {
            let message = panic_message(|| operation(&mut graph));
            assert!(message.contains(what), "{message}");
            assert_eq!(graph.try_send(other_in, 1), Ok(()), "no transaction opened");
            assert_eq!(graph.stale_operations(), 0);
        } else {
            operation(&mut graph);
            assert_eq!(graph.stale_operations(), k as u64 + 1);
        }
    }
    if cfg!(debug_assertions) {
        let message = panic_message(|| graph.transaction(|tx| tx.send(numbers_in, 1)));
        assert!(message.contains("a send to a collected input"), "{message}");
        assert_eq!(graph.try_send(other_in, 1), Err(SendError::Poisoned));
    } else {
        graph.transaction(|tx| {
            tx.send(numbers_in, 1);
            tx.send(other_in, 2);
        });
        assert_eq!(graph.stale_operations(), 6);
        assert_eq!(graph.try_send(other_in, 3), Ok(()));
    }
}

/// A collection runs the `Drop` of what it frees, which is user code, and
/// holds the transaction-in-progress flag while it runs: a panic there
/// leaves the arena half swept, so it poisons the graph as a panic in a
/// transaction does.
#[test]
fn a_panic_in_a_drop_during_collection_poisons_the_graph() {
    struct Fragile;
    impl Drop for Fragile {
        fn drop(&mut self) {
            panic!("a value's drop panicked");
        }
    }
    impl Trace for Fragile {
        fn trace(&self, _tracer: &mut Tracer) {}
    }
    let (mut graph, n_in) = Graph::build(|b| {
        let (_, n_in) = b.input::<u32>();
        let _unrooted = b.constant(Fragile);
        n_in
    });
    let message = panic_message(|| graph.collect_garbage());
    assert_eq!(message, "a value's drop panicked");
    assert_eq!(graph.try_send(n_in, 1), Err(SendError::Poisoned));
    assert_eq!(graph.try_collect_garbage(), Err(PoisonedError));
    assert!(panic_message(|| graph.collect_garbage()).contains("poisoned"));
}

/// A collection empties every stream's slot first (RFD 3), so an event
/// that holds a token roots nothing, and an event type needs no `Trace`.
/// A shared stream keeps its last event for any consumer until the next,
/// here a parcel holding a token of a node nothing else names: the
/// collection drops the parcel and frees the node.
#[test]
fn an_event_holding_a_token_roots_nothing() {
    #[derive(Clone)]
    struct Parcel {
        _cell: Cell<u32>,
        drops: Rc<StdCell<u32>>,
    }
    impl Drop for Parcel {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }
    let (out, inp) = side_channel();
    let (mut graph, (parcels_in, _parcels)) = Graph::build(move |b| {
        let (parcels, parcels_in) = b.input::<Parcel>();
        let parcels = parcels.share(b);
        *inp.borrow_mut() = Some(b.constant(5u32));
        (parcels_in, parcels)
    });
    graph.set_collection_policy(CollectionPolicy::Manual);
    let lonely = out.borrow().expect("the build ran");
    let drops = Rc::new(StdCell::new(0));
    let parcel = Parcel {
        _cell: lonely,
        drops: drops.clone(),
    };
    graph.send(parcels_in, parcel);
    assert_eq!(drops.get(), 0, "the shared slot keeps the event");
    graph.collect_garbage();
    assert_eq!(drops.get(), 1, "the collection emptied the slot");
    assert_eq!(graph.try_sample(lonely).err(), Some(TokenError::Stale));
}

/// A `State`, a switch over states and `depends` on a state: the
/// operations that take any token take a `State` too.
#[test]
fn states_are_traced_declared_and_anchored_like_cells() {
    let (out, inp) = side_channel();
    let (mut graph, names_in) = Graph::build(move |b| {
        let (names, names_in) = b.input::<String>();
        let names = names.share(b);
        let all = names.accumulate_mut(b, Vec::new(), |n, v: &mut Vec<String>| v.push(n));
        let short = names.filter(|n| n.len() < 4).accumulate_mut(
            b,
            Vec::new(),
            |n, v: &mut Vec<String>| v.push(n),
        );
        let (pick, _pick_in) = b.input::<bool>();
        let chosen = pick.map(move |p| if p { short } else { all }).hold(b, all);
        b.depends(&chosen, &[&short, &all]);
        let current: State<Vec<String>> = chosen.switch_cell(b);
        *inp.borrow_mut() = Some((current, short));
        names_in
    });
    let (current, short) = out.borrow().expect("the build ran");
    let _anchor = graph.anchor(&current);
    graph.set_collect_after_every_transaction(true);
    graph.send(names_in, "ada".to_string());
    graph.send(names_in, "grace".to_string());
    assert_eq!(*graph.sample(current), ["ada", "grace"]);
    assert_eq!(*graph.sample(short), ["ada"], "declared, so alive");
}

/// What a token names is one of the five token types, and `TokenRef`
/// covers them all: a heterogeneous declaration takes a stream, a shared
/// stream, a cell, a state and an input in one slice without consuming
/// the linear stream.
#[test]
fn a_declaration_takes_every_kind_of_token_without_consuming_a_stream() {
    let (mut graph, (n_in, held)) = Graph::build(|b| {
        let (n, n_in) = b.input::<u32>();
        let n = n.share(b);
        let linear: Stream<u32> = n.map(|v| v + 1).node(b);
        let cell = b.constant(1u32);
        let state = n.accumulate_mut(b, 0u32, |v, s: &mut u32| *s += v);
        let (_, input) = b.input::<u32>();
        let held = n.hold(b, 0u32);
        let on: [&dyn TokenRef; 5] = [&linear, &n, &cell, &state, &input];
        b.depends(&held, &on);
        let _still_mine = linear.hold(b, 0u32);
        (n_in, held)
    });
    graph.collect_garbage();
    // The input and its share, the linear stream's node, the constant, the
    // state, the declared input and the hold. The hold over the linear
    // stream, which nothing names, is collected.
    assert_eq!(graph.live_nodes(), 7);
    graph.send(n_in, 2);
    assert_eq!(*graph.sample(held), 2);
}

/// RFD 3 asks a declaration of a closure that captures a token. `map_to`
/// takes no closure but keeps its value in the chain, where the collector
/// does not look either: a token given to `map_to` needs a declaration
/// too, or the node it names is collected while only the chain names it.
#[test]
fn a_token_given_to_map_to_needs_a_declaration_too() {
    for declare in [false, true] {
        let (mut graph, (go_in, shown)) = Graph::build(move |b| {
            let (go, go_in) = b.input::<()>();
            let home = b.constant("home");
            let away = b.constant("away");
            let chosen = go.map_to(away).hold(b, home);
            if declare {
                b.depends(&chosen, &[&away]);
            }
            (go_in, chosen.switch_cell(b))
        });
        graph.set_collect_after_every_transaction(true);
        if declare {
            graph.send(go_in, ());
            assert_eq!(*graph.sample(shown), "away");
        } else {
            let message = panic_message(|| graph.send(go_in, ()));
            assert!(message.contains("a stale token"), "{message}");
        }
    }
}

/// A leak the model allows: a declaration has no inverse. A construct
/// closure that declares, on a node that lives as long as the graph, that
/// it keeps the screen the closure built, keeps every screen ever built:
/// the node's reach grows by one entry a run, and no collection frees a
/// screen. Declared on the closure's own node, as RFD 3 asks, the old
/// screens go.
#[test]
fn a_declaration_on_a_long_lived_node_keeps_every_node_it_names() {
    for on_long_lived in [true, false] {
        let (mut graph, (open_in, _cells)) = Graph::build(move |b| {
            let (open, open_in) = b.input::<u32>();
            let open = open.share(b);
            let registry = open.hold(b, 0u32);
            let screens = open.construct(b, move |b, n| {
                let screen = b.constant(n);
                if on_long_lived {
                    b.depends(&registry, &[&screen]);
                }
                screen
            });
            let current = screens.hold(b, registry).switch_cell(b);
            (open_in, (registry, current))
        });
        graph.set_collection_policy(CollectionPolicy::Manual);
        graph.collect_garbage();
        let before = graph.live_nodes();
        for n in 1..=50 {
            graph.send(open_in, n);
        }
        graph.collect_garbage();
        let grown = graph.live_nodes() - before;
        assert_eq!(grown, if on_long_lived { 50 } else { 1 });
    }
}
