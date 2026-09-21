//! Benchmark shapes and baselines.
//!
//! The UI, frame and shallow shapes and their hand-written imperative
//! baselines land with the benchmark increment. Until then the two bench
//! targets exercise the harnesses only.

/// A placeholder workload so both harnesses run end to end.
#[inline(never)]
pub fn placeholder(n: u64) -> u64 {
    (0..n).fold(0, |acc, i| acc.wrapping_add(i))
}
