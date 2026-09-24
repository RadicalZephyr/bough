//! Wall-clock benchmarks, run by hand with `cargo bench -p bough-bench --bench shapes`.
//!
//! The bar is a factor of three against the baseline with a realistic
//! payload; the trivial-payload numbers are information.

use std::hint::black_box;

use bough_bench::{Frame, FrameBaseline, Shallow, ShallowBaseline, payload};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

fn the_payload(c: &mut Criterion) {
    c.bench_function("payload", |b| b.iter(|| payload(black_box(7))));
}

fn shallow(c: &mut Criterion) {
    let mut group = c.benchmark_group("shallow");
    for (name, share, heavy) in [
        ("plain", false, false),
        ("share", true, false),
        ("plain_payload", false, true),
        ("share_payload", true, true),
    ] {
        group.bench_function(BenchmarkId::new("bough", name), |b| {
            let mut shape = Shallow::new(share, heavy);
            let mut k = 0u64;
            b.iter(|| {
                k += 1;
                shape.send(black_box(k));
            });
            black_box(shape.value());
        });
    }
    for (name, heavy) in [("trivial", false), ("payload", true)] {
        group.bench_function(BenchmarkId::new("baseline", name), |b| {
            let mut base = ShallowBaseline { held: 0, heavy };
            let mut k = 0u64;
            b.iter(|| {
                k += 1;
                base.send(black_box(k));
                black_box(base.held)
            });
        });
    }
    group.finish();
}

fn frame(c: &mut Criterion) {
    let mut group = c.benchmark_group("frame");
    group.bench_function("bough", |b| {
        let mut shape = Frame::new();
        let mut k = 0u64;
        b.iter(|| {
            k += 1;
            shape.frame(black_box(k));
        });
        black_box(shape.checksum());
    });
    group.bench_function("baseline", |b| {
        let mut base = FrameBaseline::new();
        let mut k = 0u64;
        b.iter(|| {
            k += 1;
            base.frame(black_box(k));
        });
        black_box(base.checksum());
    });
    group.finish();
}

criterion_group!(benches, the_payload, shallow, frame);
criterion_main!(benches);
