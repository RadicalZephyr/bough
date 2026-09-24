//! Every node kind of the engine so far, in one graph per mode, so that a
//! library build for a bare-metal target pushes the generic engine through
//! code generation (`--features smoke`). Spike scaffolding, not API.

use alloc::string::String;
use alloc::vec::Vec;

use crate::{Graph, Lift, Source};

/// Builds and drives the smoke graph with one of the two graph
/// constructors. A macro rather than a function generic over the mode: the
/// closures here are made inside the body, and a function generic over the
/// mode cannot name the `Accepts` bound their types need.
macro_rules! smoke_graph {
    ($build:path) => {{
        let (
            mut graph,
            ((n_in, words_in, level_in, digits_in), (total, words, out, merged), stage2),
        ) = $build(|b| {
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

            // Stage 2: read-through cells, lifts at arities two and
            // six, accumulators, an in-place accumulator and the
            // States over it, a scan, and both steps views.
            let scaled = level.map_cell(b, |l| l * 10);
            let pair = (scaled, digits).lift(b, |s, d| s + d);
            let six = (level, digits, three, open, scaled, pair)
                .lift(b, |l, d, t, o, s, p| l + d + t + u32::from(*o) + s + p);
            let count = n.accumulate(b, 0u32, |_, c| c + 1);
            let log = n.accumulate_mut(b, Vec::new(), |v, log: &mut Vec<u32>| log.push(v));
            let length = log.map_cell(b, |l| l.len() as u32);
            let mixed = (length, count).lift(b, |l, c| l + c);
            let labels = n.scan(b, 0u32, |v, s| (v + s, s + 1)).hold(b, 0u32);
            let pair_steps = pair.steps(b).hold(b, 0u32);
            let current = six.steps_with_current(b).hold(b, 0u32);
            let odd = n.accumulate_mut(b, false, |v, odd: &mut bool| *odd = v % 2 == 1);
            let seen = n.snapshot(mixed, |v, m| v + m).hold(b, 0u32);
            let gated = n.gate(odd).hold(b, 0u32);
            assert_eq!(*current.sample(b), 0, "before transaction zero");
            let stage2 = (
                (scaled, pair, six, count, labels),
                (pair_steps, current, seen, gated),
                (log, length, mixed),
            );
            (
                (n_in, words_in, level_in, digits_in),
                (total, words, out, merged),
                stage2,
            )
        });
        let (
            (scaled, pair, six, count, labels),
            (pair_steps, current, seen, gated),
            (log, length, mixed),
        ) = stage2;
        assert_eq!(
            *graph.sample(current),
            25,
            "steps_with_current fired in transaction zero"
        );
        graph.listen_cell(mixed, |_| ()).keep();
        graph.listen_steps(log, |_| ()).keep();
        let _ = graph.try_listen_cell(length, |_| ());
        let _ = graph.try_listen_steps(six, |_| ());
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
        // 18 + 6 + 2 and 241: see the smoke test for the arithmetic.
        let total = *graph.try_sample(total).unwrap_or(&0);
        let stage1 = *graph.sample(out) + total + graph.sample(words).len() as u32;
        let cells = [
            scaled, pair, six, count, labels, pair_steps, current, seen, gated,
        ]
        .iter()
        .map(|c| *graph.sample(*c))
        .sum::<u32>();
        let states = *graph.sample(length) + *graph.try_sample(mixed).unwrap_or(&0);
        stage1 + cells + states
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
