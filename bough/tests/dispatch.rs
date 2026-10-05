//! Whether graph code runs in dispatch: a read-through cell's function runs
//! when the cell is read, and commit clears the memo of every one that
//! stepped, so the first read after commit is usually a listener's. The
//! rollback probe's `force` computes every value a cell listener will read
//! before commit instead, so dispatch runs listeners only.
//!
//! Each function here asserts that no listener has run in its instant yet,
//! and each listener says one has. The shapes are the ones a read after
//! commit passes through: a lift, a `map_cell`, a switch moved to a cell
//! built at the instant, which didn't step, a switch whose outer is a
//! read-through cell, and a cell loop.

use std::cell::Cell as StdCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use bough::{Cell, Input, Lift, Runtime, Source};

/// The flag a listener sets and every function checks.
#[derive(Clone, Default)]
struct Dispatching(Rc<StdCell<bool>>);

impl Dispatching {
    fn check(&self, what: &str) {
        assert!(!self.0.get(), "{what} ran in dispatch");
    }
}

struct Graph {
    runtime: Runtime,
    a_in: Input<i64>,
    pick_in: Input<bool>,
    defs_in: Input<i64>,
    cells: [Cell<i64>; 5],
}

fn graph(dispatching: &Dispatching) -> Graph {
    let d = dispatching.clone();
    let (runtime, edge) = Runtime::build(move |b| {
        let (a, a_in) = b.input_cell(1i64);
        let (pick, pick_in) = b.input_cell(false);
        let (defs, defs_in) = b.input::<i64>();
        let d1 = d.clone();
        let sum = (a, a).lift(b, move |x, y| {
            d1.check("the lift");
            x + y
        });
        let d2 = d.clone();
        let tens = a.map_cell(b, move |x| {
            d2.check("the map_cell");
            x * 10
        });
        // A definition built at the instant reads `a`, which didn't fire,
        // so it doesn't step: the switch reads it through its memo.
        let d3 = d.clone();
        let built = defs.construct(b, move |b, k| {
            let d = d3.clone();
            a.map_cell(b, move |x| {
                d.check("a definition built at the instant");
                x + k
            })
        });
        let defined = built.hold(b, tens).switch_cell(b);
        let d4 = d.clone();
        let chooser = pick.map_cell(b, move |p| {
            d4.check("the read-through outer");
            if *p { sum } else { tens }
        });
        let chosen = chooser.switch_cell(b);
        let (forward, closer) = b.cell_loop::<i64>();
        let d5 = d.clone();
        let definition = a.map_cell(b, move |x| {
            d5.check("the loop's definition");
            x - 1
        });
        closer.close(b, definition);
        let d6 = d.clone();
        let looped = forward.map_cell(b, move |x| {
            d6.check("the cell over the loop");
            x * 2
        });
        (a_in, pick_in, defs_in, [sum, tens, defined, chosen, looped])
    });
    let (a_in, pick_in, defs_in, cells) = edge.keep();
    Graph {
        runtime,
        a_in,
        pick_in,
        defs_in,
        cells,
    }
}

/// Builds the graph under `seed`, listens to every cell, and runs the
/// sends, clearing the flag before each. Returns the cells' values after
/// each send, or the first panic's message.
fn run(seed: u64) -> Result<Vec<[i64; 5]>, String> {
    let dispatching = Dispatching::default();
    let mut g = graph(&dispatching);
    g.runtime.set_shuffle_seed(Some(seed));
    for cell in g.cells {
        let d = dispatching.clone();
        g.runtime.listen_cell(cell, move |_| d.0.set(true)).keep();
        // The call at registration is a listener's too.
        dispatching.0.set(false);
    }
    let mut seen = Vec::new();
    let sends: [&dyn Fn(&mut Graph); 6] = [
        &|g| g.runtime.send(g.a_in, 2),
        &|g| g.runtime.send(g.defs_in, 5),
        &|g| g.runtime.send(g.pick_in, true),
        &|g| g.runtime.send(g.a_in, 3),
        &|g| g.runtime.send(g.defs_in, 7),
        &|g| g.runtime.send(g.pick_in, false),
    ];
    for send in sends {
        dispatching.0.set(false);
        catch_unwind(AssertUnwindSafe(|| send(&mut g))).map_err(|payload| {
            match payload.downcast::<String>() {
                Ok(message) => *message,
                Err(_) => "a panic".to_string(),
            }
        })?;
        dispatching.0.set(false);
        seen.push(g.cells.map(|cell| *g.runtime.sample(cell)));
    }
    Ok(seen)
}

/// What every order gives: the semantics don't move with the computing.
#[cfg(feature = "force")]
const SEEN: [[i64; 5]; 6] = [
    [4, 20, 20, 20, 2],
    [4, 20, 7, 20, 2],
    [4, 20, 7, 4, 2],
    [6, 30, 8, 6, 4],
    [6, 30, 10, 6, 4],
    [6, 30, 10, 30, 4],
];

#[cfg(feature = "force")]
#[test]
fn with_force_no_graph_code_runs_in_dispatch() {
    for seed in 0..16 {
        assert_eq!(run(seed), Ok(SEEN.to_vec()), "seed {seed}");
    }
}

#[cfg(not(feature = "force"))]
#[test]
fn without_force_graph_code_runs_in_dispatch() {
    let failed: Vec<String> = (0..16).filter_map(|seed| run(seed).err()).collect();
    assert_eq!(failed.len(), 16, "every order runs a function in dispatch");
    assert!(
        failed.iter().all(|m| m.contains("ran in dispatch")),
        "{failed:?}"
    );
}
