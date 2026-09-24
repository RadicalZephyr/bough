//! A `Threaded` graph is `Send` because every stored value is, with no
//! `unsafe impl` (RFD 6): built on one thread, driven on another.

use std::sync::{Arc, Mutex};
use std::thread;

use bough::{Graph, Listener, Source, Threaded};

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
