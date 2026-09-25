// SPDX-License-Identifier: MPL-2.0

//! Instruction-count benchmarks, run in CI as the regression gate.
//!
//! CI runs this twice in one job, first on the base commit and then on the
//! head commit, so the second run is compared against the first and fails on a
//! regression beyond the configured limit.

use iai_callgrind::{
    EventKind, LibraryBenchmarkConfig, RegressionConfig, library_benchmark,
    library_benchmark_group, main,
};
use std::hint::black_box;

#[library_benchmark]
#[bench::small(1_000)]
fn placeholder(n: u64) -> u64 {
    black_box(bough_bench::placeholder(n))
}

library_benchmark_group!(name = shapes; benchmarks = placeholder);

main!(
    config = LibraryBenchmarkConfig::default()
        .regression(RegressionConfig::default().limits([(EventKind::Ir, 5.0)]).fail_fast(true));
    library_benchmark_groups = shapes
);
