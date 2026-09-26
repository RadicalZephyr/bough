# defer-paint-probe

A throwaway GTK 4 probe for one question in the RFD revision. Say every
`Io` call waited for the driver's next pump, as a `Remote` call does.
Would GTK ever paint a widget without its text?

It doesn't use Bough. It emulates the driver instead: one
`glib::spawn_future_local` future holds a queue of closures, and each
time it's polled it runs the ones queued before that poll. Each scenario
runs twice. In **control**, the text is written the moment it's known. In
**deferred**, the write goes through the driver's queue.

## The answer

Yes, in one situation: a list view binds a row inside GTK's frame clock,
in its `update` phase, and that row is already on screen. Kinetic
scrolling, adjustment animations and tick-driven scrolling all bind
there. That frame shows the reused row with its previous item's text,
and the next frame is right. Rows were never blank.

Everywhere else, deferring gave no wrong frames, and control gave none
anywhere.

The reason is priority. The driver runs at default priority (0), and GDK
paints at 120. So a write deferred outside the frame clock lands before
the next paint. A write deferred from inside the frame clock's `update`
phase lands after that frame's paint.

## How it measures

- GTK 4.14.5, gtk4-rs 0.11.5, Xvfb with no window manager, and the GL
  renderer.
- A painted frame is an emission of `GdkSurface::render`. An emission
  hook runs before the window draws, so it sees what the frame shows.
- Emission hooks on the frame clock's signals record the phase each
  `bind` runs in.
- A row counts as visible if its label is inside the view. GTK's own
  `pick` agreed on every row checked.
- The long list has 5000 rows, 21 px apart, in a 480 px viewport.

## Results

Painted frames, and frames with wrong text, for the deferred variant.
Control had no wrong frames anywhere.

- **Rows added from inside the driver**, five rounds: no wrong frames.
- **A new window's first paint**, built three ways: no wrong frames.
- **`set_value` from ordinary code**, five patterns and 2,595 binds: no
  wrong frames.
- **`set_value` from a tick callback:**
  - Downward: wrong frames only at 2,400 px per frame, 28 of 41 from
    the top and 10 of 23 mid-list.
  - Upward: 2 of 12 and 2 of 60 at 480 px per frame, and 20 of 23 at
    2,400.
  - Slower steps: no wrong frames.
- **Real GTK paths:**
  - Mouse-wheel clicks through XTest: no wrong frames.
  - GTK's own kinetic deceleration: 8 of 10 runs had no wrong frames.
    The other two, both scrolling upward, had 4 of 89 and 2 of 186.
  - The scroll-to-start and scroll-to-end animations: 10 of 26 and 8 of
    29.
- **A GTK quirk:** after the list is first shown, the first programmatic
  `set_value` leaves the view blank for 5 to 9 frames in both variants.
  It lasts until the next scroll change rebinds every row. If that change
  comes from a tick callback, deferred shows one more frame with every
  visible row stale.

The full numbers are in the `results-*.txt` files.
`results-final-a.txt`, `results-final-b.txt` and
`results-final-blocking.txt` are the same run three times; the last one
uses blocking main-loop iterations. `results-3d-*.txt` cover the quirk.

## Not verified

- Real touchpad or touchscreen flings, which XTest can't generate.
- Keyboard-driven scrolling. What's said about it comes from reading
  GTK's source, not from a measurement.
- Wayland, a compositor with frame sync, other GTK versions,
  `GridView`, `ColumnView`, and rows of varying height.
- Pixels on screen. "Shown" means the widgets' state when GTK renders
  the frame, and nobody assessed whether one stale frame is noticeable.

## Running it

It needs GTK 4.14 or later and Xvfb. It builds into `./target` unless
`CARGO_TARGET_DIR` is set.

```sh
./run.sh                            # every scenario, control and deferred
./run.sh 3b-tick                    # only scenarios whose name contains 3b-tick
PROBE_BLOCKING=1 ./run.sh           # blocking main-loop iterations
PROBE_DEBUG=1 ./run.sh 1-           # trace every bind and painted frame
python3 summarize.py results-*.txt  # several runs side by side
```
