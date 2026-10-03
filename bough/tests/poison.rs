//! A panic that poisons a runtime marks the poison in its handles on its
//! way out, where panics unwind (RFD 5). So a handle's next call reports
//! `Poisoned`, from any thread, whichever entry the panic left through and
//! whatever raised it: graph code, a listener, a transaction's closure, or
//! a value's `Drop` in a collection. A panic that leaves the runtime usable
//! marks nothing.

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::thread;

use bough::{Anchor, Input, Io, IoError, RemoteIo, Runtime, Source, Trace, Tracer};

/// A value whose `Drop` panics once it's armed.
struct Bomb(Rc<Cell<bool>>);

impl Drop for Bomb {
    fn drop(&mut self) {
        if self.0.get() {
            panic!("a value's drop");
        }
    }
}

impl Trace for Bomb {
    fn trace(&self, _tracer: &mut Tracer) {}
}

/// A runtime whose graph code panics on 13 and whose listener panics on 7,
/// with a bomb that only its own anchor keeps alive.
struct Fixture {
    graph: Runtime,
    numbers_in: Input<u32>,
    held: bough::Cell<u32>,
    io: Io,
    remote: RemoteIo,
    armed: Rc<Cell<bool>>,
    bomb: Option<Anchor>,
}

fn fixture() -> Fixture {
    let armed = Rc::new(Cell::new(false));
    let bomb = Bomb(armed.clone());
    let (mut graph, edge) = Runtime::build(move |b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let checked = numbers
            .map(|n: u32| {
                assert_ne!(n, 13, "graph code");
                n
            })
            .share(b);
        let held = checked.hold(b, 0u32);
        (numbers_in, checked, held, b.constant(bomb))
    });
    let ((numbers_in, checked, held, bomb), edge) = edge.into_parts();
    let (numbers_in, held) = graph.anchor((numbers_in, held)).keep();
    let (_, bomb) = graph.anchor(bomb).into_parts();
    drop(edge);
    graph
        .listen(checked, |n: u32| assert_ne!(n, 7, "a listener"))
        .keep();
    let io = graph.io();
    let remote = graph.remote_io();
    Fixture {
        graph,
        numbers_in,
        held,
        io,
        remote,
        armed,
        bomb: Some(bomb),
    }
}

/// Drops the bomb's anchor and arms it, so the next collection panics.
fn arm(f: &mut Fixture) {
    drop(f.bomb.take());
    f.armed.set(true);
}

/// Each way to poison the runtime, through each entry that can.
type Way = (&'static str, fn(&mut Fixture));

const WAYS: &[Way] = &[
    ("graph code, through send", |f| {
        f.graph.send(f.numbers_in, 13)
    }),
    ("graph code, through try_send", |f| {
        let _ = f.graph.try_send(f.numbers_in, 13);
    }),
    ("a listener, through send", |f| {
        f.graph.send(f.numbers_in, 7)
    }),
    ("a transaction's closure, through transaction", |f| {
        let numbers_in = f.numbers_in;
        f.graph.transaction(|tx| {
            tx.send(numbers_in, 1);
            panic!("a transaction's closure");
        })
    }),
    ("a transaction's closure, through try_transaction", |f| {
        let _ = f
            .graph
            .try_transaction(|_| -> u32 { panic!("a transaction's closure") });
    }),
    ("graph code, through pump", |f| {
        f.io.send(f.numbers_in, 13).unwrap();
        f.graph.pump();
    }),
    ("graph code, through try_pump", |f| {
        f.io.send(f.numbers_in, 13).unwrap();
        let _ = f.graph.try_pump();
    }),
    ("a value's drop, through collect_garbage", |f| {
        arm(f);
        f.graph.collect_garbage();
    }),
    ("a value's drop, through try_collect_garbage", |f| {
        arm(f);
        let _ = f.graph.try_collect_garbage();
    }),
];

/// A fresh fixture, poisoned the given way.
fn poisoned(way: &Way) -> Fixture {
    let mut f = fixture();
    let result = catch_unwind(AssertUnwindSafe(|| (way.1)(&mut f)));
    assert!(result.is_err(), "{}: it panicked", way.0);
    f
}

#[test]
fn every_panic_that_poisons_a_runtime_poisons_its_io_at_once() {
    for way in WAYS {
        let f = poisoned(way);
        assert_eq!(
            f.io.send(f.numbers_in, 1),
            Err(IoError::Poisoned),
            "{}",
            way.0
        );
    }
}

#[test]
fn every_panic_that_poisons_a_runtime_poisons_its_remote_io_at_once() {
    for way in WAYS {
        let f = poisoned(way);
        assert_eq!(
            f.remote.send(f.numbers_in, 1),
            Err(IoError::Poisoned),
            "{}, on the driver's thread",
            way.0
        );
        let other = f.remote.clone();
        let numbers_in = f.numbers_in;
        let elsewhere = thread::spawn(move || other.send(numbers_in, 2));
        assert_eq!(
            elsewhere.join().unwrap(),
            Err(IoError::Poisoned),
            "{}, on another thread",
            way.0
        );
    }
}

/// A tied cell listener runs once its unit is done, outside the
/// transaction, so its panic leaves the runtime usable, and marks nothing.
#[test]
fn a_panic_that_leaves_the_runtime_usable_leaves_its_handles_usable() {
    let mut f = fixture();
    let held = f.held;
    let result = catch_unwind(AssertUnwindSafe(|| {
        f.graph
            .transaction(|tx| tx.listen_cell_once(held, |_| panic!("a tied cell listener")))
    }));
    assert!(result.is_err(), "it panicked");
    assert_eq!(f.io.send(f.numbers_in, 1), Ok(()));
    assert_eq!(f.remote.send(f.numbers_in, 2), Ok(()));
    f.graph.pump();
    assert_eq!(*f.graph.sample(held), 2);
}
