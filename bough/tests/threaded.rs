//! A `Threaded` graph is `Send` because every stored value is, with no
//! `unsafe impl` (RFD 6): built on one thread, driven on another.

use std::sync::{Arc, Mutex};
use std::thread;

use bough::{Graph, Lift, Listener, Source, Threaded};

#[test]
fn a_threaded_graph_is_send_and_runs_on_another_thread() {
    fn assert_send<T: Send>() {}
    assert_send::<Graph<Threaded>>();
    assert_send::<Listener<Threaded>>();

    let (mut graph, (numbers_in, words_in, doubled, sentence)) = Graph::build_threaded(|b| {
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
    let (graph, (numbers_in, latest)) = Graph::build_threaded(|b| {
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
        Graph::build_threaded(|b| {
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
    let (mut graph, (ticks_in, (count, views, total, log))) = Graph::build_threaded(|b| {
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
    let (mut graph, (lists_in, events, total)) = Graph::build_threaded(|b| {
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
