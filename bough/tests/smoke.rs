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
    // Stage 3. n = 1: ticks = 1, which its steps view carries into
    // tick_steps; running = 1 + 0; entries = [10], one entry. n = 2:
    // ticks = 2, tick_steps = 2; running = 2 + 1 = 3; entries = [10, 21],
    // two entries. So 2 + 2 + 3 + 2 = 9.
    //
    // Stage 4. Build: the constant's steps_with_current fires 3 at [0],
    // split as [3, 3] into [0,0] and [0,1], so `zero` sums to 6 before
    // build returns. n = 1: the split of [1, 2] and the defer of 1 merge
    // at [1,0] into 1 * 10 + 1, and 2 follows at [1,1], which `children`
    // holds; the countdown fires 1 and stops, so `counted` = 1. n = 2: 22
    // at [3,0], then 3 at [3,1]; the countdown fires 2 at [3] and 1 at
    // [3,0], so `counted` = 1 + 2 + 1. So 3 + 4 + 6 = 13.
    //
    // Stage 5. Build: the switch links `level` and steps at its creation,
    // so its steps view carries 1 into `switched_steps`; the state switch
    // links the log. n = 1, odd: the switch moves to the constant, a quiet
    // inner, and steps to 3; the state switch moves to the entries, [10].
    // The transaction steps `level`, which is deselected. n = 2, even: the
    // switch moves back to `level`, 2, and the state switch to the log,
    // [1, 2]. So 2 + 2 + 2 = 6.
    //
    // Each mode gives 26 + 241 + 9 + 13 + 6, and the host has both modes.
    assert_eq!(bough::smoke(), 2 * (26 + 241 + 9 + 13 + 6));
}
