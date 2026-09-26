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

use bough_bench::{FanOut, Frame, Shallow};

// A thousand transactions of the shallow shape, build included.
#[library_benchmark]
#[bench::plain(false)]
#[bench::share(true)]
fn shallow(share: bool) -> u64 {
    let mut shape = Shallow::new(share, false);
    for k in 0..1000 {
        shape.send(black_box(k));
    }
    black_box(shape.value())
}

// Ten frames of the frame shape, build included.
#[library_benchmark]
fn frame() -> u64 {
    let mut shape = Frame::new();
    for k in 0..10 {
        shape.frame(black_box(k));
    }
    black_box(shape.checksum())
}

// A thousand transactions of the fan-out shape, build included.
#[library_benchmark]
fn fan_out() -> u64 {
    let mut shape = FanOut::new();
    for k in 0..1000 {
        shape.send(black_box(k));
    }
    black_box(shape.sum())
}

library_benchmark_group!(name = shapes; benchmarks = shallow, frame, fan_out);

main!(
    config = LibraryBenchmarkConfig::default()
        .regression(RegressionConfig::default().limits([(EventKind::Ir, 5.0)]).fail_fast(true));
    library_benchmark_groups = shapes
);
