//! Instruction counts of the rollback probe's two shapes, run by hand with
//! each mechanism's feature: `Watched`, where `force` computes a watched
//! chain before commit, and `Rebind`, a thousand redefinitions, where the
//! mechanisms stage, log and move. Not part of the CI gate.

use gungraun::{
    Callgrind, LibraryBenchmarkConfig, library_benchmark, library_benchmark_group, main,
};
use std::hint::black_box;

use bough_bench::{Rebind, Watched};

// A thousand sends through a watched chain of three read-through cells,
// build included.
#[library_benchmark]
fn watched() -> u64 {
    let mut shape = Watched::new();
    for k in 0..1000 {
        shape.send(black_box(k));
    }
    black_box(shape.seen.get())
}

// A thousand redefinitions of a watched binding, build included.
#[library_benchmark]
fn rebind() -> u64 {
    let mut shape = Rebind::new();
    for k in 0..1000 {
        shape.redefine(black_box(k));
    }
    black_box(shape.seen.get())
}

library_benchmark_group!(name = probe; benchmarks = watched, rebind);

main!(
    config = LibraryBenchmarkConfig::default().tool(Callgrind::default());
    library_benchmark_groups = probe
);
