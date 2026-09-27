# bough-gtk: a spike

Not API, and not meant to merge. This crate shows what GTK 4 code looks
like on bough's `Io`, so that the I/O edge can be judged from real
widgets. The `Io` is on this branch, in `bough/src/io.rs`.

## Why the `Io`

GTK runs a signal handler at once, even while the runtime pumps:

- a listener writes to an entry, and the entry runs its `changed`
  handler;
- a listener changes a list view's model, and GTK binds the new row
  there and then;
- a cell listener's first call writes a widget, and the write runs a
  handler.

With the runtime behind `Rc<RefCell<Runtime>>`, each of these is a
double borrow. A panic cannot unwind through a gtk-rs handler, so the
process aborts. Through the `Io`, every call waits for the next pump,
so none of them borrows the runtime.

## What is here

- `src/lib.rs`: the helpers a `bough-gtk` crate would offer:
  - `send` and `sender`, for signal handlers;
  - `bind_label`;
  - `bind_entry`, which blocks its own handler while it writes;
  - `tie`, which drops a listener, or the driver, when its widget goes;
  - `list_factory` and `sync_store`, for list views;
  - `spawn_driver`, which hands the runtime to the `Driver`, a future on
    the main loop that pumps it. Dropping the `Driver` stops it.
- `examples/app/mod.rs`: a graph that knows nothing of GTK, and a row
  component that wires itself through the `Io`.
- `examples/demo.rs`: one window with all of it.
- `tests/scenarios.rs`: eleven headless scenarios. Each drives real
  widgets by emitting their signals, and runs in its own process, so
  that an abort shows as one.

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

- Rows wire themselves in the listener that hears of them, and their
  labels' listeners register at the next pump. Removing one drops its
  widget, which drops its listener, and a collection frees its nodes.
- A list view binds new rows inside the listener that changed its model.
  Their labels are right from the next pump, which for the driver is a
  turn of the main loop later: the paint probe's one late frame.
- A listener's first call can set off a handler that sends, without an
  abort.
- The driver runs remote sends from another thread. Rows that one pump
  opens survive the collection after their unit, because a registration
  keeps what it names alive while it waits for the next pump.
- Dropping the `Driver` stops the pumping, the runtime drops at the main
  loop's next turn, and later calls find it gone.
- A unit the pump drops is logged, and the driver pumps again for what
  waited behind it.
- A runtime that panics as it drops, as a debug build's check for a kept
  once-listener does, doesn't abort: the driver catches it.
- Waiting does not cure an echo. A two-way binding that does not block
  its own handler flips between "HELLO" and "" at every turn of the main
  loop, without an abort and without settling. `bind_entry` blocks the
  handler, and settles.

## What is missing

- A panic in a GTK handler still aborts. A panic in a listener the
  driver runs doesn't: glib catches it where it polls the driver, which
  ends the driver, and the poisoned runtime drops with it. No scenario
  tests that.
- `sync_store` only appends and removes; it does not move rows.
- Nothing tests a real scroll. When the investigation scrolled from code,
  GTK bound rows outside the frame clock's paint.
