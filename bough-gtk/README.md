# bough-gtk: a spike

Not API, and not meant to merge. This crate shows what GTK 4 code looks
like on bough's same-thread handle, so that the handle can be judged
from real widgets. The handle is on this branch, in `bough/src/handle.rs`.

## Why a handle

GTK runs a signal handler at once, even while the graph is busy:

- a listener writes to an entry, and the entry runs its `changed`
  handler;
- a listener changes a list view's model, and GTK binds the new row
  there and then;
- `listen_cell` calls its listener as it registers, and a widget write
  in that call runs a handler.

With the graph behind `Rc<RefCell<Graph>>`, each of these is a double
borrow. A panic cannot unwind through a gtk-rs handler, so the process
aborts. With the handle, a call made while the graph is busy waits and
runs right after the transaction.

## What is here

- `src/lib.rs`: the helpers a `bough-gtk` crate would offer:
  - `send` and `sender`, for signal handlers;
  - `bind_label`;
  - `bind_entry`, which blocks its own handler while it writes;
  - `tie`, which drops a listener when its widget goes;
  - `list_factory` and `sync_store`, for list views;
  - `spawn_driver`, a future on the main loop that pumps.
- `examples/app/mod.rs`: a graph that knows nothing of GTK, and a row
  component that wires itself through the handle.
- `examples/demo.rs`: one window with all of it.
- `tests/scenarios.rs`: nine headless scenarios. Each drives real widgets
  by emitting their signals, and runs in its own process, so that an
  abort shows as one.

## Running it

It needs GTK 4.14 or later and its development files, such as
`libgtk-4-dev` on Ubuntu 24.04. The crate is outside the bough
workspace, so the workspace builds without GTK.

```sh
cd bough-gtk
cargo run --example demo
xvfb-run -a cargo test   # the scenarios, headless
```

Without a display, the scenarios skip. A name as an argument runs only
the scenarios whose names contain it: `cargo test -- entry`.

## What the scenarios show

- Rows wire themselves in the listener that hears of them. Removing one
  drops its widget, which drops its listener, and a collection frees its
  nodes.
- A list view binds new rows inside the listener that changed its model,
  and the labels are right before the main loop runs.
- A listener's first call can set off a handler that sends, without an
  abort.
- The driver runs remote sends from another thread. It also runs the
  queue between the units of one pump, so rows one pump opens survive the
  collection before the next unit.
- Dropping the owner ends the driver, and later handlers find the graph
  gone.
- Waiting does not cure an echo. A two-way binding that does not block
  its own handler flips between "HELLO" and "" at every turn of the main
  loop, without an abort and without settling. `bind_entry` blocks the
  handler, and settles.

## What is missing

- A panic still aborts. A stale token in a call that waited panics in a
  debug build when the call runs, and `Io::pump` panics as `Graph::pump`
  does. The handle has no `try_` forms yet.
- `sync_store` only appends and removes; it does not move rows.
- Nothing tests a real scroll. When the investigation scrolled from code,
  GTK bound rows outside the frame clock's paint.
