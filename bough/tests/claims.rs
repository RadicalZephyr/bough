//! The load-bearing claims of the engine's architecture that stage 1 can
//! show: an erased arena whose nodes read each other's slots by linear take
//! or shared clone, a chain fused into one node, and `sample` handing out a
//! reference from `&self` without running anything.

use std::cell::{Cell as StdCell, RefCell};
use std::rc::Rc;

use bough::{Runtime, Source, Trace, Tracer};

fn recorder<T: 'static>() -> (Rc<RefCell<Vec<T>>>, impl FnMut(T) + 'static) {
    let log = Rc::new(RefCell::new(Vec::new()));
    let writer = log.clone();
    (log, move |v| writer.borrow_mut().push(v))
}

/// An event that counts its clones and its drops.
struct Counted {
    value: u32,
    clones: Rc<StdCell<u32>>,
    drops: Rc<StdCell<u32>>,
}

impl Clone for Counted {
    fn clone(&self) -> Self {
        self.clones.set(self.clones.get() + 1);
        Counted {
            value: self.value,
            clones: self.clones.clone(),
            drops: self.drops.clone(),
        }
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        self.drops.set(self.drops.get() + 1);
    }
}

impl Trace for Counted {
    fn trace(&self, _tracer: &mut Tracer) {}
}

/// An event with no `Clone`, which only a linear path can carry.
struct NoClone(u32);

impl Trace for NoClone {
    fn trace(&self, _tracer: &mut Tracer) {}
}

// ----------------------------------------------------------- claim 1

#[test]
fn claim1_an_erased_arena_moves_linear_events_and_clones_shared_ones() {
    let clones = Rc::new(StdCell::new(0));
    let drops = Rc::new(StdCell::new(0));
    let counted = |value| Counted {
        value,
        clones: clones.clone(),
        drops: drops.clone(),
    };
    let initial = counted(0);
    let (mut graph, edge) = Runtime::build(move |b| {
        // Linear: an input of a type with no Clone, consumed by one hold.
        let (linear, linear_in) = b.input::<NoClone>();
        let last_linear = linear.hold(b, NoClone(0));
        // Shared: one shared node, three consumers of different kinds.
        let (shared, shared_in) = b.input::<Counted>();
        let shared = shared.share(b);
        let lengths = shared.map(|c| c.value * 10).hold(b, 0u32);
        let bangs = shared.map(|c| c.value + 1).node(b);
        // Returned, so a root: a hold no root reaches is collected at the
        // first send, and neither clones nor keeps anything.
        let keeps = shared.hold(b, initial);
        (linear_in, shared_in, last_linear, lengths, bangs, keeps)
    });
    let (linear_in, shared_in, last_linear, lengths, bangs, _keeps) = edge.keep();
    let (seen, on) = recorder();
    graph.listen(bangs, on).keep();

    graph.send(linear_in, NoClone(5));
    graph.send(linear_in, NoClone(7));
    assert_eq!(graph.sample(last_linear).0, 7);

    graph.send(shared_in, counted(3));
    assert_eq!(*graph.sample(lengths), 30);
    assert_eq!(*seen.borrow(), [4]);
    // Each of the three consumers cloned the event once; the shared slot
    // keeps the original for any other consumer.
    assert_eq!(clones.get(), 3);
    // The two maps dropped their clones; the hold kept its clone and
    // dropped the initial value at commit.
    assert_eq!(drops.get(), 3);

    graph.send(shared_in, counted(4));
    assert_eq!(clones.get(), 6);
    // The new event replaced the old one in the shared slot, and the hold's
    // new clone replaced its old one at commit.
    assert_eq!(drops.get(), 3 + 2 + 2);
}

// ----------------------------------------------------------- claim 2

#[test]
fn claim2_a_chain_fuses_into_one_node_whose_snapshot_reads_the_value_before_the_instant() {
    let calls = Rc::new(StdCell::new(0u32));
    let counter = calls.clone();
    let (mut graph, edge) = Runtime::build(move |b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let (limit_events, limit_in) = b.input::<u32>();
        let limit = limit_events.hold(b, 10);
        let out = numbers
            .map(move |x| {
                counter.set(counter.get() + 1);
                x * 2
            })
            .filter(|x| *x > 2)
            .snapshot(limit, |x, l| x.min(*l))
            .hold(b, 0u32);
        (numbers_in, limit_in, out)
    });
    let (numbers_in, limit_in, out) = edge.keep();
    // Two inputs, the limit's hold and the chain's hold: the three adapters
    // are inside one node.
    assert_eq!(graph.live_nodes(), 4);

    let (steps, mut on) = recorder();
    graph.listen_steps(out, move |v| on(*v)).keep();

    #[cfg(feature = "statistics")]
    let before = graph.statistics();
    graph.send(numbers_in, 1); // 2, filtered out: no step
    graph.send(numbers_in, 3); // 6
    graph.send(numbers_in, 9); // 18, clipped to 10
    #[cfg(feature = "statistics")]
    {
        let after = graph.statistics();
        assert_eq!(after.ordered - before.ordered, 3, "one node per send");
        assert_eq!(after.evaluations - before.evaluations, 3);
    }
    graph.transaction(|tx| {
        tx.send(limit_in, 100);
        tx.send(numbers_in, 9); // the snapshot reads the limit before the instant
    });
    graph.send(numbers_in, 9); // now 100
    assert_eq!(*steps.borrow(), [6, 10, 10, 18]);
    assert_eq!(calls.get(), 5, "the map ran once per event");
}

// ----------------------------------------------------------- claim 3

#[test]
fn claim3_sample_returns_the_same_reference_twice_and_runs_nothing() {
    let calls = Rc::new(StdCell::new(0u32));
    let counter = calls.clone();
    let (mut graph, edge) = Runtime::build(move |b| {
        let (names, names_in) = b.input::<String>();
        let names = names.share(b);
        let joined = names
            .map(move |n| {
                counter.set(counter.get() + 1);
                n.to_uppercase()
            })
            .hold(b, String::new());
        let count = names.map(|n| n.len()).hold(b, 0usize);
        // Two samples compose in graph code too.
        let text = format!("{}{}", joined.sample(b), count.sample(b));
        assert_eq!(text, "0");
        (names_in, joined, count)
    });
    let (names_in, joined, count) = edge.keep();
    graph.send(names_in, "ada".to_string());
    assert_eq!(calls.get(), 1);

    #[cfg(feature = "statistics")]
    let before = graph.statistics();
    // Two samples compose in one expression.
    let text = format!("{} {}", graph.sample(joined), graph.sample(count));
    assert_eq!(text, "ADA 3");
    let first: &String = graph.sample(joined);
    let second: &String = graph.sample(joined);
    assert!(std::ptr::eq(first, second), "the same committed value");
    assert_eq!(calls.get(), 1, "sampling runs no function");
    #[cfg(feature = "statistics")]
    assert_eq!(graph.statistics(), before, "sampling runs no phase");
}
