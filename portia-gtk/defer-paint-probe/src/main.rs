//! Does GTK 4.14 paint a frame that shows a widget without its text when
//! the text write is deferred to a default-priority `spawn_future_local`
//! driver, instead of made at once?
//!
//! `defer-paint-probe all [filter]` runs every scenario twice, each time in
//! a child process: `control` writes the text the moment it is known, and
//! `deferred` queues the write on an emulated driver, which runs it at its
//! next poll. Run it under `xvfb-run -a`.
//!
//! A painted frame is an emission of `GdkSurface::render` on the window's
//! surface. GDK emits it from the frame clock's paint phase when the mapped
//! surface has something to redraw, and GtkWindow's handler snapshots the
//! widget tree and renders it. An emission hook runs before any handler, so
//! the hook sees exactly the state the frame shows, and checks the text of
//! every visible row or label against what it should show.
//!
//! Emission hooks on the window frame clock's signals record which phase
//! is in progress, so each `bind` is attributed to the phase it ran in;
//! "outside" means not inside the frame clock's dispatch.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::{CStr, c_char, c_int, c_uint, c_ulong, c_void};
use std::future::poll_fn;
use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use std::rc::Rc;
use std::task::{Poll, Waker};
use std::time::{Duration, Instant};

use gtk::glib::translate::IntoGlib;
use gtk::glib::{ffi as glib_ffi, gobject_ffi};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Variant {
    /// The text is written the moment it is known.
    Control,
    /// The write is queued on the driver and runs at its next poll.
    Deferred,
}

impl Variant {
    fn name(self) -> &'static str {
        match self {
            Variant::Control => "control",
            Variant::Deferred => "deferred",
        }
    }
}

// ---------------------------------------------------------------------------
// The emulated driver

/// The emulated Bough driver: one `spawn_future_local` future (default
/// priority) holding a queue of closures. Each poll stores its waker, takes
/// the closures queued at that moment and runs them, and returns pending. A
/// closure queued while they run waits for the next poll, which its wake
/// schedules, so a deferred call always waits for the driver's next pump,
/// even when it is queued from inside a pump.
#[derive(Default)]
struct Driver {
    queue: RefCell<VecDeque<Box<dyn FnOnce()>>>,
    waker: RefCell<Option<Waker>>,
    polls: Cell<u64>,
}

thread_local! {
    static DRIVER: Driver = Driver::default();
}

/// Queues `f` on the driver and wakes it.
fn defer(f: impl FnOnce() + 'static) {
    DRIVER.with(|d| {
        d.queue.borrow_mut().push_back(Box::new(f));
        if let Some(waker) = d.waker.borrow().as_ref() {
            waker.wake_by_ref();
        }
    });
}

fn spawn_driver() {
    glib::spawn_future_local(poll_fn(|cx| {
        let batch: Vec<Box<dyn FnOnce()>> = DRIVER.with(|d| {
            d.polls.set(d.polls.get() + 1);
            *d.waker.borrow_mut() = Some(cx.waker().clone());
            d.queue.borrow_mut().drain(..).collect()
        });
        if phase() != Phase::Outside {
            stats(|s| s.polls_in_clock += 1);
        }
        for f in batch {
            f();
        }
        Poll::<()>::Pending
    }));
}

// ---------------------------------------------------------------------------
// Frame-clock phases and painted frames

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
enum Phase {
    #[default]
    Outside,
    FlushEvents,
    BeforePaint,
    Update,
    Layout,
    Paint,
    AfterPaint,
    ResumeEvents,
}

/// In declaration order, so that `PHASES[phase as usize] == phase`.
const PHASES: [Phase; 8] = [
    Phase::Outside,
    Phase::FlushEvents,
    Phase::BeforePaint,
    Phase::Update,
    Phase::Layout,
    Phase::Paint,
    Phase::AfterPaint,
    Phase::ResumeEvents,
];

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Outside => "outside",
            Phase::FlushEvents => "flush-events",
            Phase::BeforePaint => "before-paint",
            Phase::Update => "update",
            Phase::Layout => "layout",
            Phase::Paint => "paint",
            Phase::AfterPaint => "after-paint",
            Phase::ResumeEvents => "resume-events",
        }
    }
}

#[derive(Default)]
struct Monitor {
    /// The phase of the watched frame clock in progress.
    phase: Cell<Phase>,
    /// The watched window's frame clock and surface, as pointers.
    clock: Cell<usize>,
    surface: Cell<usize>,
    /// Frame-clock cycles (`before-paint` emissions) of the watched clock.
    frames: Cell<u64>,
    /// `render` emissions per surface: frames painted.
    renders: RefCell<HashMap<usize, u64>>,
    /// What to check at each frame a surface paints.
    checks: RefCell<HashMap<usize, Rc<dyn Fn()>>>,
}

thread_local! {
    static MONITOR: Monitor = Monitor::default();
}

fn phase() -> Phase {
    MONITOR.with(|m| m.phase.get())
}

fn frames() -> u64 {
    MONITOR.with(|m| m.frames.get())
}

fn renders_of(surface: usize) -> u64 {
    MONITOR.with(|m| m.renders.borrow().get(&surface).copied().unwrap_or(0))
}

fn watched_renders() -> u64 {
    renders_of(MONITOR.with(|m| m.surface.get()))
}

unsafe extern "C" fn clock_hook(
    _hint: *mut gobject_ffi::GSignalInvocationHint,
    _n_params: c_uint,
    params: *const gobject_ffi::GValue,
    data: glib_ffi::gpointer,
) -> glib_ffi::gboolean {
    let instance = unsafe { gobject_ffi::g_value_get_object(params) } as usize;
    let phase = PHASES[data as usize];
    MONITOR.with(|m| {
        if m.clock.get() == instance {
            if phase == Phase::BeforePaint {
                m.frames.set(m.frames.get() + 1);
            }
            m.phase.set(phase);
        }
    });
    glib_ffi::GTRUE
}

unsafe extern "C" fn render_hook(
    _hint: *mut gobject_ffi::GSignalInvocationHint,
    _n_params: c_uint,
    params: *const gobject_ffi::GValue,
    _data: glib_ffi::gpointer,
) -> glib_ffi::gboolean {
    let surface = unsafe { gobject_ffi::g_value_get_object(params) } as usize;
    let check = MONITOR.with(|m| {
        *m.renders.borrow_mut().entry(surface).or_default() += 1;
        m.checks.borrow().get(&surface).cloned()
    });
    if let Some(check) = check {
        check();
    }
    glib_ffi::GTRUE
}

/// Emission hooks run before every handler of a signal, on every instance.
fn install_hooks() {
    unsafe {
        let clock_type = gdk::FrameClock::static_type().into_glib();
        gobject_ffi::g_type_class_ref(clock_type);
        for (name, phase) in [
            (c"flush-events", Phase::FlushEvents),
            (c"before-paint", Phase::BeforePaint),
            (c"update", Phase::Update),
            (c"layout", Phase::Layout),
            (c"paint", Phase::Paint),
            (c"after-paint", Phase::AfterPaint),
            (c"resume-events", Phase::ResumeEvents),
        ] {
            let id = gobject_ffi::g_signal_lookup(name.as_ptr(), clock_type);
            assert_ne!(id, 0, "no frame-clock signal {name:?}");
            gobject_ffi::g_signal_add_emission_hook(
                id,
                0,
                Some(clock_hook),
                phase as usize as glib_ffi::gpointer,
                None,
            );
        }
        let surface_type = gdk::Surface::static_type().into_glib();
        gobject_ffi::g_type_class_ref(surface_type);
        let id = gobject_ffi::g_signal_lookup(c"render".as_ptr(), surface_type);
        assert_ne!(id, 0, "no GdkSurface::render");
        gobject_ffi::g_signal_add_emission_hook(
            id,
            0,
            Some(render_hook),
            std::ptr::null_mut(),
            None,
        );
    }
}

fn surface_ptr(window: &gtk::Window) -> usize {
    window
        .surface()
        .expect("a realized window has a surface")
        .as_ptr() as usize
}

/// Runs `check` at every frame `surface` paints.
fn on_render(surface: usize, check: Rc<dyn Fn()>) {
    MONITOR.with(|m| m.checks.borrow_mut().insert(surface, check));
}

/// Makes `window` the watched one: its frame clock's phases are tracked and
/// `check` runs at every frame it paints.
fn watch(window: &gtk::Window, check: Rc<dyn Fn()>) {
    let clock = window
        .frame_clock()
        .expect("a realized window has a frame clock");
    let surface = surface_ptr(window);
    MONITOR.with(|m| {
        m.clock.set(clock.as_ptr() as usize);
        m.surface.set(surface);
    });
    on_render(surface, check);
    // These handlers run after the hooks, and after GDK's and GTK's own
    // handlers, which were connected when the surface was made. Nothing
    // binds after them within a phase, so they mark the end of it.
    let outside = |_: &gdk::FrameClock| MONITOR.with(|m| m.phase.set(Phase::Outside));
    clock.connect_after_paint(outside);
    clock.connect_flush_events(outside);
    clock.connect_resume_events(outside);
}

// ---------------------------------------------------------------------------
// What each measured stretch saw

#[derive(Default)]
struct Stats {
    measuring: bool,
    frames_at_start: u64,
    /// Frames painted.
    renders: u64,
    /// Frames painted with at least one visible row or label wrong.
    wrong_renders: u64,
    /// Visible rows or labels checked, summed over painted frames.
    checked: u64,
    /// Wrong ones, summed over painted frames: no text, another item's
    /// text, or a row with no item.
    empty: u64,
    stale: u64,
    unbound: u64,
    /// `bind` calls by the frame-clock phase they ran in.
    binds: BTreeMap<Phase, u64>,
    /// Of those, binds of a row whose position is inside the viewport at the
    /// moment of binding (rows have a uniform height, so the adjustment's
    /// value and the row pitch place them).
    binds_in_view: BTreeMap<Phase, u64>,
    /// Deferred writes that ran, keyed by the phase their bind (or the
    /// label's creation) ran in and the number of frames the window painted
    /// between that moment and the write.
    landed: BTreeMap<(Phase, u64), u64>,
    /// Deferred writes skipped because the row was unbound or rebound first.
    dropped: u64,
    /// Driver polls that ran inside the frame clock's dispatch.
    polls_in_clock: u64,
    /// Labels GTK's `pick` found at sample points of the view, and how many
    /// of them the visibility test did not count (should be 0).
    picks: u64,
    pick_misses: u64,
    /// Of the wrong labels: the most pixels of one inside the view, and how
    /// many were wholly inside it.
    wrong_px_max: f32,
    wrong_whole: u64,
    /// Wrong labels that GTK's `pick` finds at their place in the view.
    wrong_picked: u64,
    /// Painted frames in which no row was visible at all.
    blank_renders: u64,
    last_value: Option<f64>,
    /// Largest scroll between two painted frames, in pixels.
    max_step: f64,
    examples: Vec<String>,
}

thread_local! {
    static STATS: RefCell<Stats> = RefCell::default();
}

fn stats<R>(f: impl FnOnce(&mut Stats) -> R) -> R {
    STATS.with(|s| f(&mut s.borrow_mut()))
}

fn begin() {
    let frames = frames();
    stats(|s| {
        *s = Stats {
            measuring: true,
            frames_at_start: frames,
            ..Stats::default()
        }
    });
}

thread_local! {
    /// The list's vertical adjustment and store, to place a bound row.
    static GEOMETRY: RefCell<Option<(gtk::Adjustment, gio::ListStore)>> = const { RefCell::new(None) };
}

/// Whether the row at `position` is inside the viewport right now.
fn in_view(position: u32) -> bool {
    GEOMETRY.with(|g| {
        let g = g.borrow();
        let Some((adj, store)) = g.as_ref() else {
            return false;
        };
        let n = store.n_items();
        if n == 0 || adj.upper() <= 0.0 {
            return false;
        }
        let pitch = adj.upper() / n as f64;
        let top = position as f64 * pitch;
        top + pitch > adj.value() && top < adj.value() + adj.page_size()
    })
}

fn record_bind(position: u32) -> Phase {
    let phase = phase();
    let visible = in_view(position);
    stats(|s| {
        if s.measuring {
            *s.binds.entry(phase).or_default() += 1;
            if visible {
                *s.binds_in_view.entry(phase).or_default() += 1;
            }
        }
    });
    phase
}

fn record_landing(phase: Phase, paints_between: u64) {
    stats(|s| {
        if s.measuring {
            *s.landed.entry((phase, paints_between)).or_default() += 1;
        }
    });
}

fn record_frame(checked: u64, empty: u64, stale: u64, unbound: u64, example: Option<String>) {
    stats(|s| {
        s.renders += 1;
        if checked == 0 {
            s.blank_renders += 1;
        }
        s.checked += checked;
        s.empty += empty;
        s.stale += stale;
        s.unbound += unbound;
        if empty + stale + unbound > 0 {
            s.wrong_renders += 1;
            if let Some(example) = example {
                if s.examples.len() < 3 {
                    s.examples.push(example);
                }
            }
        }
    });
}

fn report(scenario: &str, round: &str, variant: Variant, extra: &str) {
    let frames = frames();
    stats(|s| {
        s.measuring = false;
        let binds: Vec<String> = s
            .binds
            .iter()
            .map(|(p, n)| format!("{}:{n}", p.name()))
            .collect();
        let binds_in_view: Vec<String> = s
            .binds_in_view
            .iter()
            .map(|(p, n)| format!("{}:{n}", p.name()))
            .collect();
        let landed: Vec<String> = s
            .landed
            .iter()
            .map(|((p, k), n)| format!("{}+{k}:{n}", p.name()))
            .collect();
        println!(
            "RESULT scenario={scenario} round={} variant={} frames={} renders={} wrong_renders={} \
             checked={} empty={} stale={} unbound={} binds={} binds_in_view={} landed={} dropped={} \
             polls_in_clock={} picks={} pick_misses={} wrong_px_max={:.0} wrong_whole={} \
             wrong_picked={} blank_renders={} max_step={:.0} {extra}",
            round.replace(' ', "_"),
            variant.name(),
            frames - s.frames_at_start,
            s.renders,
            s.wrong_renders,
            s.checked,
            s.empty,
            s.stale,
            s.unbound,
            if binds.is_empty() {
                "-".into()
            } else {
                binds.join(",")
            },
            if binds_in_view.is_empty() {
                "-".into()
            } else {
                binds_in_view.join(",")
            },
            if landed.is_empty() {
                "-".into()
            } else {
                landed.join(",")
            },
            s.dropped,
            s.polls_in_clock,
            s.picks,
            s.pick_misses,
            s.wrong_px_max,
            s.wrong_whole,
            s.wrong_picked,
            s.blank_renders,
            s.max_step,
        );
        for example in &s.examples {
            println!(
                "EXAMPLE {scenario} / {round} / {}: {example}",
                variant.name()
            );
        }
    });
}

// ---------------------------------------------------------------------------
// Driving the main loop

/// `PROBE_BLOCKING=1` drives the main loop with blocking iterations, as
/// `glib::MainLoop::run` does, instead of non-blocking iterations and
/// sleeps: a cross-check that the harness's way of iterating does not
/// change the order in which sources run.
fn blocking() -> bool {
    std::env::var_os("PROBE_BLOCKING").is_some()
}

/// Blocking iterations until `done`, or until `limit` has passed and at
/// least `min_iterations` have run (a slow dispatch, such as a window's
/// first realize, must not use up the budget). A low-priority heartbeat
/// bounds each wait so that the conditions get checked; it only returns.
fn iterate_blocking(done: &dyn Fn() -> bool, limit: Duration, min_iterations: u32) -> bool {
    let ctx = glib::MainContext::default();
    let start = Instant::now();
    let beat = glib::timeout_add_local_full(Duration::from_millis(4), glib::Priority::LOW, || {
        glib::ControlFlow::Continue
    });
    let mut finished = true;
    let mut iterations = 0;
    while !done() {
        if start.elapsed() > limit && iterations >= min_iterations {
            finished = false;
            break;
        }
        ctx.iteration(true);
        iterations += 1;
    }
    beat.remove();
    finished
}

/// Runs the main loop until it is quiet, as the existing scenarios do.
fn settle() {
    if blocking() {
        iterate_blocking(&|| false, Duration::from_millis(100), 40);
        return;
    }
    let ctx = glib::MainContext::default();
    for _ in 0..40 {
        while ctx.iteration(false) {}
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Runs the main loop until `done`, or for at most `limit`.
fn run_until(done: impl Fn() -> bool, limit: Duration) -> bool {
    if blocking() {
        return iterate_blocking(&done, limit, 0);
    }
    let ctx = glib::MainContext::default();
    let start = Instant::now();
    while !done() {
        if start.elapsed() > limit {
            return false;
        }
        while ctx.iteration(false) {}
        std::thread::sleep(Duration::from_millis(1));
    }
    true
}

fn show(child: &impl IsA<gtk::Widget>, width: i32, height: i32) -> gtk::Window {
    let window = gtk::Window::new();
    window.set_default_size(width, height);
    window.set_child(Some(child));
    window.present();
    settle();
    window
}

fn note_window(window: &gtk::Window) {
    let renderer = window.renderer().map(|r| r.type_().name().to_string());
    let surface = window.surface().expect("a surface");
    println!(
        "NOTE window {}x{}, mapped {}, surface mapped {}, renderer {:?}, frames painted so far {}, \
         gtk-enable-animations {:?}",
        window.width(),
        window.height(),
        window.is_mapped(),
        surface.is_mapped(),
        renderer,
        renders_of(surface.as_ptr() as usize),
        gtk::Settings::default().map(|s| s.is_gtk_enable_animations()),
    );
}

// ---------------------------------------------------------------------------
// A list view whose rows show "item N"

fn debug() -> bool {
    std::env::var_os("PROBE_DEBUG").is_some()
}

fn text_for(n: u32) -> String {
    format!("item {n}")
}

struct ListParts {
    store: gio::ListStore,
    view: gtk::ListView,
    scroller: gtk::ScrolledWindow,
    /// Each row's label, and the list item it is the child of.
    rows: Rc<RefCell<HashMap<gtk::Label, gtk::ListItem>>>,
}

/// A list view over `n_items` items. `bind` writes the row's label text at
/// once (control) or defers the write to the driver (deferred). `unbind`
/// makes a still-queued write for the old item a no-op, as dropping a
/// row's listener would.
fn list_parts(variant: Variant, n_items: u32) -> Rc<ListParts> {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let items: Vec<glib::BoxedAnyObject> = (0..n_items).map(glib::BoxedAnyObject::new).collect();
    store.splice(0, 0, &items);
    let rows: Rc<RefCell<HashMap<gtk::Label, gtk::ListItem>>> = Rc::default();
    let generations: Rc<RefCell<HashMap<gtk::ListItem, u64>>> = Rc::default();
    let factory = gtk::SignalListItemFactory::new();
    {
        let rows = rows.clone();
        factory.connect_setup(move |_, obj| {
            let item = obj.downcast_ref::<gtk::ListItem>().expect("a list item");
            let label = gtk::Label::new(None);
            label.set_xalign(0.0);
            item.set_child(Some(&label));
            rows.borrow_mut().insert(label, item.clone());
        });
    }
    {
        let generations = generations.clone();
        factory.connect_bind(move |_, obj| {
            let item = obj
                .downcast_ref::<gtk::ListItem>()
                .expect("a list item")
                .clone();
            let n = *item
                .item()
                .and_downcast::<glib::BoxedAnyObject>()
                .expect("a boxed item")
                .borrow::<u32>();
            let label = item
                .child()
                .and_downcast::<gtk::Label>()
                .expect("the row's label");
            let bind_phase = record_bind(item.position());
            if debug() {
                // Only the list item and its label: a row made for this
                // bind is not parented yet, and taking a reference to it
                // from Rust would sink its floating reference.
                println!(
                    "DEBUG frame {} bind in {} item {n} position {} label shows {:?} measuring {}",
                    frames(),
                    bind_phase.name(),
                    item.position(),
                    label.text().as_str(),
                    stats(|s| s.measuring),
                );
            }
            let generation = {
                let mut generations = generations.borrow_mut();
                let generation = generations.entry(item.clone()).or_default();
                *generation += 1;
                *generation
            };
            match variant {
                Variant::Control => label.set_text(&text_for(n)),
                Variant::Deferred => {
                    let at_bind = watched_renders();
                    let generations = generations.clone();
                    defer(move || {
                        if generations.borrow().get(&item) != Some(&generation) {
                            stats(|s| s.dropped += 1);
                            return;
                        }
                        label.set_text(&text_for(n));
                        record_landing(bind_phase, watched_renders() - at_bind);
                    });
                }
            }
        });
    }
    factory.connect_unbind(move |_, obj| {
        let item = obj.downcast_ref::<gtk::ListItem>().expect("a list item");
        *generations.borrow_mut().entry(item.clone()).or_default() += 1;
    });
    let view = gtk::ListView::new(
        Some(gtk::NoSelection::new(Some(store.clone()))),
        Some(factory),
    );
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_child(Some(&view));
    GEOMETRY.with(|g| *g.borrow_mut() = Some((scroller.vadjustment(), store.clone())));
    Rc::new(ListParts {
        store,
        view,
        scroller,
        rows,
    })
}

/// At a painted frame: every row that intersects the view must show its
/// item's text.
fn check_rows(list: &ListParts) {
    if !stats(|s| s.measuring) {
        return;
    }
    let value = list.scroller.vadjustment().value();
    stats(|s| {
        if let Some(last) = s.last_value {
            s.max_step = s.max_step.max((value - last).abs());
        }
        s.last_value = Some(value);
    });
    let height = list.view.height() as f32;
    let width = list.view.width() as f32;
    let rows = list.rows.borrow();
    let (mut checked, mut empty, mut stale, mut unbound) = (0, 0, 0, 0);
    let mut example = None;
    let mut visible: Vec<gtk::Label> = Vec::new();
    let mut child = list.view.first_child();
    while let Some(row) = child {
        child = row.next_sibling();
        if !row.is_drawable() {
            continue;
        }
        let Some(label) = row.first_child().and_downcast::<gtk::Label>() else {
            continue;
        };
        // A row outside the viewport is allocated its tile's zero-size
        // area, so its label draws nothing: only a label with a non-empty
        // box inside the view counts as visible.
        let Some(bounds) = label.compute_bounds(&list.view) else {
            continue;
        };
        if bounds.width() <= 0.0
            || bounds.height() <= 0.0
            || bounds.y() + bounds.height() <= 0.0
            || bounds.y() >= height
            || bounds.x() + bounds.width() <= 0.0
            || bounds.x() >= width
        {
            continue;
        }
        let Some(item) = rows.get(&label) else {
            continue;
        };
        visible.push(label.clone());
        checked += 1;
        let shown = label.text();
        let want = item
            .item()
            .and_downcast::<glib::BoxedAnyObject>()
            .map(|b| text_for(*b.borrow::<u32>()));
        let wrong = match &want {
            None => {
                unbound += 1;
                true
            }
            Some(want) if shown.as_str() == want => false,
            Some(_) if shown.is_empty() => {
                empty += 1;
                true
            }
            Some(_) => {
                stale += 1;
                true
            }
        };
        if wrong {
            let top = bounds.y().max(0.0);
            let inside = (bounds.y() + bounds.height()).min(height) - top;
            let whole = inside >= bounds.height() - 0.5;
            // Reverse check: GTK's hit-testing at the middle of the part
            // inside the view must find this label.
            let hit = list
                .view
                .pick(
                    width as f64 / 2.0,
                    (top + inside / 2.0) as f64,
                    gtk::PickFlags::DEFAULT,
                )
                .is_some_and(|w| w == *label.upcast_ref::<gtk::Widget>());
            stats(|s| {
                s.wrong_px_max = s.wrong_px_max.max(inside);
                if whole {
                    s.wrong_whole += 1;
                }
                if hit {
                    s.wrong_picked += 1;
                }
            });
        }
        if wrong && example.is_none() {
            example = Some(format!(
                "frame {}: label at y={:.0} of {:.0} shows {:?}, its row's item is {:?} (scroll {:.0})",
                frames(),
                bounds.y(),
                height,
                shown.as_str(),
                want,
                value
            ));
        }
    }
    // Cross-check against GTK's own hit-testing: every label `pick` finds
    // in the view must be one of those counted as visible.
    let (mut picks, mut pick_misses) = (0, 0);
    let mut y = 1.0;
    while y < height as f64 {
        if let Some(hit) = list
            .view
            .pick(width as f64 / 2.0, y, gtk::PickFlags::DEFAULT)
        {
            if let Ok(label) = hit.downcast::<gtk::Label>() {
                picks += 1;
                if !visible.contains(&label) {
                    pick_misses += 1;
                }
            }
        }
        y += 7.0;
    }
    stats(|s| {
        s.picks += picks;
        s.pick_misses += pick_misses;
    });
    if debug() {
        println!(
            "DEBUG frame {} render (measuring): scroll {value:.0}, visible rows {checked}, wrong {} ({:?})",
            frames(),
            empty + stale + unbound,
            example
        );
    }
    record_frame(checked, empty, stale, unbound, example);
}

fn show_list(list: &Rc<ListParts>) -> gtk::Window {
    let window = show(&list.scroller, 320, 480);
    let parts = list.clone();
    watch(&window, Rc::new(move || check_rows(&parts)));
    settle();
    note_window(&window);
    let adj = list.scroller.vadjustment();
    println!(
        "NOTE list: {} items, row height {:?}, view height {}, adjustment upper {:.0}, page {:.0}, rows made {}",
        list.store.n_items(),
        list.view.first_child().map(|r| r.height()),
        list.view.height(),
        adj.upper(),
        adj.page_size(),
        list.rows.borrow().len(),
    );
    window
}

// ---------------------------------------------------------------------------
// Case 1: rows added from inside the driver

fn case1(variant: Variant) {
    const NAME: &str = "1-rows-added-in-driver";
    let list = list_parts(variant, 0);
    let _window = show_list(&list);
    let next = Rc::new(Cell::new(1000u32));
    let fresh = move |count: u32| -> Vec<glib::BoxedAnyObject> {
        (0..count)
            .map(|_| {
                let n = next.get();
                next.set(n + 1);
                glib::BoxedAnyObject::new(n)
            })
            .collect()
    };
    let fresh: Rc<dyn Fn(u32) -> Vec<glib::BoxedAnyObject>> = Rc::new(fresh);
    type Change = Box<dyn Fn(&gio::ListStore, &dyn Fn(u32) -> Vec<glib::BoxedAnyObject>)>;
    let rounds: Vec<(&str, Change)> = vec![
        (
            "append 3 to the empty list",
            Box::new(|store, fresh| store.extend_from_slice(&fresh(3))),
        ),
        (
            "append 40, one at a time",
            Box::new(|store, fresh| {
                for item in fresh(40) {
                    store.append(&item);
                }
            }),
        ),
        (
            "replace the first 20 (splice)",
            Box::new(|store, fresh| store.splice(0, 20, &fresh(20))),
        ),
        (
            "insert 1 at the top",
            Box::new(|store, fresh| store.insert(0, &fresh(1)[0])),
        ),
        (
            "remove all, append 30",
            Box::new(|store, fresh| {
                store.remove_all();
                store.extend_from_slice(&fresh(30));
            }),
        ),
    ];
    for (round, change) in rounds {
        begin();
        let store = list.store.clone();
        let fresh = fresh.clone();
        // The model changes inside a driver pump, as a listener's would.
        defer(move || change(&store, &*fresh));
        settle();
        report(
            NAME,
            round,
            variant,
            &format!("items={}", list.store.n_items()),
        );
    }
}

// ---------------------------------------------------------------------------
// Case 2: a new window's first paint, and labels added to a shown window

const TEXTS: [&str; 6] = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta"];

type Labels = Vec<(gtk::Label, &'static str)>;

/// Labels whose text is known when they are made. `surface` names the
/// surface whose painted frames a deferred write counts.
fn labels_with_text(variant: Variant, surface: Rc<Cell<usize>>) -> Labels {
    let made_in = phase();
    TEXTS
        .iter()
        .map(|&text| {
            let label = gtk::Label::new(None);
            match variant {
                Variant::Control => label.set_text(text),
                Variant::Deferred => {
                    let at_creation = renders_of(surface.get());
                    let label = label.clone();
                    let surface = surface.clone();
                    defer(move || {
                        label.set_text(text);
                        record_landing(made_in, renders_of(surface.get()) - at_creation);
                    });
                }
            }
            (label, text)
        })
        .collect()
}

/// At a painted frame: every label must show its text.
fn check_labels(labels: &Labels) {
    if !stats(|s| s.measuring) {
        return;
    }
    let (mut checked, mut empty, mut stale) = (0, 0, 0);
    let mut example = None;
    for (label, want) in labels {
        if !label.is_drawable() {
            continue;
        }
        checked += 1;
        let shown = label.text();
        if shown.as_str() != *want {
            if shown.is_empty() {
                empty += 1;
            } else {
                stale += 1;
            }
            example.get_or_insert_with(|| {
                format!("label shows {:?}, should show {want:?}", shown.as_str())
            });
        }
    }
    record_frame(checked, empty, stale, 0, example);
}

/// A window holding `labels`; when it is realized, its painted frames are
/// checked and `surface` is set to its surface.
fn window_with_labels(labels: Rc<Labels>, surface: Rc<Cell<usize>>) -> gtk::Window {
    let window = gtk::Window::new();
    window.set_default_size(320, 240);
    let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
    for (label, _) in labels.iter() {
        column.append(label);
    }
    window.set_child(Some(&column));
    // `realize` runs its class handler first, so the surface exists here,
    // and no frame can have painted yet.
    window.connect_realize(move |window| {
        let ptr = surface_ptr(window);
        surface.set(ptr);
        let labels = labels.clone();
        on_render(ptr, Rc::new(move || check_labels(&labels)));
    });
    window
}

fn describe_new_window(window: &gtk::Window, surface: usize) -> String {
    format!(
        "window_mapped={} surface_mapped={} window_frames_painted={}",
        window.is_mapped(),
        window.surface().is_some_and(|s| s.is_mapped()),
        renders_of(surface)
    )
}

fn case2a(variant: Variant) {
    const NAME: &str = "2a-new-window-built-in-ordinary-code";
    begin();
    let surface = Rc::new(Cell::new(0));
    let labels = Rc::new(labels_with_text(variant, surface.clone()));
    let window = window_with_labels(labels, surface.clone());
    window.present();
    settle();
    report(
        NAME,
        "labels made, window presented",
        variant,
        &describe_new_window(&window, surface.get()),
    );
}

fn case2b(variant: Variant) {
    const NAME: &str = "2b-new-window-built-in-driver";
    begin();
    let surface = Rc::new(Cell::new(0));
    let window: Rc<RefCell<Option<gtk::Window>>> = Rc::default();
    {
        let surface = surface.clone();
        let window = window.clone();
        // Built and presented inside a pump, as a listener would; the
        // deferred texts wait for the next pump.
        defer(move || {
            let labels = Rc::new(labels_with_text(variant, surface.clone()));
            let made = window_with_labels(labels, surface);
            made.present();
            *window.borrow_mut() = Some(made);
        });
    }
    settle();
    let window = window
        .borrow()
        .clone()
        .expect("the driver built the window");
    report(
        NAME,
        "window built and presented in a pump",
        variant,
        &describe_new_window(&window, surface.get()),
    );
}

fn case2c(variant: Variant) {
    const NAME: &str = "2c-labels-added-to-shown-window-in-driver";
    let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
    column.append(&gtk::Label::new(Some("already shown")));
    let window = show(&column, 320, 240);
    let labels: Rc<RefCell<Labels>> = Rc::default();
    {
        let labels = labels.clone();
        watch(&window, Rc::new(move || check_labels(&labels.borrow())));
    }
    settle();
    note_window(&window);
    begin();
    let surface = Rc::new(Cell::new(surface_ptr(&window)));
    {
        let labels = labels.clone();
        defer(move || {
            let made = labels_with_text(variant, surface);
            for (label, _) in &made {
                column.append(label);
            }
            labels.borrow_mut().extend(made);
        });
    }
    settle();
    report(NAME, "6 labels added in a pump", variant, "");
}

// ---------------------------------------------------------------------------
// Case 3: scrolling a long list

const LONG_LIST: u32 = 5000;

#[derive(Clone, Copy)]
enum Start {
    Top,
    End,
    /// Pixels from the top.
    FromTop(f64),
    /// Pixels from the end.
    FromEnd(f64),
    /// A fraction of the scrollable range.
    Fraction(f64),
}

fn scroll_setup(variant: Variant, start: Start) -> (Rc<ListParts>, gtk::Window) {
    let list = list_parts(variant, LONG_LIST);
    let window = show_list(&list);
    let adj = list.scroller.vadjustment();
    let max = adj.upper() - adj.page_size();
    let value = match start {
        Start::Top => 0.0,
        Start::End => max,
        Start::FromTop(px) => px,
        Start::FromEnd(px) => max - px,
        Start::Fraction(f) => (max * f).round(),
    };
    if debug() {
        println!(
            "DEBUG frame {} setup: set_value({value}) from ordinary code",
            frames()
        );
    }
    adj.set_value(value);
    if debug() {
        println!(
            "DEBUG frame {} setup: set_value returned, value {}",
            frames(),
            adj.value()
        );
    }
    settle();
    // GTK 4.14: the first set_value after the list is shown moves the
    // adjustment but not the list's anchor, so the view stays blank or
    // stale until the next value change (see the debug-set-value
    // scenario). Two more changes from ordinary code bring the list in line
    // before anything is measured.
    adj.set_value(if value >= 1.0 {
        value - 1.0
    } else {
        value + 1.0
    });
    settle();
    adj.set_value(value);
    settle();
    settle();
    let pitch = adj.upper() / list.store.n_items() as f64;
    let top = list
        .view
        .pick(100.0, 10.0, gtk::PickFlags::DEFAULT)
        .and_then(|w| w.downcast::<gtk::Label>().ok())
        .map(|l| l.text().to_string());
    println!(
        "NOTE setup at scroll {:.0}: label at the top of the view {:?}, expected about \"item {}\"",
        adj.value(),
        top,
        (adj.value() / pitch).floor()
    );
    (list, window)
}

fn position(adj: &gtk::Adjustment, from: f64) -> String {
    format!(
        "from={from:.0} to={:.0} max={:.0}",
        adj.value(),
        adj.upper() - adj.page_size()
    )
}

/// (a) The adjustment is set from ordinary code between main-loop
/// iterations, `steps` times, `step` pixels each.
fn set_value_steps(name: &str, variant: Variant, start: Start, step: f64, steps: u32) {
    let (list, _window) = scroll_setup(variant, start);
    let adj = list.scroller.vadjustment();
    let from = adj.value();
    begin();
    for _ in 0..steps {
        adj.set_value(adj.value() + step);
        settle();
    }
    report(
        name,
        &format!("{steps} steps of {step} px"),
        variant,
        &position(&adj, from),
    );
}

/// (a) Jumps to the given positions (negative: from the end), from ordinary
/// code, settling after each.
fn set_value_jumps(name: &str, variant: Variant, targets: &[f64]) {
    let (list, _window) = scroll_setup(variant, Start::Top);
    let adj = list.scroller.vadjustment();
    let from = adj.value();
    begin();
    for &target in targets {
        let max = adj.upper() - adj.page_size();
        adj.set_value(if target < 0.0 { max + target } else { target });
        settle();
    }
    report(
        name,
        &format!("{} jumps", targets.len()),
        variant,
        &position(&adj, from),
    );
}

/// (b) The adjustment is set from a tick callback, inside the frame clock's
/// update phase, `speed` pixels per frame, for at most `max_frames` frames
/// or until it reaches an end, as kinetic scrolling does.
fn tick_scroll(name: &str, variant: Variant, start: Start, speed: f64, max_frames: u32) {
    let (list, _window) = scroll_setup(variant, start);
    let adj = list.scroller.vadjustment();
    let from = adj.value();
    let done = Rc::new(Cell::new(false));
    let ticks = Rc::new(Cell::new(0u32));
    let tick_phases: Rc<RefCell<BTreeMap<Phase, u32>>> = Rc::default();
    begin();
    {
        let (done, ticks, adj) = (done.clone(), ticks.clone(), adj.clone());
        let tick_phases = tick_phases.clone();
        list.scroller.add_tick_callback(move |_, _| {
            *tick_phases.borrow_mut().entry(phase()).or_default() += 1;
            let lower = adj.lower();
            let upper = adj.upper() - adj.page_size();
            let value = (adj.value() + speed).clamp(lower, upper);
            adj.set_value(value);
            ticks.set(ticks.get() + 1);
            if ticks.get() >= max_frames || value <= lower || value >= upper {
                done.set(true);
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }
    let finished = run_until(|| done.get(), Duration::from_secs(30));
    settle();
    report(
        name,
        &format!("tick {speed} px per frame"),
        variant,
        &format!(
            "{} ticks={} finished={finished} tick_callback_phases={}",
            position(&adj, from),
            ticks.get(),
            tick_phases
                .borrow()
                .iter()
                .map(|(p, n)| format!("{}:{n}", p.name()))
                .collect::<Vec<_>>()
                .join(",")
        ),
    );
}

/// Real X input through XTest, on a second connection to the X server.
struct XTest {
    display: *mut c_void,
    fake_button: unsafe extern "C" fn(*mut c_void, c_uint, c_int, c_ulong) -> c_int,
    fake_motion: unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int, c_ulong) -> c_int,
    flush: unsafe extern "C" fn(*mut c_void) -> c_int,
}

fn symbol(lib: *mut c_void, name: &CStr) -> Result<*mut c_void, String> {
    let ptr = unsafe { libc::dlsym(lib, name.as_ptr()) };
    if ptr.is_null() {
        Err(format!("no symbol {name:?}"))
    } else {
        Ok(ptr)
    }
}

impl XTest {
    fn open() -> Result<XTest, String> {
        unsafe {
            let x11 = libc::dlopen(c"libX11.so.6".as_ptr(), libc::RTLD_NOW);
            let xtst = libc::dlopen(c"libXtst.so.6".as_ptr(), libc::RTLD_NOW);
            if x11.is_null() || xtst.is_null() {
                return Err("cannot dlopen libX11.so.6 or libXtst.so.6".into());
            }
            type Open = unsafe extern "C" fn(*const c_char) -> *mut c_void;
            type Query = unsafe extern "C" fn(
                *mut c_void,
                *mut c_int,
                *mut c_int,
                *mut c_int,
                *mut c_int,
            ) -> c_int;
            let open: Open = std::mem::transmute(symbol(x11, c"XOpenDisplay")?);
            let query: Query = std::mem::transmute(symbol(xtst, c"XTestQueryExtension")?);
            let display = open(std::ptr::null());
            if display.is_null() {
                return Err("XOpenDisplay failed".into());
            }
            let (mut a, mut b, mut c, mut d) = (0, 0, 0, 0);
            if query(display, &mut a, &mut b, &mut c, &mut d) == 0 {
                return Err("the X server has no XTEST".into());
            }
            Ok(XTest {
                display,
                fake_button: std::mem::transmute(symbol(xtst, c"XTestFakeButtonEvent")?),
                fake_motion: std::mem::transmute(symbol(xtst, c"XTestFakeMotionEvent")?),
                flush: std::mem::transmute(symbol(x11, c"XFlush")?),
            })
        }
    }

    fn move_to(&self, x: i32, y: i32) {
        unsafe {
            (self.fake_motion)(self.display, -1, x, y, 0);
            (self.flush)(self.display);
        }
    }

    /// Button 4 is the wheel up, 5 down.
    fn clicks(&self, button: u32, count: u32) {
        unsafe {
            for _ in 0..count {
                (self.fake_button)(self.display, button, 1, 0);
                (self.fake_button)(self.display, button, 0, 0);
            }
            (self.flush)(self.display);
        }
    }
}

/// (c) Real wheel events: XTest button 4/5 clicks over the list, in
/// `batches` batches of `per_batch`, settling after each batch.
fn wheel(name: &str, variant: Variant, start: Start, button: u32, per_batch: u32, batches: u32) {
    let (list, _window) = scroll_setup(variant, start);
    let xtest = match XTest::open() {
        Ok(xtest) => xtest,
        Err(error) => {
            println!("NOTE {name}: skipped, {error}");
            return;
        }
    };
    let pointer: Rc<Cell<Option<(f64, f64)>>> = Rc::default();
    let motion = gtk::EventControllerMotion::new();
    {
        let pointer = pointer.clone();
        motion.connect_motion(move |_, x, y| pointer.set(Some((x, y))));
    }
    list.view.add_controller(motion);
    let scrolls = Rc::new(Cell::new(0u32));
    let units: Rc<RefCell<BTreeMap<String, u32>>> = Rc::default();
    let observer = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    {
        let (scrolls, units) = (scrolls.clone(), units.clone());
        observer.connect_scroll(move |controller, _, _| {
            scrolls.set(scrolls.get() + 1);
            *units
                .borrow_mut()
                .entry(format!("{:?}", controller.unit()))
                .or_default() += 1;
            glib::Propagation::Proceed
        });
    }
    observer.set_propagation_phase(gtk::PropagationPhase::Capture);
    list.view.add_controller(observer);
    xtest.move_to(150, 240);
    settle();
    xtest.move_to(160, 250);
    settle();
    let adj = list.scroller.vadjustment();
    let from = adj.value();
    begin();
    for _ in 0..batches {
        xtest.clicks(button, per_batch);
        settle();
    }
    report(
        name,
        &format!("{batches} batches of {per_batch} wheel clicks"),
        variant,
        &format!(
            "{} pointer_in_view={:?} scroll_events_seen={} units={:?}",
            position(&adj, from),
            pointer.get().map(|(x, y)| (x.round(), y.round())),
            scrolls.get(),
            units.borrow()
        ),
    );
}

#[derive(Clone, Copy)]
enum Next {
    /// A tick callback moves the adjustment 96 px a frame for 10 frames.
    Tick,
    /// One real wheel click through XTest.
    Wheel,
    /// A set_value of +96 px from ordinary code.
    SetValue,
}

/// GTK 4.14 quirk (see debug-set-value): after the list is first shown, the
/// first programmatic set_value moves the adjustment but not the list, whose
/// view stays blank until the next value change, and that change binds every
/// row. Measured from the first set_value, with the next change made by
/// `next`.
fn first_jump_then(name: &str, variant: Variant, next: Next) {
    let list = list_parts(variant, LONG_LIST);
    let _window = show_list(&list);
    let adj = list.scroller.vadjustment();
    let target = ((adj.upper() - adj.page_size()) * 0.5).round();
    let xtest = match next {
        Next::Wheel => match XTest::open() {
            Ok(xtest) => {
                xtest.move_to(160, 250);
                settle();
                Some(xtest)
            }
            Err(error) => {
                println!("NOTE {name}: skipped, {error}");
                return;
            }
        },
        _ => None,
    };
    begin();
    adj.set_value(target);
    settle();
    let blank_before = stats(|s| s.blank_renders);
    match next {
        Next::Tick => {
            let done = Rc::new(Cell::new(false));
            let ticks = Rc::new(Cell::new(0u32));
            {
                let (done, ticks, adj) = (done.clone(), ticks.clone(), adj.clone());
                list.scroller.add_tick_callback(move |_, _| {
                    adj.set_value(adj.value() + 96.0);
                    ticks.set(ticks.get() + 1);
                    if ticks.get() >= 10 {
                        done.set(true);
                        glib::ControlFlow::Break
                    } else {
                        glib::ControlFlow::Continue
                    }
                });
            }
            run_until(|| done.get(), Duration::from_secs(10));
        }
        Next::Wheel => xtest.as_ref().expect("xtest").clicks(5, 1),
        Next::SetValue => adj.set_value(adj.value() + 96.0),
    }
    settle();
    report(
        name,
        match next {
            Next::Tick => "first set_value, then a tick scroll",
            Next::Wheel => "first set_value, then a wheel click",
            Next::SetValue => "first set_value, then set_value +96",
        },
        variant,
        &format!(
            "blank_frames_after_the_first_set_value={blank_before} {}",
            position(&adj, target)
        ),
    );
}

/// The scrolled window's own scroll controller, the kinetic one.
fn kinetic_controller(scroller: &gtk::ScrolledWindow) -> Option<gtk::EventControllerScroll> {
    let controllers = scroller.observe_controllers();
    (0..controllers.n_items())
        .filter_map(|i| {
            controllers
                .item(i)
                .and_downcast::<gtk::EventControllerScroll>()
        })
        .find(|c| c.flags().contains(gtk::EventControllerScrollFlags::KINETIC))
}

/// (c) GTK's own kinetic scrolling: emits the scrolled window's scroll
/// controller's `decelerate` signal, as the controller does when a touchpad
/// scroll ends, and lets GtkScrolledWindow's deceleration tick callback run.
fn kinetic(name: &str, variant: Variant, start: Start, velocity: f64) {
    let (list, _window) = scroll_setup(variant, start);
    let Some(controller) = kinetic_controller(&list.scroller) else {
        println!("NOTE {name}: skipped, no kinetic scroll controller on the scrolled window");
        return;
    };
    let adj = list.scroller.vadjustment();
    let from = adj.value();
    begin();
    controller.emit_by_name::<()>("decelerate", &[&0.0f64, &velocity]);
    run_until(|| false, Duration::from_millis(3000));
    settle();
    report(
        name,
        &format!("decelerate at {velocity} units/s"),
        variant,
        &format!(
            "{} kinetic_scrolling={}",
            position(&adj, from),
            list.scroller.is_kinetic_scrolling()
        ),
    );
}

/// (c) GtkScrolledWindow's `scroll-child` keybinding signal, which animates
/// the adjustment with `gtk_adjustment_animate_to_value`.
fn scroll_child(name: &str, variant: Variant, start: Start, scroll: gtk::ScrollType, times: u32) {
    let (list, _window) = scroll_setup(variant, start);
    let adj = list.scroller.vadjustment();
    let from = adj.value();
    begin();
    let mut handled = 0;
    for _ in 0..times {
        if list.scroller.emit_scroll_child(scroll, false) {
            handled += 1;
        }
        run_until(|| false, Duration::from_millis(400));
    }
    settle();
    report(
        name,
        &format!("scroll-child {scroll:?} x{times}"),
        variant,
        &format!("{} handled={handled}", position(&adj, from)),
    );
}

/// Diagnostic: what a set_value from ordinary code does to the list.
fn debug_set_value(variant: Variant) {
    let list = list_parts(variant, LONG_LIST);
    let _window = show_list(&list);
    let adj = list.scroller.vadjustment();
    let bound = |list: &ListParts| -> String {
        let rows = list.rows.borrow();
        let mut positions: Vec<u32> = Vec::new();
        let mut child = list.view.first_child();
        while let Some(row) = child {
            child = row.next_sibling();
            if let Some(label) = row.first_child().and_downcast::<gtk::Label>() {
                if let Some(item) = rows.get(&label) {
                    positions.push(item.position());
                }
            }
        }
        positions.sort();
        format!(
            "{} rows, positions {:?}..{:?}, first visible row text {:?}",
            positions.len(),
            positions.first(),
            positions.last(),
            first_visible_text(list)
        )
    };
    {
        let rows = list.rows.borrow();
        let mut child = list.view.first_child();
        let mut i = 0;
        while let Some(row) = child {
            child = row.next_sibling();
            let label = row.first_child().and_downcast::<gtk::Label>();
            let item = label.as_ref().and_then(|l| rows.get(l));
            if i < 3 || (21..26).contains(&i) {
                println!(
                    "DEBUG child {i}: type {} pos {:?} label {:?} bounds {:?} label bounds {:?} alloc {:?} translated {:?}",
                    row.type_().name(),
                    item.map(|it| it.position()),
                    label.as_ref().map(|l| l.text().to_string()),
                    row.compute_bounds(&list.view)
                        .map(|b| (b.x(), b.y(), b.width(), b.height())),
                    label
                        .as_ref()
                        .and_then(|l| l.compute_bounds(&list.view))
                        .map(|b| (b.x(), b.y(), b.width(), b.height())),
                    (row.width(), row.height()),
                    row.compute_point(&list.view, &gtk::graphene::Point::new(0.0, 0.0))
                        .map(|p| (p.x(), p.y())),
                );
            }
            i += 1;
        }
        println!("DEBUG {} children", i);
    }
    let fired = Rc::new(Cell::new(0));
    {
        let fired = fired.clone();
        adj.connect_value_changed(move |a| {
            fired.set(fired.get() + 1);
            println!(
                "DEBUG frame {} value-changed to {} in {}",
                frames(),
                a.value(),
                phase().name()
            );
        });
    }
    let picked = |list: &ListParts| -> String {
        [2.0, 240.0, 470.0]
            .iter()
            .map(|&y| {
                let text = list
                    .view
                    .pick(100.0, y, gtk::PickFlags::DEFAULT)
                    .and_then(|w| match w.clone().downcast::<gtk::Label>() {
                        Ok(label) => Some(label),
                        Err(_) => w.first_child().and_downcast::<gtk::Label>(),
                    })
                    .map(|l| l.text().to_string());
                format!("y={y}: {text:?}")
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    println!(
        "DEBUG frame {} before: value {} hadj upper {:?} page {:?}; {}; picked {}",
        frames(),
        adj.value(),
        list.scroller.hadjustment().upper(),
        list.scroller.hadjustment().page_size(),
        bound(&list),
        picked(&list)
    );
    for target in [52260.0, 2000.0, 52260.0, 52360.0] {
        println!("DEBUG frame {} set_value({target})", frames());
        adj.set_value(target);
        println!(
            "DEBUG frame {} returned; value {}: {}",
            frames(),
            adj.value(),
            bound(&list)
        );
        let ctx = glib::MainContext::default();
        for _ in 0..3 {
            let f = frames();
            run_until(|| frames() > f, Duration::from_secs(1));
            while ctx.iteration(false) {}
            println!(
                "DEBUG frame {} after a frame; value {}: {}; picked {}",
                frames(),
                adj.value(),
                bound(&list),
                picked(&list)
            );
        }
        settle();
        settle();
        println!(
            "DEBUG frame {} settled; value {}: {}",
            frames(),
            adj.value(),
            bound(&list)
        );
    }
    println!("NOTE value-changed fired {} times", fired.get());
}

fn first_visible_text(list: &ListParts) -> Option<String> {
    let mut child = list.view.first_child();
    let mut best: Option<(f32, String)> = None;
    while let Some(row) = child {
        child = row.next_sibling();
        let Some(bounds) = row.compute_bounds(&list.view) else {
            continue;
        };
        if bounds.y() + bounds.height() <= 0.0 || bounds.y() >= list.view.height() as f32 {
            continue;
        }
        if best.as_ref().is_none_or(|(y, _)| bounds.y() < *y) {
            let text = row
                .first_child()
                .and_downcast::<gtk::Label>()
                .map(|l| l.text().to_string())
                .unwrap_or_default();
            best = Some((bounds.y(), text));
        }
    }
    best.map(|(y, t)| format!("{t} at y={y:.0}"))
}

// ---------------------------------------------------------------------------
// Scenarios

type Scenario = (String, Box<dyn Fn(Variant)>);

fn scenarios() -> Vec<Scenario> {
    let mut all: Vec<Scenario> = vec![
        ("debug-set-value".into(), Box::new(debug_set_value)),
        ("1-rows-added-in-driver".into(), Box::new(case1)),
        (
            "2a-new-window-built-in-ordinary-code".into(),
            Box::new(case2a),
        ),
        ("2b-new-window-built-in-driver".into(), Box::new(case2b)),
        (
            "2c-labels-added-to-shown-window-in-driver".into(),
            Box::new(case2c),
        ),
    ];
    let mut add = |name: String, f: Box<dyn Fn(Variant)>| all.push((name, f));

    // (a) set_value from ordinary code between main-loop iterations.
    let steps: [(&str, Start, f64, u32); 4] = [
        ("3a-set-value-down-from-top", Start::Top, 48.0, 30),
        (
            "3a-set-value-up-near-top",
            Start::FromTop(1800.0),
            -150.0,
            12,
        ),
        (
            "3a-set-value-down-near-end",
            Start::FromEnd(1800.0),
            150.0,
            12,
        ),
        (
            "3a-set-value-big-steps-mid",
            Start::Fraction(0.5),
            2400.0,
            10,
        ),
    ];
    for (name, start, step, n) in steps {
        let name_owned = name.to_string();
        add(
            name.into(),
            Box::new(move |v| set_value_steps(&name_owned, v, start, step, n)),
        );
    }
    add(
        "3a-set-value-jumps".into(),
        Box::new(|v| {
            set_value_jumps(
                "3a-set-value-jumps",
                v,
                &[20000.0, 60000.0, -0.0001, 0.0, 90000.0, -300.0, 300.0],
            )
        }),
    );

    // (b) set_value from a tick callback: inside the frame clock.
    let ticks: [(&str, Start, f64, u32); 17] = [
        ("3b-tick-down-from-top-24", Start::Top, 24.0, 60),
        ("3b-tick-down-from-top-96", Start::Top, 96.0, 60),
        ("3b-tick-down-from-top-480", Start::Top, 480.0, 60),
        ("3b-tick-down-from-top-2400", Start::Top, 2400.0, 40),
        ("3b-tick-down-mid-96", Start::Fraction(0.5), 96.0, 60),
        ("3b-tick-down-mid-480", Start::Fraction(0.5), 480.0, 60),
        ("3b-tick-down-mid-2400", Start::Fraction(0.5), 2400.0, 20),
        (
            "3b-tick-down-near-end-24",
            Start::FromEnd(3000.0),
            24.0,
            200,
        ),
        (
            "3b-tick-down-near-end-96",
            Start::FromEnd(3000.0),
            96.0,
            200,
        ),
        (
            "3b-tick-down-near-end-480",
            Start::FromEnd(3000.0),
            480.0,
            200,
        ),
        ("3b-tick-up-near-top-24", Start::FromTop(3000.0), -24.0, 200),
        ("3b-tick-up-near-top-96", Start::FromTop(3000.0), -96.0, 200),
        (
            "3b-tick-up-near-top-480",
            Start::FromTop(3000.0),
            -480.0,
            200,
        ),
        ("3b-tick-up-from-end-96", Start::End, -96.0, 60),
        ("3b-tick-up-mid-96", Start::Fraction(0.5), -96.0, 60),
        ("3b-tick-up-mid-480", Start::Fraction(0.5), -480.0, 60),
        ("3b-tick-up-mid-2400", Start::Fraction(0.5), -2400.0, 20),
    ];
    for (name, start, speed, frames) in ticks {
        let name_owned = name.to_string();
        add(
            name.into(),
            Box::new(move |v| tick_scroll(&name_owned, v, start, speed, frames)),
        );
    }

    // (c) Real GTK paths.
    let wheels: [(&str, Start, u32, u32, u32); 4] = [
        ("3c-wheel-down-from-top-x1", Start::Top, 5, 1, 20),
        (
            "3c-wheel-down-near-end-x1",
            Start::FromEnd(1800.0),
            5,
            1,
            30,
        ),
        ("3c-wheel-down-near-end-x8", Start::FromEnd(3000.0), 5, 8, 8),
        ("3c-wheel-up-near-top-x8", Start::FromTop(3000.0), 4, 8, 8),
    ];
    for (name, start, button, per, batches) in wheels {
        let name_owned = name.to_string();
        add(
            name.into(),
            Box::new(move |v| wheel(&name_owned, v, start, button, per, batches)),
        );
    }
    let kinetics: [(&str, Start, f64); 10] = [
        ("3c-kinetic-down-from-top-50", Start::Top, 50.0),
        ("3c-kinetic-down-from-top-200", Start::Top, 200.0),
        ("3c-kinetic-down-near-end-50", Start::FromEnd(3000.0), 50.0),
        (
            "3c-kinetic-down-near-end-200",
            Start::FromEnd(3000.0),
            200.0,
        ),
        ("3c-kinetic-up-near-top-50", Start::FromTop(3000.0), -50.0),
        ("3c-kinetic-up-near-top-200", Start::FromTop(3000.0), -200.0),
        ("3c-kinetic-down-mid-500", Start::Fraction(0.5), 500.0),
        ("3c-kinetic-up-mid-500", Start::Fraction(0.5), -500.0),
        ("3c-kinetic-up-from-end-200", Start::End, -200.0),
        ("3c-kinetic-up-from-end-500", Start::End, -500.0),
    ];
    for (name, start, velocity) in kinetics {
        let name_owned = name.to_string();
        add(
            name.into(),
            Box::new(move |v| kinetic(&name_owned, v, start, velocity)),
        );
    }
    for (name, next) in [
        ("3d-first-set-value-then-tick", Next::Tick),
        ("3d-first-set-value-then-wheel", Next::Wheel),
        ("3d-first-set-value-then-set-value", Next::SetValue),
    ] {
        add(
            name.into(),
            Box::new(move |v| first_jump_then(name, v, next)),
        );
    }
    let children: [(&str, Start, gtk::ScrollType, u32); 3] = [
        (
            "3c-scroll-child-page-down-near-end",
            Start::FromEnd(3000.0),
            gtk::ScrollType::PageForward,
            8,
        ),
        (
            "3c-scroll-child-end-from-top",
            Start::Top,
            gtk::ScrollType::End,
            1,
        ),
        (
            "3c-scroll-child-start-from-end",
            Start::End,
            gtk::ScrollType::Start,
            1,
        ),
    ];
    for (name, start, scroll, times) in children {
        let name_owned = name.to_string();
        add(
            name.into(),
            Box::new(move |v| scroll_child(&name_owned, v, start, scroll, times)),
        );
    }
    all
}

// ---------------------------------------------------------------------------
// Runner

fn field<'a>(line: &'a str, key: &str) -> &'a str {
    line.split_whitespace()
        .find_map(|token| {
            token
                .strip_prefix(key)
                .and_then(|rest| rest.strip_prefix('='))
        })
        .unwrap_or("?")
}

fn run_all(filter: Option<&str>) {
    let exe = std::env::current_exe().expect("the probe's path");
    for (name, _) in scenarios() {
        if filter.is_some_and(|f| !name.contains(f)) {
            continue;
        }
        println!("=== {name}");
        for variant in [Variant::Control, Variant::Deferred] {
            let out = Command::new("timeout")
                .arg("120")
                .arg(&exe)
                .arg(&name)
                .arg(variant.name())
                .output()
                .expect("run a scenario");
            let stdout = String::from_utf8_lossy(&out.stdout);
            let status = match (out.status.code(), out.status.signal()) {
                (Some(0), _) => String::new(),
                (Some(124), _) => " TIMED OUT".into(),
                (Some(code), _) => format!(" EXIT {code}"),
                (None, Some(signal)) => format!(" KILLED BY SIGNAL {signal}"),
                _ => " ?".into(),
            };
            for line in stdout.lines() {
                if line.starts_with("RESULT") {
                    println!(
                        "  {:<8} {:<34} frames {:>4}  painted {:>4}  wrong {:>3} (rows {:>4} empty, {:>4} stale, {:>2} unbound, of {:>5}; most px of a wrong label in view {:>2}, wholly in view {:>3}, found by pick {:>3}; blank frames {})  binds [{}] of them in view [{}]  writes [{}] dropped {}  polls-in-clock {}  pick-check {}/{}  max-step {}  {}",
                        field(line, "variant"),
                        field(line, "round").replace('_', " "),
                        field(line, "frames"),
                        field(line, "renders"),
                        field(line, "wrong_renders"),
                        field(line, "empty"),
                        field(line, "stale"),
                        field(line, "unbound"),
                        field(line, "checked"),
                        field(line, "wrong_px_max"),
                        field(line, "wrong_whole"),
                        field(line, "wrong_picked"),
                        field(line, "blank_renders"),
                        field(line, "binds"),
                        field(line, "binds_in_view"),
                        field(line, "landed"),
                        field(line, "dropped"),
                        field(line, "polls_in_clock"),
                        field(line, "pick_misses"),
                        field(line, "picks"),
                        field(line, "max_step"),
                        line.split_whitespace()
                            .skip_while(|t| !t.starts_with("max_step="))
                            .skip(1)
                            .collect::<Vec<_>>()
                            .join(" "),
                    );
                } else if line.starts_with("EXAMPLE") || line.starts_with("NOTE") {
                    println!("      {} {line}", variant.name());
                }
            }
            if !status.is_empty() {
                println!("  {:<8}{status}", variant.name());
                for line in String::from_utf8_lossy(&out.stderr).lines().take(20) {
                    println!("      stderr: {line}");
                }
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        None | Some("all") => run_all(args.get(2).map(String::as_str)),
        Some("list") => {
            for (name, _) in scenarios() {
                println!("{name}");
            }
        }
        Some(name) => {
            let variant = match args.get(2).map(String::as_str) {
                Some("control") => Variant::Control,
                Some("deferred") => Variant::Deferred,
                _ => panic!("usage: defer-paint-probe <scenario> control|deferred"),
            };
            gtk::init().expect("gtk init");
            install_hooks();
            spawn_driver();
            let scenarios = scenarios();
            let (_, scenario) = scenarios
                .iter()
                .find(|(n, _)| n == name)
                .unwrap_or_else(|| panic!("no scenario {name}; `list` names them"));
            scenario(variant);
        }
    }
}
