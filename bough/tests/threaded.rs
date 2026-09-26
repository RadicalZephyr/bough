//! A `Threaded` graph is `Send` because every stored value is, with no
//! `unsafe impl` (RFD 6): built on one thread, driven on another.

use std::sync::{Arc, Mutex};
use std::thread;

use bough::{Lift, Listener, Runtime, Source, State, Threaded};

#[test]
fn a_threaded_graph_is_send_and_runs_on_another_thread() {
    fn assert_send<T: Send>() {}
    assert_send::<Runtime<Threaded>>();
    assert_send::<Listener>();

    let (mut graph, (numbers_in, words_in, doubled, sentence)) = Runtime::build_threaded(|b| {
        let (numbers, numbers_in) = b.input::<u64>();
        let doubled = numbers.map(|n| n * 2).hold(b, 0u64);
        let (words, words_in) = b.input_coalescing(|a: String, b: String| a + " " + &b);
        (numbers_in, words_in, doubled, words.hold(b, String::new()))
    });
    let heard = Arc::new(Mutex::new(Vec::new()));
    let writer = heard.clone();
    let listener = graph.listen_steps(doubled, move |v| writer.lock().unwrap().push(*v));
    graph.send(numbers_in, 1);

    let driver = thread::spawn(move || {
        for k in 2..=5 {
            graph.send(numbers_in, k);
        }
        graph.transaction(|tx| {
            tx.send(words_in, "moved".to_string());
            tx.send(words_in, "across".to_string());
        });
        // The listener's handle may be dropped on this thread too.
        drop(listener);
        graph.send(numbers_in, 6);
        (*graph.sample(doubled), graph.sample(sentence).clone())
    });
    let (last, words) = driver.join().unwrap();
    assert_eq!(last, 12);
    assert_eq!(words, "moved across");
    assert_eq!(*heard.lock().unwrap(), [2, 4, 6, 8, 10]);
}

#[test]
fn a_threaded_graph_can_live_behind_a_mutex() {
    let (graph, (numbers_in, latest)) = Runtime::build_threaded(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.hold(b, 0u32))
    });
    let shared = Arc::new(Mutex::new(graph));
    let workers: Vec<_> = (1..=4u32)
        .map(|k| {
            let shared = shared.clone();
            thread::spawn(move || shared.lock().unwrap().send(numbers_in, k))
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let graph = shared.lock().unwrap();
    assert!((1..=4).contains(graph.sample(latest)));
}

#[test]
fn a_threaded_graph_runs_every_cell_operation_on_another_thread() {
    let (mut graph, (numbers_in, level_in, (label, product, total, log, labels, steps))) =
        Runtime::build_threaded(|b| {
            let (numbers, numbers_in) = b.input::<u64>();
            let numbers = numbers.share(b);
            let (level, level_in) = b.input_cell(2u64);
            let label = level.map_cell(b, |l| format!("level {l}"));
            let product = (level, numbers.hold(b, 1u64)).lift(b, |l, n| l * n);
            let total = numbers.accumulate(b, 0u64, |n, t| t + n);
            let log = numbers.accumulate_mut(b, Vec::new(), |n, log: &mut Vec<u64>| log.push(n));
            let labels = numbers
                .scan(b, 0u64, |n, k| (format!("#{k}: {n}"), k + 1))
                .hold(b, String::new());
            let steps = product.steps_with_current(b).hold(b, 0u64);
            (
                numbers_in,
                level_in,
                (label, product, total, log, labels, steps),
            )
        });
    let heard = Arc::new(Mutex::new(Vec::new()));
    let writer = heard.clone();
    graph
        .listen_cell(label, move |l: &String| {
            writer.lock().unwrap().push(l.clone())
        })
        .keep();
    let sizes = Arc::new(Mutex::new(Vec::new()));
    let writer = sizes.clone();
    graph
        .listen_steps(log, move |l| writer.lock().unwrap().push(l.len()))
        .keep();
    let driver = thread::spawn(move || {
        graph.send(numbers_in, 3);
        graph.transaction(|tx| {
            tx.send(level_in, 5);
            tx.send(numbers_in, 4);
        });
        (
            *graph.sample(product),
            *graph.sample(total),
            graph.sample(log).clone(),
            graph.sample(labels).clone(),
            *graph.sample(steps),
        )
    });
    let (product, total, log, labels, steps) = driver.join().unwrap();
    assert_eq!((product, total, steps), (20, 7, 20));
    assert_eq!(log, [3, 4]);
    assert_eq!(labels, "#1: 4");
    assert_eq!(*heard.lock().unwrap(), ["level 2", "level 5"]);
    assert_eq!(*sizes.lock().unwrap(), [1, 2]);
}

/// Every loop kind in a Threaded graph: a cell loop with a steps view of
/// its forward, a stream loop through a hold, and a state loop. The stream
/// loop's closer requires the mode to accept the event type, which u64
/// satisfies. Built on one thread, driven and sampled on another.
#[test]
fn a_threaded_graph_runs_every_loop_kind_on_another_thread() {
    let (mut graph, (ticks_in, (count, views, total, log))) = Runtime::build_threaded(|b| {
        let (ticks, ticks_in) = b.input::<u64>();
        let ticks = ticks.share(b);
        let (count, count_loop) = b.cell_loop::<u64>();
        let views = count.steps(b).hold(b, 0u64);
        let next = ticks.snapshot(count, |_, n| n + 1).hold(b, 0u64);
        count_loop.close(b, next);
        let (sums, sums_loop) = b.stream_loop::<u64>();
        let total = sums.hold(b, 0u64);
        sums_loop.close(b, ticks.snapshot(total, |t, s| t + s));
        let (log, log_loop) = b.state_loop::<Vec<u64>>();
        let entries = ticks
            .snapshot(log, |t, l| t * 10 + l.len() as u64)
            .accumulate_mut(b, Vec::new(), |e, l: &mut Vec<u64>| l.push(e));
        log_loop.close(b, entries);
        (ticks_in, (count, views, total, log))
    });
    let heard = Arc::new(Mutex::new(Vec::new()));
    let writer = heard.clone();
    graph
        .listen_cell(count, move |n| writer.lock().unwrap().push(*n))
        .keep();
    let driver = thread::spawn(move || {
        for t in 1..=3 {
            graph.send(ticks_in, t);
        }
        (
            *graph.sample(count),
            *graph.sample(views),
            *graph.sample(total),
            graph.sample(log).clone(),
        )
    });
    let (count, views, total, log) = driver.join().unwrap();
    assert_eq!((count, views, total), (3, 3, 6));
    assert_eq!(log, [10, 21, 32]);
    assert_eq!(*heard.lock().unwrap(), [0, 1, 2, 3]);
}

/// Child transactions in a Threaded graph: a split whose elements feed a
/// loop through a defer, which halves each element above 1 one child level
/// down, and an accumulator over both. split requires the mode to accept
/// the iterator and the elements, defer the event, which Vec<u64>, its
/// iterator and u64 satisfy. Built on one thread, driven and sampled on
/// another, whose children run there too. Send [4, 1]: 4 at [1,0], 2 at
/// [1,0,0], 1 at [1,0,0,0], then 1 at [1,1].
#[test]
fn a_threaded_graph_runs_split_and_defer_children_on_another_thread() {
    let (mut graph, (lists_in, events, total)) = Runtime::build_threaded(|b| {
        let (lists, lists_in) = b.input::<Vec<u64>>();
        let (halves, halves_loop) = b.stream_loop::<u64>();
        let again = halves.filter(|n| *n > 1).map(|n| n / 2).defer(b);
        let events = lists.split(b).or_else(b, again).share(b);
        halves_loop.close(b, events);
        let total = events.accumulate(b, 0u64, |n, t| t + n);
        (lists_in, events, total)
    });
    let heard = Arc::new(Mutex::new(Vec::new()));
    let writer = heard.clone();
    graph
        .listen(events, move |n| writer.lock().unwrap().push(n))
        .keep();
    let driver = thread::spawn(move || {
        graph.send(lists_in, vec![4, 1]);
        let first = *graph.sample(total);
        graph.send(lists_in, vec![3]);
        (first, *graph.sample(total))
    });
    assert_eq!(driver.join().unwrap(), (8, 12));
    assert_eq!(*heard.lock().unwrap(), [4, 2, 1, 1, 3, 1]);
}

/// Every switch kind in a Threaded graph: a switch_cell with a steps view,
/// a switch_cell over states, a switch_stream among shared streams, and one
/// among linear streams in constant cells selected through a switch_cell.
/// switch_stream requires the mode to accept the event type, which u64
/// satisfies. Built on one thread, driven and sampled on another, where
/// the switches move: at [2] each moves, the switch_cell to a map_cell
/// whose value after the instant the steps view reads, and each
/// switch_stream still forwards its old stream's event.
#[test]
fn a_threaded_graph_runs_every_switch_kind_on_another_thread() {
    let (mut graph, (numbers_in, pick_in, cells)) = Runtime::build_threaded(|b| {
        let (numbers, numbers_in) = b.input::<u64>();
        let numbers = numbers.share(b);
        let (pick, pick_in) = b.input::<bool>();
        let pick = pick.share(b);
        let latest = numbers.hold(b, 0u64);
        let doubled = latest.map_cell(b, |n| n * 2);
        let chosen = pick
            .map(move |p| if p { doubled } else { latest })
            .hold(b, latest);
        b.depends(&chosen, &[&doubled, &latest]);
        let shown = chosen.switch_cell(b);
        let views = shown.steps(b).accumulate(b, 0u64, |v, t| t + v);
        let all = numbers.accumulate_mut(b, Vec::new(), |n, v: &mut Vec<u64>| v.push(n));
        let odd =
            numbers
                .filter(|n| n % 2 == 1)
                .accumulate_mut(b, Vec::new(), |n, v: &mut Vec<u64>| v.push(n));
        let chosen = pick.map(move |p| if p { odd } else { all }).hold(b, all);
        b.depends(&chosen, &[&odd, &all]);
        let current: State<Vec<u64>> = chosen.switch_cell(b);
        let evens = numbers.filter(|n| n % 2 == 0).share(b);
        let chosen = pick
            .map(move |p| if p { evens } else { numbers })
            .hold(b, numbers);
        b.depends(&chosen, &[&evens, &numbers]);
        let followed = chosen.switch_stream(b).accumulate(b, 0u64, |n, t| t + n);
        let tens = numbers.map(|n| n * 10).node(b);
        let tens = b.constant(tens);
        let hundreds = numbers.map(|n| n * 100).node(b);
        let hundreds = b.constant(hundreds);
        let chosen = pick
            .map(move |p| if p { hundreds } else { tens })
            .hold(b, tens);
        b.depends(&chosen, &[&hundreds, &tens]);
        let lines = chosen.switch_cell(b);
        let taken = lines.switch_stream(b).accumulate(b, 0u64, |n, t| t + n);
        (
            numbers_in,
            pick_in,
            (shown, views, current, followed, taken),
        )
    });
    let heard = Arc::new(Mutex::new(Vec::new()));
    let writer = heard.clone();
    graph
        .listen_steps(cells.2, move |v| writer.lock().unwrap().push(v.len()))
        .keep();
    let driver = thread::spawn(move || {
        graph.send(numbers_in, 3);
        graph.transaction(|tx| {
            tx.send(pick_in, true);
            tx.send(numbers_in, 4);
        });
        graph.send(numbers_in, 5);
        let (shown, views, current, followed, taken) = cells;
        (
            [*graph.sample(shown), *graph.sample(views)],
            graph.sample(current).clone(),
            [*graph.sample(followed), *graph.sample(taken)],
        )
    });
    let (shown, current, streams) = driver.join().unwrap();
    // shown: 3, then doubled at [2], 8, then 10; its steps sum to 21.
    assert_eq!(shown, [10, 21]);
    assert_eq!(current, [3, 5]);
    // followed: 3, and 4 at the switch instant, then only evens: 5 drops.
    // taken: 30, and 40 at the switch instant, then 500.
    assert_eq!(streams, [7, 570]);
    assert_eq!(*heard.lock().unwrap(), [1, 1, 2]);
}

/// Stage 6's construct in a `Threaded` graph: the closure and its output
/// go through the mode's `Accepts` bounds, which `Send` closures over
/// tokens and `u64` meet. Built on one thread; on another, each event runs
/// the closure there, which builds a hold, a loop and a switch_stream's
/// inner, and I/O code receives an input a closure built and sends to it.
#[test]
fn a_threaded_graph_runs_construct_closures_on_another_thread() {
    let (mut graph, (numbers_in, opens_in, made, cells)) = Runtime::build_threaded(|b| {
        let (numbers, numbers_in) = b.input::<u64>();
        let numbers = numbers.share(b);
        let (opens, opens_in) = b.input::<u64>();
        let opens = opens.share(b);
        let zero = b.constant(0u64);
        let latest = opens
            .construct(b, move |b, k| numbers.map(move |n| n + k).hold(b, k))
            .hold(b, zero)
            .switch_cell(b);
        let counted = opens
            .construct(b, move |b, _| {
                let (count, count_loop) = b.cell_loop::<u64>();
                let next = numbers.snapshot(count, |_, c| c + 1).hold(b, 0u64);
                count_loop.close(b, next);
                count
            })
            .hold(b, zero)
            .switch_cell(b);
        let first = numbers.map(|n| n).node(b);
        let taken = opens
            .construct(b, move |b, k| numbers.map(move |n| n * k).node(b))
            .hold(b, first)
            .switch_stream(b)
            .accumulate(b, 0u64, |n, t| t + n);
        let made = opens.construct(b, |b, k| {
            let (sends, sends_in) = b.input::<u64>();
            (sends_in, sends.accumulate(b, k, |n, t| t + n))
        });
        (numbers_in, opens_in, made, (latest, counted, taken))
    });
    let received = Arc::new(Mutex::new(Vec::new()));
    let writer = received.clone();
    graph
        .listen(made, move |m| writer.lock().unwrap().push(m))
        .keep();
    let driver = thread::spawn(move || {
        graph.send(numbers_in, 1);
        graph.send(opens_in, 10);
        graph.send(numbers_in, 2);
        let (sends_in, total) = received.lock().unwrap()[0];
        graph.send(sends_in, 5);
        let (latest, counted, taken) = cells;
        [
            *graph.sample(latest),
            *graph.sample(counted),
            *graph.sample(taken),
            *graph.sample(total),
        ]
    });
    // latest: 10 from [2], 2 + 10 at [3]. counted: built at [2], one
    // number since. taken: 1 and, the old stream at [2], nothing; then
    // 2 * 10. total: 10 + 5.
    assert_eq!(driver.join().unwrap(), [12, 1, 21, 15]);
}
