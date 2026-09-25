// SPDX-License-Identifier: MPL-2.0

//! Wall-clock benchmarks, run by hand with `cargo bench -p bough-bench --bench shapes`.

use criterion::{Criterion, black_box, criterion_group, criterion_main};

fn placeholder(c: &mut Criterion) {
    c.bench_function("placeholder", |b| {
        b.iter(|| bough_bench::placeholder(black_box(1_000)))
    });
}

criterion_group!(benches, placeholder);
criterion_main!(benches);
