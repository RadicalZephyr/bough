//! The smoke graph runs: every node kind of the engine so far, in one graph
//! per mode. Its real job is the bare-metal library build with
//! `--features smoke`, which pushes the generic engine through code
//! generation for the target.

#[cfg(feature = "smoke")]
#[test]
fn the_smoke_graph_runs_in_every_mode() {
    // n = 1: total = (1 + 1) * 1 = 2; first = 100; out = 1 * 3 + 100 + 0.
    // Then digits fold to 12, level becomes 2, words fold to "ab".
    // n = 2: total = 3 * 2 = 6; out = 2 * 3 + 12 = 18.
    // Each mode gives 18 + 6 + 2, and the host has both modes.
    assert_eq!(bough::smoke(), 2 * 26);
}
