//! A core written once against `Portable` runs under both modes, and a
//! `Local` host mixes it with graph code of its own that is not `Send`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use bough::prelude::*;
use bough::{CollectionPolicy, Tracer};

// ----------------------------------------------------------- the smallest core

/// Finding F23's shape: a core that writes its own closure and stores it.
fn plus_one(b: &mut Portable<'_>, numbers: Stream<u32>) -> Cell<u32> {
    numbers.map(|n| n + 1).hold(b, 0)
}

#[test]
fn a_core_that_stores_its_own_closure_builds_in_both_modes() {
    let mut local = Runtime::local();
    let (local_in, local_out) = local.build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, plus_one(&mut b.portable(), numbers))
    });

    let mut threaded = Runtime::threaded();
    let (threaded_in, threaded_out) = threaded.build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, plus_one(&mut b.portable(), numbers))
    });

    for n in [1, 5, 41] {
        local.send(local_in, n);
        threaded.send(threaded_in, n);
        assert_eq!(*local.sample(local_out), n + 1);
        assert_eq!(*threaded.sample(threaded_out), n + 1);
    }
}

// ----------------------------------------------------------- a GTK-shaped host

/// A core the `Local` graph code feeds: it counts clicks in steps that
/// code's cell gives, and reports the total and each new total.
fn counter(b: &mut Portable<'_>, clicks: Stream<()>, step: Cell<u32>) -> (Cell<u32>, Stream<u32>) {
    let total = clicks
        .snapshot(step, |(), s| *s)
        .accumulate(b, 0u32, |s, t| t + s);
    (total, total.steps(b))
}

/// A second core the `Local` graph code feeds with a cell it built from
/// the first core's output.
fn longest(b: &mut Portable<'_>, label: Cell<String>) -> Cell<usize> {
    label
        .steps(b)
        .map(|l| l.len())
        .accumulate(b, 0usize, |n, m| n.max(*m))
}

/// RFD 6's GTK frontend: a `Local` build holds graph code that is not
/// `Send`, here an `Rc` captured in a closure the graph stores, beside a
/// portable core's outputs. Tokens pass both ways: the `Local` graph code's
/// input and cell into the core, the core's cell and stream into that code,
/// and its `Rc`-built cell into a second core.
#[test]
fn a_local_host_mixes_graph_code_that_is_not_send_with_a_portable_core() {
    let mut graph = Runtime::local();
    let (clicks_in, step_in, total, label, logged, widest) = graph.build(|b| {
        let prefix: Rc<str> = Rc::from("total ");
        let (clicks, clicks_in) = b.input::<()>();
        let (step, step_in) = b.input_cell(1u32);

        // `Local` graph code into the core.
        let (total, totals) = counter(&mut b.portable(), clicks, step);

        // The core into `Local` graph code: a closure capturing an `Rc`,
        // stored in the graph.
        let label = totals
            .map(move |t| format!("{prefix}{t}"))
            .hold(b, String::from("none"));
        let seen = Rc::new(RefCell::new(0usize));
        let reads = seen.clone();
        let logged = label.map_cell(b, move |l| {
            *reads.borrow_mut() += 1;
            l.len()
        });

        // The `Local` graph code's `Rc`-built cell into a second core.
        let widest = longest(&mut b.portable(), label);
        (clicks_in, step_in, total, label, logged, widest)
    });

    graph.send(clicks_in, ());
    graph.send(step_in, 10);
    graph.send(clicks_in, ());
    graph.send(clicks_in, ());
    assert_eq!(*graph.sample(total), 21);
    assert_eq!(graph.sample(label), "total 21");
    assert_eq!(*graph.sample(logged), "total 21".len());
    assert_eq!(*graph.sample(widest), "total 21".len());
}

// ----------------------------------------------------------- the petrol pump

/// What the nozzle did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Nozzle {
    Up,
    Down,
}

/// What the pump's I/O feeds the core: the Sodium book's petrol pump, cut
/// down to one nozzle.
struct PumpInputs {
    nozzle: Stream<Nozzle>,
    pulses: Stream<u32>,
    calibration: Cell<f64>,
    price: Cell<f64>,
    keypad: Stream<u32>,
    clear: Stream<()>,
}

/// What the core gives back.
struct PumpOutputs {
    filling: Cell<bool>,
    litres: Cell<f64>,
    dollars: Cell<f64>,
    preset: Cell<u32>,
    sales: Stream<(f64, f64)>,
}

impl Trace for PumpOutputs {
    fn trace(&self, tracer: &mut Tracer) {
        self.filling.trace(tracer);
        self.litres.trace(tracer);
        self.dollars.trace(tracer);
        self.preset.trace(tracer);
        self.sales.trace(tracer);
    }
}

/// The pump: a fill starts when the nozzle lifts and ends when it hangs up
/// or the preset is reached, through a loop. Each fill constructs a fresh
/// meter, which a switch follows. The sale is the litres and dollars at
/// the end of a fill.
fn pump_core(b: &mut Portable<'_>, i: PumpInputs) -> PumpOutputs {
    let nozzle = i.nozzle.share(b);
    let pulses = i.pulses.share(b);
    let (stop, stop_loop) = b.stream_loop::<()>();

    let starts = nozzle.filter(|n| *n == Nozzle::Up).map(|_| ()).share(b);
    let hangs = nozzle.filter(|n| *n == Nozzle::Down).map(|_| ());
    let ends = hangs.or_else(b, stop).share(b);
    let filling = starts
        .map(|()| true)
        .or_else(b, ends.map(|()| false))
        .hold(b, false);

    // A fresh meter per fill: map, gate, snapshot and accumulate, built at
    // run time and followed by a switch.
    let calibration = i.calibration;
    let meters = starts.construct(b, move |b, ()| {
        pulses
            .gate(filling)
            .snapshot(calibration, |p, c| p as f64 * c)
            .accumulate(b, 0.0, |l, t| t + l)
    });
    b.depends(&meters, &[&pulses, &filling, &calibration]);
    let idle = b.constant(0.0);
    let litres = meters.hold(b, idle).switch_cell(b);

    let price = starts.snapshot(i.price, |(), p| *p).hold(b, 0.0);
    let dollars = (litres, price).lift(b, |l, p| l * p);

    let preset = i
        .keypad
        .map(Some)
        .or_else(b, i.clear.map(|()| None))
        .accumulate(b, 0u32, |key, p| match key {
            Some(d) => (p * 10 + d) % 1000,
            None => 0,
        });

    // The loop: reaching the preset ends the fill.
    let reached = litres
        .steps(b)
        .snapshot(preset, |l, p| *p > 0 && l >= *p as f64)
        .filter(|reached| *reached)
        .map(|_| ());
    stop_loop.close(b, reached);

    let sales = ends
        .gate(filling)
        .snapshot(litres, |(), l| *l)
        .snapshot(dollars, |l, d| (l, *d))
        .node(b);
    PumpOutputs {
        filling,
        litres,
        dollars,
        preset,
        sales,
    }
}

/// The pump's inputs, as I/O code drives them.
#[derive(Clone, Copy)]
struct PumpHandles {
    nozzle: Input<Nozzle>,
    pulses: Input<u32>,
    calibration: Input<f64>,
    price: Input<f64>,
    keypad: Input<u32>,
    clear: Input<()>,
}

impl Trace for PumpHandles {
    fn trace(&self, tracer: &mut Tracer) {
        self.nozzle.trace(tracer);
        self.pulses.trace(tracer);
        self.calibration.trace(tracer);
        self.price.trace(tracer);
        self.keypad.trace(tracer);
        self.clear.trace(tracer);
    }
}

/// One thing the pump's I/O does.
#[derive(Clone, Copy, Debug)]
enum Event {
    Hose(Nozzle),
    Pulses(u32),
    Calibration(f64),
    Price(f64),
    Key(u32),
    Clear,
}

/// Everything the pump shows after a transaction.
type Shown = (bool, f64, f64, u32, Vec<(f64, f64)>);

macro_rules! pump_in {
    ($b:expr) => {{
        let (nozzle, nozzle_in) = $b.input::<Nozzle>();
        let (pulses, pulses_in) = $b.input::<u32>();
        let (calibration, calibration_in) = $b.input_cell(0.1f64);
        let (price, price_in) = $b.input_cell(2.0f64);
        let (keypad, keypad_in) = $b.input::<u32>();
        let (clear, clear_in) = $b.input::<()>();
        let inputs = PumpInputs {
            nozzle,
            pulses,
            calibration,
            price,
            keypad,
            clear,
        };
        let handles = PumpHandles {
            nozzle: nozzle_in,
            pulses: pulses_in,
            calibration: calibration_in,
            price: price_in,
            keypad: keypad_in,
            clear: clear_in,
        };
        (handles, inputs)
    }};
}

/// Drives a runtime of either mode and reads back what it shows.
macro_rules! drive {
    ($graph:expr, $h:expr, $out:expr, $sales:expr, $event:expr) => {{
        match $event {
            Event::Hose(n) => $graph.send($h.nozzle, n),
            Event::Pulses(p) => $graph.send($h.pulses, p),
            Event::Calibration(c) => $graph.send($h.calibration, c),
            Event::Price(p) => $graph.send($h.price, p),
            Event::Key(k) => $graph.send($h.keypad, k),
            Event::Clear => $graph.send($h.clear, ()),
        }
        let shown: Shown = (
            *$graph.sample($out.filling),
            *$graph.sample($out.litres),
            *$graph.sample($out.dollars),
            *$graph.sample($out.preset),
            $sales.lock().unwrap().clone(),
        );
        shown
    }};
}

/// The same core gives the same answers under both modes: the pump is
/// built once in each, both are fed the same events, and every listener
/// output and sample agrees after every transaction.
#[test]
fn the_petrol_pump_core_gives_the_same_answers_in_both_modes() {
    use Event::*;
    use Nozzle::{Down, Up};

    let mut local = Runtime::local();
    let (lh, lo) = local.build(|b| {
        let (handles, inputs) = pump_in!(b);
        (handles, pump_core(&mut b.portable(), inputs))
    });
    let local_sales = Arc::new(Mutex::new(Vec::new()));
    let log = local_sales.clone();
    local
        .listen(lo.sales, move |s| log.lock().unwrap().push(s))
        .keep();

    let mut threaded = Runtime::threaded();
    let (th, to) = threaded.build(|b| {
        let (handles, inputs) = pump_in!(b);
        (handles, pump_core(&mut b.portable(), inputs))
    });
    let threaded_sales = Arc::new(Mutex::new(Vec::new()));
    let log = threaded_sales.clone();
    threaded
        .listen(to.sales, move |s| log.lock().unwrap().push(s))
        .keep();

    let script = [
        Pulses(5), // the nozzle is down: no fill
        Hose(Up),
        Pulses(10),
        Pulses(20),
        Price(3.0), // the fill keeps the price it started at
        Hose(Down),
        Pulses(7),
        Key(1),
        Key(0), // preset 10 litres
        Calibration(0.5),
        Hose(Up),
        Pulses(8),
        Pulses(8),
        Pulses(8), // passes 10 litres: the loop ends the fill
        Pulses(8),
        Hose(Down), // already ended: no second sale
        Clear,
        Hose(Up),
        Pulses(3),
        Hose(Down),
    ];
    let mut last = None;
    for (k, event) in script.into_iter().enumerate() {
        let a = drive!(local, lh, lo, local_sales, event);
        let b = drive!(threaded, th, to, threaded_sales, event);
        assert_eq!(a, b, "after event {k}, {event:?}");
        last = Some(a);
    }
    let (filling, litres, _, preset, sales) = last.unwrap();
    assert!(!filling);
    assert_eq!(preset, 0);
    assert!((litres - 1.5).abs() < 1e-9);
    assert_eq!(sales.len(), 3);
    assert!((sales[0].0 - 3.0).abs() < 1e-9 && (sales[0].1 - 6.0).abs() < 1e-9);
    // The stop and the step that reached the preset are one instant, and
    // the sale snapshots the litres from before it.
    assert!((sales[1].0 - 8.0).abs() < 1e-9 && (sales[1].1 - 24.0).abs() < 1e-9);
}

// ----------------------------------------------------------- collection

/// What a screen shows at a click: the screen, the click, and how many
/// clicks the screen has counted.
type Screen = (u32, u32, u32);

/// A screen: a counter over the clicks, and a stream of what it shows, two
/// nodes.
fn screen(b: &mut Portable<'_>, clicks: Shared<u32>, n: u32) -> Stream<Screen> {
    let count = clicks.accumulate(b, 0u32, |_, c| c + 1);
    clicks
        .snapshot(count, move |click, k| (n, click, k + 1))
        .node(b)
}

/// RFD 4's navigation loop as a portable core: every click of 0 constructs
/// the next screen, a hold keeps the current one, and a switch takes its
/// events.
fn navigation(b: &mut Portable<'_>, clicks: Stream<u32>) -> Shared<Screen> {
    let clicks = clicks.share(b);
    let (navigate, navigate_loop) = b.stream_loop::<u32>();
    let first = screen(b, clicks, 0);
    let screens = navigate.construct(b, move |b, n| screen(b, clicks, n));
    b.depends(&screens, &[&clicks]);
    let current = screens.hold(b, first);
    let events = current.switch_stream(b).share(b);
    let next = events.filter_map(|(n, click, _)| (click == 0).then_some(n + 1));
    navigate_loop.close(b, next);
    events
}

/// Navigates `times` times with manual collection and returns the live
/// nodes once settled, before collecting, and after, and the last event.
macro_rules! navigate {
    ($graph:expr, $clicks_in:expr, $heard:expr, $times:expr) => {{
        $graph.set_collection_policy(CollectionPolicy::Manual);
        $graph.collect_garbage();
        let settled = $graph.live_nodes();
        for _ in 0..$times {
            $graph.send($clicks_in, 0);
            $graph.send($clicks_in, 7);
        }
        let grown = $graph.live_nodes();
        $graph.collect_garbage();
        let collected = $graph.live_nodes();
        let last = *$heard.lock().unwrap().last().unwrap();
        (settled, grown, collected, last)
    }};
}

/// The screens a portable core's `construct` builds are freed once the
/// switch moves off them, in both modes: the live nodes come back to what
/// they were before the navigations.
#[test]
fn nodes_a_portable_construct_builds_are_freed_after_the_switch_leaves_them() {
    let mut local = Runtime::local();
    let (local_in, local_events) = local.build(|b| {
        let (clicks, clicks_in) = b.input::<u32>();
        (clicks_in, navigation(&mut b.portable(), clicks))
    });
    let local_heard = Arc::new(Mutex::new(Vec::new()));
    let log = local_heard.clone();
    local
        .listen(local_events, move |e| log.lock().unwrap().push(e))
        .keep();
    let local_counts = navigate!(local, local_in, local_heard, 10);

    let mut threaded = Runtime::threaded();
    let (threaded_in, threaded_events) = threaded.build(|b| {
        let (clicks, clicks_in) = b.input::<u32>();
        (clicks_in, navigation(&mut b.portable(), clicks))
    });
    let threaded_heard = Arc::new(Mutex::new(Vec::new()));
    let log = threaded_heard.clone();
    threaded
        .listen(threaded_events, move |e| log.lock().unwrap().push(e))
        .keep();
    let threaded_counts = navigate!(threaded, threaded_in, threaded_heard, 10);

    let (settled, grown, collected, last) = local_counts;
    // Ten screens of two nodes each were built; nine of them, and the
    // first, have been left.
    assert_eq!(grown, settled + 20);
    assert_eq!(collected, settled);
    // A screen built at the click of 0 counts that click too.
    assert_eq!(last, (10, 7, 2));
    assert_eq!(threaded_counts, local_counts);
    assert_eq!(
        *threaded_heard.lock().unwrap(),
        *local_heard.lock().unwrap()
    );
}

// ----------------------------------------------------------- Send

/// A portable core built under `Threaded` leaves the runtime `Send`: it is
/// built on this thread, moved to another, and pumped there, with events
/// queued from this one.
#[test]
fn a_threaded_runtime_with_a_portable_core_moves_to_another_thread() {
    fn assert_send<T: Send>(_: &T) {}

    let mut graph = Runtime::threaded();
    let (h, out) = graph.build(|b| {
        let (handles, inputs) = pump_in!(b);
        (handles, pump_core(&mut b.portable(), inputs))
    });
    assert_send(&graph);
    let sales = Arc::new(Mutex::new(Vec::new()));
    let log = sales.clone();
    graph
        .listen(out.sales, move |s| log.lock().unwrap().push(s))
        .keep();

    let remote = graph.remote_io();
    let (queued, ready) = std::sync::mpsc::channel::<()>();
    let pump = std::thread::spawn(move || {
        ready.recv().unwrap();
        graph.pump();
        (
            *graph.sample(out.litres),
            *graph.sample(out.dollars),
            *graph.sample(out.filling),
        )
    });
    remote.send(h.nozzle, Nozzle::Up).unwrap();
    remote.send(h.pulses, 10).unwrap();
    remote.send(h.pulses, 20).unwrap();
    remote.send(h.nozzle, Nozzle::Down).unwrap();
    queued.send(()).unwrap();
    let (litres, dollars, filling) = pump.join().unwrap();
    assert!((litres - 3.0).abs() < 1e-9);
    assert!((dollars - 6.0).abs() < 1e-9);
    assert!(!filling);
    assert_eq!(sales.lock().unwrap().len(), 1);
}
