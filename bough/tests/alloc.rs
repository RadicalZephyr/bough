//! RFD 3's rule, a claim under test from stage 1: a graph that is not
//! growing never allocates per transaction. A counting global allocator,
//! alone in its test binary so no other test allocates concurrently. The
//! graph covers a share, a fused chain, a coalescing input, a merge, an
//! or_else, snapshots, a gate, a hold, a stream listener and cell
//! listeners. Later stages widen it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell as StdCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bough::{Graph, Source};

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

// Test scaffolding: `GlobalAlloc` is an unsafe trait. The crate under test
// forbids `unsafe`.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn steady_state_transactions_do_not_allocate() {
    let (mut graph, (numbers_in, bumps_in, open_in, total, both, merged)) = Graph::build(|b| {
        let (numbers, numbers_in) = b.input::<u64>();
        let numbers = numbers.share(b);
        let total = numbers
            .map(|x| x * 2)
            .filter(|x| x % 3 != 0)
            .map(|x| x + 1)
            .hold(b, 0u64);
        let (bumps, bumps_in) = b.input_coalescing(|a: u64, b| a + b);
        let merged = numbers
            .map(|x| x + 1)
            .merge(b, bumps, |l, r| l + r)
            .share(b);
        let (open, open_in) = b.input_cell(true);
        let nothing = b.never::<u64>();
        let both = merged
            .snapshot(total, |m, t| m + t)
            .gate(open)
            .or_else(b, nothing)
            .hold(b, 0u64);
        (numbers_in, bumps_in, open_in, total, both, merged)
    });
    let heard = Rc::new(StdCell::new(0u64));
    let recorder = heard.clone();
    graph.listen_cell(both, move |v| recorder.set(*v)).keep();
    let steps = Rc::new(StdCell::new(0u64));
    let count = steps.clone();
    graph
        .listen_steps(total, move |_| count.set(count.get() + 1))
        .keep();
    let events = Rc::new(StdCell::new(0u64));
    let sum = events.clone();
    graph
        .listen(merged, move |m| sum.set(sum.get().wrapping_add(m)))
        .keep();

    let drive = |graph: &mut Graph, i: u64| {
        graph.send(numbers_in, i);
        graph.transaction(|tx| {
            tx.send(bumps_in, i);
            tx.send(bumps_in, 1);
            tx.send(numbers_in, i + 1);
        });
        graph.send(open_in, i % 4 != 0);
    };
    for i in 0..100 {
        drive(&mut graph, i); // warm up: reused buffers reach their size
    }
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for i in 0..10_000 {
        drive(&mut graph, i);
    }
    let plain = ALLOCATIONS.load(Ordering::Relaxed) - before;

    graph.set_shuffle_seed(Some(3));
    for i in 0..100 {
        drive(&mut graph, i);
    }
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for i in 0..10_000 {
        drive(&mut graph, i);
    }
    let shuffled = ALLOCATIONS.load(Ordering::Relaxed) - before;

    assert_eq!(plain, 0, "allocations in 30,000 steady-state transactions");
    assert_eq!(shuffled, 0, "allocations with the shuffle on");
    // A negative control: the counter sees what does allocate.
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    graph.listen_steps(total, |_| ()).keep();
    assert!(
        ALLOCATIONS.load(Ordering::Relaxed) > before,
        "listen allocates"
    );
    assert_eq!(
        heard.get(),
        *graph.sample(both),
        "the cell listener kept up"
    );
    assert!(steps.get() > 0 && events.get() > 0);
}
