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
