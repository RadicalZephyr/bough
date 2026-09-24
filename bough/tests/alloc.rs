//! RFD 3's rule, a claim under test from stage 1: a graph that is not
//! growing never allocates per transaction. A counting global allocator,
//! alone in its test binary so no other test allocates concurrently. It
//! counts the allocations of the thread that drives the graph, where every
//! transaction runs: the test harness's own thread allocates now and then
//! while a test runs (four blocks of 96 bytes in about one release run in
//! 150 to 400, before stage 3 as after), which a process-wide count would
//! blame on the engine.
//!
//! The graph covers a share, a fused chain, a coalescing input, a merge,
//! an or_else, snapshots, a gate, a hold, a stream listener and cell
//! listeners; and from stage 2 a map_cell, lifts, a steps view of a lift
//! over the map_cell, a steps_with_current, an accumulator, an in-place
//! accumulator with a fixed-size state read by a gate, a snapshot and a
//! lift, and a scan; and from stage 3 loops: a counter through a snapshot
//! of its forward with a steps view and a cell listener on the forward,
//! an accumulator reading itself through a read-through cell over its
//! forward, two loops reading each other lifted over their forwards with a
//! steps view, a stream loop through a hold, and a state loop with a
//! fixed-size state; and from stage 4 child transactions: a split of a
//! fixed-size array, whose iterator allocates nothing, and a defer, merged
//! at child index 0, with a hold and listeners in the children, and a
//! countdown loop through a defer that goes up to three child levels deep;
//! and from stage 5 switches that move at every transaction between inners
//! seen before: a switch_cell with a steps view and a cell listener, a
//! switch_stream between shared streams, and a switch_stream between
//! linear streams in constant cells, selected through a switch_cell, whose
//! one-consumer claim moves with it; and from stage 6 constructs that do
//! not fire in the steady state, one whose chain rejects every event and
//! one over an input the drives do not send to. A construct allocates when
//! it fires, since its closure builds nodes, by design; once it has, the
//! grown graph is steady again. Later stages widen it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell as StdCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bough::{Graph, Lift, Source};

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// Set on the thread that drives the graph. A const-initialized flag
    /// with no destructor: reading it allocates nothing.
    static DRIVER: StdCell<bool> = const { StdCell::new(false) };
}

// Test scaffolding: `GlobalAlloc` is an unsafe trait. The crate under test
// forbids `unsafe`. The default `realloc` allocates through `alloc`, so a
// buffer that grows is counted.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if DRIVER.try_with(StdCell::get).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// A listener's sink that allocates nothing: a shared counter.
fn tally() -> (Rc<StdCell<u64>>, Rc<StdCell<u64>>) {
    let c = Rc::new(StdCell::new(0));
    (c.clone(), c)
}

/// Switches moved since the graph was built, with the `statistics`
/// feature.
#[cfg(feature = "statistics")]
fn relinks(graph: &Graph) -> Option<u64> {
    Some(graph.statistics().relinks)
}

#[cfg(not(feature = "statistics"))]
fn relinks(_: &Graph) -> Option<u64> {
    None
}

#[test]
fn steady_state_transactions_do_not_allocate() {
    DRIVER.with(|driver| driver.set(true));
    let (mut graph, ((numbers_in, bumps_in, open_in), (total, both, merged), later)) =
        Graph::build(|b| {
            let (numbers, numbers_in) = b.input::<u64>();
            let numbers = numbers.share(b);
            let total = numbers
                .map(|x| x * 2)
                .filter(|x| x % 3 != 0)
                .map(|x| x + 1)
                .hold(b, 0u64);
            let (bumps, bumps_in) = b.input_coalescing(|a: u64, b| a + b);
            let merged = numbers
                .map(|x| x + 1)
                .merge(b, bumps, |l, r| l + r)
                .share(b);
            let (open, open_in) = b.input_cell(true);
            let nothing = b.never::<u64>();
            let both = merged
                .snapshot(total, |m, t| m + t)
                .gate(open)
                .or_else(b, nothing)
                .hold(b, 0u64);

            // Stage 2. A fixed-size state: a growing Vec would be the user's
            // own allocation.
            let tripled = total.map_cell(b, |t| t * 3);
            let product = (tripled, both).lift(b, |t, b| t.wrapping_mul(*b));
            let products = product.steps(b);
            let current = tripled.steps_with_current(b);
            let count = numbers.accumulate(b, 0u64, |_, n| n + 1);
            let recent = numbers.accumulate_mut(b, [0u64; 4], |x, r: &mut [u64; 4]| {
                r.rotate_left(1);
                r[3] = x;
            });
            let odd = numbers.accumulate_mut(b, false, |x, odd: &mut bool| *odd = x % 2 == 1);
            let recent_sum = (recent.map_cell(b, |r| r.iter().sum::<u64>()), count)
                .lift(b, |s, c| s.wrapping_add(*c));
            let seen = numbers
                .gate(odd)
                .snapshot(recent, |x, r| x + r[0])
                .hold(b, 0u64);
            let running = numbers
                .scan(b, 0u64, |x, s| (x.wrapping_add(*s), s.wrapping_add(x)))
                .hold(b, 0u64);
            let stage2 = (
                (products, current),
                (recent, recent_sum),
                (seen, running, product),
            );

            // Stage 3: loops.
            let (counted, counted_loop) = b.cell_loop::<u64>();
            let counted_view = counted.steps(b);
            let next = numbers.snapshot(counted, |_, n| n + 1).hold(b, 0u64);
            counted_loop.close(b, next);
            let (acc_fwd, acc_loop) = b.cell_loop::<u64>();
            let halved = acc_fwd.map_cell(b, |s| s / 2);
            let acc = numbers
                .snapshot(halved, |x, h| x + h)
                .accumulate(b, 1u64, |x, s| (s + x) % 1_000_003);
            acc_loop.close(b, acc);
            let (x_fwd, x_loop) = b.cell_loop::<u64>();
            let (y_fwd, y_loop) = b.cell_loop::<u64>();
            let x = numbers.snapshot(y_fwd, |t, y| (t + y) % 1000).hold(b, 1u64);
            let y = merged
                .snapshot(x_fwd, |m, x| (m + x * 2) % 1000)
                .hold(b, 2u64);
            x_loop.close(b, x);
            y_loop.close(b, y);
            let joined = (x_fwd, y_fwd, total).lift(b, |x, y, t| x * 1000 + y + t);
            let joined_view = joined.steps(b);
            let (sums, sums_loop) = b.stream_loop::<u64>();
            let last = sums.hold(b, 0u64);
            sums_loop.close(b, numbers.snapshot(last, |x, l| x.wrapping_add(*l)));
            let (window, window_loop) = b.state_loop::<[u64; 4]>();
            let windowed = numbers.snapshot(window, |x, w| x + w[3]).accumulate_mut(
                b,
                [0u64; 4],
                |x, w: &mut [u64; 4]| {
                    w.rotate_left(1);
                    w[3] = x % 1000;
                },
            );
            window_loop.close(b, windowed);
            let stage3 = (
                (counted, counted_view, acc_fwd),
                (joined, joined_view),
                (last, window),
            );

            // Stage 4: an array's iterator allocates nothing, so what the
            // split keeps between children is the capture's own stack.
            let items = numbers.map(|x| [x, x + 1, x + 2]).split(b);
            let later = numbers.defer(b);
            let children = items
                .merge(b, later, |i, l| i.wrapping_mul(31).wrapping_add(l))
                .share(b);
            let last_child = children.hold(b, 0u64);
            let (down, down_loop) = b.stream_loop::<u64>();
            let again = down.filter(|n| *n > 1).map(|n| n - 1).defer(b);
            let countdown = numbers.map(|x| x % 4).or_else(b, again).share(b);
            down_loop.close(b, countdown);
            let counted_down = countdown.accumulate(b, 0u64, |n, t| t.wrapping_add(n));
            let stage4 = (children, last_child, countdown, counted_down);

            // Stage 5: each drive below sends i and then i + 1, so every
            // switch's outer steps twice per drive and moves once, between
            // two inners it has followed before.
            let picked = numbers
                .map(move |x| if x % 2 == 0 { total } else { tripled })
                .hold(b, total);
            let switched = picked.switch_cell(b);
            let switched_view = switched.steps(b);
            let evens = numbers.filter(|x| x % 2 == 0).share(b);
            let odds = numbers.filter(|x| x % 2 == 1).share(b);
            let followed = numbers
                .map(move |x| if x % 2 == 0 { odds } else { evens })
                .hold(b, evens);
            let followed = followed.switch_stream(b).share(b);
            let plus = numbers.map(|x| x + 1).node(b);
            let plus = b.constant(plus);
            let times = numbers.map(|x| x.wrapping_mul(3)).node(b);
            let times = b.constant(times);
            let lines = numbers
                .map(move |x| if x % 2 == 0 { plus } else { times })
                .hold(b, plus)
                .switch_cell(b);
            let taken = lines.switch_stream(b).share(b);
            let stage5 = (switched, switched_view, followed, taken);

            // Stage 6: a construct marked at every transaction whose chain
            // passes nothing, and one over an input only the negative
            // control sends to, whose closure builds a hold and a map_cell
            // that a switch_cell follows.
            let _rejected = numbers
                .filter(|x| *x == u64::MAX)
                .construct(b, |b, x| b.constant(x));
            let (opens, opens_in) = b.input::<u64>();
            let zero = b.constant(0u64);
            let opened = opens
                .construct(b, move |b, k| {
                    let latest = numbers.map(move |x| x + k).hold(b, k);
                    latest.map_cell(b, |l| l * 2)
                })
                .hold(b, zero)
                .switch_cell(b);
            let stage6 = (opens_in, opened);
            (
                (numbers_in, bumps_in, open_in),
                (total, both, merged),
                (stage2, stage3, stage4, stage5, stage6),
            )
        });
    let (stage2, stage3, stage4, stage5, stage6) = later;
    let ((products, current), (recent, recent_sum), (seen, running, product)) = stage2;
    let ((counted, counted_view, acc_fwd), (joined, joined_view), (last, window)) = stage3;
    let (heard, recorder) = tally();
    graph.listen_cell(both, move |v| recorder.set(*v)).keep();
    let (steps, count) = tally();
    graph
        .listen_steps(total, move |_| count.set(count.get() + 1))
        .keep();
    let (events, sum) = tally();
    graph
        .listen(merged, move |m| sum.set(sum.get().wrapping_add(m)))
        .keep();
    let (last_product, on_product) = tally();
    graph.listen(products, move |p| on_product.set(p)).keep();
    let (currents, on_current) = tally();
    graph
        .listen(current, move |_| on_current.set(on_current.get() + 1))
        .keep();
    let (recents, on_recent) = tally();
    graph
        .listen_steps(recent, move |r| on_recent.set(r[3]))
        .keep();
    let (sums, on_sum) = tally();
    graph
        .listen_cell(recent_sum, move |s| on_sum.set(*s))
        .keep();

    let (counts, on_count) = tally();
    graph.listen(counted_view, move |n| on_count.set(n)).keep();
    let (counted_cell, on_counted) = tally();
    graph
        .listen_cell(counted, move |n| on_counted.set(*n))
        .keep();
    let (joins, on_join) = tally();
    graph.listen(joined_view, move |j| on_join.set(j)).keep();
    let (windows, on_window) = tally();
    graph
        .listen_steps(window, move |w| on_window.set(w[3]))
        .keep();

    let (children, last_child, countdown, counted_down) = stage4;
    let (child_events, on_child) = tally();
    graph
        .listen(children, move |c| {
            on_child.set(on_child.get().wrapping_add(c))
        })
        .keep();
    let (last_children, on_last_child) = tally();
    graph
        .listen_steps(last_child, move |c| on_last_child.set(*c))
        .keep();
    let (countdowns, on_countdown) = tally();
    graph
        .listen(countdown, move |_| on_countdown.set(on_countdown.get() + 1))
        .keep();

    let (switched, switched_view, followed, taken) = stage5;
    let (switch_steps, on_switch_step) = tally();
    graph
        .listen_cell(switched, move |v| on_switch_step.set(*v))
        .keep();
    let (switch_views, on_switch_view) = tally();
    graph
        .listen(switched_view, move |_| {
            on_switch_view.set(on_switch_view.get() + 1)
        })
        .keep();
    let (follows, on_follow) = tally();
    graph
        .listen(followed, move |_| on_follow.set(on_follow.get() + 1))
        .keep();
    let (takes, on_take) = tally();
    graph
        .listen(taken, move |_| on_take.set(on_take.get() + 1))
        .keep();

    let (opens_in, opened) = stage6;
    let (openings, on_opened) = tally();
    graph.listen_cell(opened, move |v| on_opened.set(*v)).keep();

    let drive = |graph: &mut Graph, i: u64| {
        graph.send(numbers_in, i);
        graph.transaction(|tx| {
            tx.send(bumps_in, i);
            tx.send(bumps_in, 1);
            tx.send(numbers_in, i + 1);
        });
        graph.send(open_in, i % 4 != 0);
    };
    for i in 0..100 {
        drive(&mut graph, i); // warm up: reused buffers reach their size
    }
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let views_before = switch_views.get();
    let relinks_before = relinks(&graph);
    for i in 0..10_000 {
        drive(&mut graph, i);
    }
    let plain = ALLOCATIONS.load(Ordering::Relaxed) - before;
    // The switch_cell steps at every transaction whose outer steps.
    assert!(switch_views.get() - views_before >= 20_000);
    if let (Some(after), Some(before)) = (relinks(&graph), relinks_before) {
        // Four switches, each moving once per drive.
        assert_eq!(after - before, 40_000, "relinks in 10,000 drives");
    }

    graph.set_shuffle_seed(Some(3));
    for i in 0..100 {
        drive(&mut graph, i);
    }
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for i in 0..10_000 {
        drive(&mut graph, i);
    }
    let shuffled = ALLOCATIONS.load(Ordering::Relaxed) - before;

    assert_eq!(plain, 0, "allocations in 30,000 steady-state transactions");
    assert_eq!(shuffled, 0, "allocations with the shuffle on");
    // Negative controls: the counter sees what does allocate. A construct
    // that fires builds nodes.
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    graph.listen_steps(total, |_| ()).keep();
    assert!(
        ALLOCATIONS.load(Ordering::Relaxed) > before,
        "listen allocates"
    );
    let live = graph.live_nodes();
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    graph.send(opens_in, 1000);
    assert!(
        ALLOCATIONS.load(Ordering::Relaxed) > before,
        "a construct that fires allocates"
    );
    assert_eq!(graph.live_nodes(), live + 2, "its closure built two nodes");
    // The graph it grew is steady again.
    for i in 0..100 {
        drive(&mut graph, i);
    }
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for i in 0..1_000 {
        drive(&mut graph, i);
    }
    assert_eq!(
        ALLOCATIONS.load(Ordering::Relaxed) - before,
        0,
        "allocations in 3,000 transactions after the construct fired"
    );
    assert_eq!(
        openings.get(),
        *graph.sample(opened),
        "the switch follows the cell the closure built"
    );
    assert!(openings.get() > 2000);
    assert_eq!(
        heard.get(),
        *graph.sample(both),
        "the cell listener kept up"
    );
    assert!(steps.get() > 0 && events.get() > 0);
    // The stage 2 listeners kept up with the values they listen to.
    assert_eq!(last_product.get(), *graph.sample(product));
    assert!(currents.get() > 0);
    assert_eq!(recents.get(), graph.sample(recent)[3]);
    assert_eq!(sums.get(), *graph.sample(recent_sum));
    assert!(*graph.sample(seen) > 0 && *graph.sample(running) > 0);
    // The stage 3 listeners kept up with the loops.
    assert_eq!(counts.get(), *graph.sample(counted));
    assert_eq!(counted_cell.get(), *graph.sample(counted));
    assert_eq!(joins.get(), *graph.sample(joined));
    assert_eq!(windows.get(), graph.sample(window)[3]);
    assert!(*graph.sample(acc_fwd) > 0 && *graph.sample(last) > 0);
    // The stage 4 listeners heard every child.
    assert!(child_events.get() > 0);
    assert_eq!(last_children.get(), *graph.sample(last_child));
    assert!(countdowns.get() > 0 && *graph.sample(counted_down) > 0);
    // The stage 5 listeners kept up with the switches.
    assert_eq!(switch_steps.get(), *graph.sample(switched));
    assert!(follows.get() > 0 && takes.get() > 0);
}
