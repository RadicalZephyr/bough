//! Every node kind of the engine so far, in one graph per mode, so that a
//! library build for a bare-metal target pushes the generic engine through
//! code generation (`--features smoke`). Spike scaffolding, not API.

use alloc::string::String;

use crate::{Graph, Source};

/// Builds and drives the smoke graph with one of the two graph
/// constructors. A macro rather than a function generic over the mode: the
/// closures here are made inside the body, and a function generic over the
/// mode cannot name the `Accepts` bound their types need.
macro_rules! smoke_graph {
    ($build:path) => {{
        let (mut graph, ((n_in, words_in, level_in, digits_in), (total, words, out, merged))) =
            $build(|b| {
                let (n, n_in) = b.input::<u32>();
                let n = n.share(b);
                let (level, level_in) = b.input_cell(1u32);
                let (digits, digits_in) = b.input_cell_coalescing(0u32, |a, d| a * 10 + d);
                let (open, _open_in) = b.input_cell(true);
                let three = b.constant(3u32);
                let nothing = b.never::<u32>();
                let total = n
                    .map(|v| v + 1)
                    .filter(|v| *v > 1)
                    .filter_map(Some)
                    .snapshot(level, |v, l| v * l)
                    .gate(open)
                    .hold(b, 0u32);
                let first = n.once().map_to(100u32).node(b);
                let merged = n
                    .snapshot(three, |v, k| v * k)
                    .merge(b, first, |a, c| a + c)
                    .share(b);
                let out = merged
                    .or_else(b, nothing)
                    .snapshot(digits, |v, d| v + d)
                    .hold(b, 0u32);
                let (words, words_in) = b.input_coalescing(|a: String, w: String| a + &w);
                let words = words.hold(b, String::new());
                assert_eq!(*out.sample(b), 0);
                (
                    (n_in, words_in, level_in, digits_in),
                    (total, words, out, merged),
                )
            });
        graph.listen(merged, |_| ()).keep();
        graph.listen_cell(total, |_| ()).keep();
        graph.listen_steps(out, |_| ()).keep();
        let _ = graph.try_listen(merged, |_| ());
        let _ = graph.try_listen_cell(words, |_| ());
        let _ = graph.try_listen_steps(words, |_| ());
        graph.set_shuffle_seed(Some(1));
        graph.send(n_in, 1);
        let _ = graph.try_transaction(|tx| {
            tx.send(digits_in, 1);
            let _ = tx.try_send(digits_in, 2);
            tx.send(level_in, 2);
            tx.send(words_in, String::from("a"));
            tx.send(words_in, String::from("b"));
        });
        graph.set_shuffle_seed(None);
        let _ = graph.try_send(n_in, 2);
        assert!(graph.live_nodes() > 0);
        // 18 + 6 + 2: see the smoke test for the arithmetic.
        let total = *graph.try_sample(total).unwrap_or(&0);
        *graph.sample(out) + total + graph.sample(words).len() as u32
    }};
}

/// Builds and drives a graph with every node kind of the engine so far, in
/// each mode the target has, and returns a sum of the final values.
#[doc(hidden)]
pub fn smoke() -> u32 {
    let local = smoke_graph!(Graph::build);
    #[cfg(target_has_atomic = "ptr")]
    let threaded = smoke_graph!(Graph::build_threaded);
    #[cfg(not(target_has_atomic = "ptr"))]
    let threaded = local;
    local + threaded
}
