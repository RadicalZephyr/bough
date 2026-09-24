//! The smoke graph runs: every node kind of the engine so far, in one graph
//! per mode. Its real job is the bare-metal library build with
//! `--features smoke`, which pushes the generic engine through code
//! generation for the target.

#[cfg(feature = "smoke")]
#[test]
fn the_smoke_graph_runs_in_every_mode() {
    // Stage 1. n = 1: total = (1 + 1) * 1 = 2; first = 100;
    // out = 1 * 3 + 100 + 0. Then digits fold to 12, level becomes 2, words
    // fold to "ab". n = 2: total = 3 * 2 = 6; out = 2 * 3 + 12 = 18. So
    // 18 + 6 + 2 = 26.
    //
    // Stage 2. Build: scaled = 10, pair = 10 + 0, six = 1 + 0 + 3 + 1 +
    // 10 + 10 = 25, which steps_with_current carries into `current` in
    // transaction zero. n = 1: count = 1, log = [1], length = 1, mixed = 2;
    // the scan emits 1 + 0; seen = 1 + 0, mixed before the instant; odd
    // becomes true at commit, so the gate, reading false, drops the event.
    // The transaction: scaled = 20, pair = 32 in one step, which
    // pair_steps holds; six = 2 + 12 + 3 + 1 + 20 + 32 = 70, which
    // `current` holds. n = 2: count = 2, length = 2, mixed = 4; the scan
    // emits 2 + 1 = 3; seen = 2 + 2; the gate reads odd = true and passes 2.
    // Cells: 20 + 32 + 70 + 2 + 3 + 32 + 70 + 4 + 2 = 235; states: 2 + 4.
    // So 241.
    //
    // Each mode gives 26 + 241, and the host has both modes.
    assert_eq!(bough::smoke(), 2 * (26 + 241));
}
