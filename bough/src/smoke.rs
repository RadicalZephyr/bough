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
            ((n_in, words_in, level_in, digits_in), (total, words, out, merged), later),
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

            // Stage 3: a cell loop with a steps view of its forward, taken
            // before close; a stream loop through a hold read by snapshot;
            // and a state loop whose in-place accumulator reads its own
            // forward, with a State over it.
            let (ticks, ticks_loop) = b.cell_loop::<u32>();
            let tick_steps = ticks.steps(b).hold(b, 0u32);
            let next = n.snapshot(ticks, |_, t| t + 1).hold(b, 0u32);
            ticks_loop.close(b, next);
            let (sums, sums_loop) = b.stream_loop::<u32>();
            let running = sums.hold(b, 0u32);
            sums_loop.close(b, n.snapshot(running, |v, s| v + s));
            let (entries, entries_loop) = b.state_loop::<Vec<u32>>();
            let appended = n
                .snapshot(entries, |v, e| v * 10 + e.len() as u32)
                .accumulate_mut(b, Vec::new(), |e, l: &mut Vec<u32>| l.push(e));
            entries_loop.close(b, appended);
            let entry_count = entries.map_cell(b, |e| e.len() as u32);
            let stage3 = (ticks, tick_steps, running, entry_count);

            // Stage 4: a split of an array and a defer, merged at child
            // index 0; a countdown loop through a defer; and a split of a
            // steps_with_current, whose children run in transaction zero.
            let pieces = n.map(|v| [v, v + 1]).split(b);
            let later = n.defer(b);
            let children = pieces.merge(b, later, |p, l| p * 10 + l).hold(b, 0u32);
            let (down, down_loop) = b.stream_loop::<u32>();
            let again = down.filter(|v| *v > 1).map(|v| v - 1).defer(b);
            let countdown = n.or_else(b, again).share(b);
            down_loop.close(b, countdown);
            let counted = countdown.accumulate(b, 0u32, |v, c| c + v);
            let zero = three
                .steps_with_current(b)
                .map(|t| [t, t])
                .split(b)
                .accumulate(b, 0u32, |t, s| s + t);
            let stage4 = ([children, counted, zero], countdown);
            (
                (n_in, words_in, level_in, digits_in),
                (total, words, out, merged),
                (stage2, stage3, stage4),
            )
        });
        let (stage2, (ticks, tick_steps, running, entry_count), (stage4, countdown)) = later;
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
        assert_eq!(
            *graph.sample(stage4[2]),
            6,
            "transaction zero's children ran"
        );
        graph.listen(countdown, |_| ()).keep();
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
        let loops = [ticks, tick_steps, running]
            .iter()
            .map(|c| *graph.sample(*c))
            .sum::<u32>()
            + *graph.sample(entry_count);
        let children = stage4.iter().map(|c| *graph.sample(*c)).sum::<u32>();
        stage1 + cells + states + loops + children
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
