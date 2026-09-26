//! Headless checks of the GTK prototype. Each scenario drives real widgets
//! by emitting their signals, and asserts what it sees. Each runs in a
//! child process of its own, since a panic in a GTK handler aborts the
//! process; the harness reports how each one ended.
//!
//! Needs a display: `xvfb-run -a cargo test`. Without one it skips. A
//! name as an argument runs only the scenarios whose names contain it.

#[path = "../examples/app/mod.rs"]
mod app;

use std::cell::{Cell as StdCell, RefCell};
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, ExitCode, ExitStatus};
use std::rc::Rc;
use std::time::Duration;

use bough::{Cell, Input, Io, IoError, NowError, Owner, Runtime, Source};
use gtk::gio;
use gtk::glib::{self, clone};
use gtk::prelude::*;

use app::Row;

const SCENARIOS: &[(&str, fn())] = &[
    ("a_counter", a_counter),
    (
        "rows_wire_themselves_in_the_listener_that_hears_of_them",
        rows_wire_themselves,
    ),
    (
        "a_list_view_binds_rows_inside_a_listener",
        a_list_view_binds_rows_inside_a_listener,
    ),
    ("a_two_way_entry_settles", a_two_way_entry_settles),
    (
        "a_handler_the_first_call_sets_off_does_not_abort",
        a_handler_the_first_call_sets_off,
    ),
    (
        "a_remote_send_from_a_thread_reaches_a_label",
        a_remote_send_from_a_thread,
    ),
    (
        "rows_one_pump_opens_are_wired_between_its_units",
        rows_one_pump_opens,
    ),
    (
        "an_echo_is_not_cured_by_waiting",
        an_echo_is_not_cured_by_waiting,
    ),
    (
        "dropping_the_owner_ends_the_driver",
        dropping_the_owner_ends_the_driver,
    ),
];

fn main() -> ExitCode {
    if let Ok(name) = std::env::var("BOUGH_GTK_SCENARIO") {
        let (_, scenario) = SCENARIOS
            .iter()
            .find(|(n, _)| *n == name)
            .expect("a scenario by that name");
        gtk::init().expect("GTK initializes");
        scenario();
        return ExitCode::SUCCESS;
    }
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        println!("skipped: no display; run under `xvfb-run -a cargo test`");
        return ExitCode::SUCCESS;
    }
    let filter = std::env::args().skip(1).find(|arg| !arg.starts_with('-'));
    let exe = std::env::current_exe().expect("the test binary");
    let mut failed = 0;
    for (name, _) in SCENARIOS {
        if filter.as_ref().is_some_and(|f| !name.contains(f.as_str())) {
            continue;
        }
        let out = Command::new("timeout")
            .arg("60")
            .arg(&exe)
            .env("BOUGH_GTK_SCENARIO", name)
            .env("RUST_BACKTRACE", "0")
            .output()
            .expect("the scenario runs");
        let ok = out.status.success();
        println!("{} {name}", if ok { "ok  " } else { "FAIL" });
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            println!("       {line}");
        }
        if !ok {
            failed += 1;
            println!("       ended: {}", describe(out.status));
            for line in String::from_utf8_lossy(&out.stderr).lines() {
                if !line.trim().is_empty() {
                    println!("       stderr: {line}");
                }
            }
        }
    }
    if failed > 0 {
        println!("{failed} failed");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn describe(status: ExitStatus) -> String {
    match (status.code(), status.signal()) {
        (Some(124), _) => "timed out after 60 s".into(),
        (Some(134), _) | (None, Some(6)) => "aborted: a panic reached a GTK handler".into(),
        (Some(code), _) => format!("exit code {code}"),
        (None, Some(signal)) => format!("killed by signal {signal}"),
        _ => "unknown".into(),
    }
}

/// Runs the main loop until it is quiet, so that X events, the frame
/// clock and spawned futures have run.
fn settle() {
    let context = glib::MainContext::default();
    for _ in 0..40 {
        while context.iteration(false) {}
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn window_with(child: &impl IsA<gtk::Widget>) -> gtk::Window {
    let window = gtk::Window::new();
    window.set_default_size(320, 480);
    window.set_child(Some(child));
    window.present();
    settle();
    window
}

/// Prints what a scenario saw, and asserts it.
fn check<T: PartialEq + std::fmt::Debug>(what: &str, seen: T, expected: T) {
    println!("{what}: {seen:?}");
    assert_eq!(seen, expected, "{what}");
}

fn strings(texts: &[&str]) -> Vec<String> {
    texts.iter().map(|text| text.to_string()).collect()
}

/// The labels among a box's children, in order.
fn box_labels(list: &gtk::Box) -> Vec<String> {
    let mut labels = Vec::new();
    let mut child = list.first_child();
    while let Some(widget) = child {
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            labels.push(label.text().to_string());
        }
        child = widget.next_sibling();
    }
    labels
}

/// The labels a list view shows, in order: each row's widget holds one.
fn list_view_labels(view: &gtk::ListView) -> Vec<String> {
    let mut labels = Vec::new();
    let mut child = view.first_child();
    while let Some(widget) = child {
        if let Some(label) = widget.first_child().and_downcast::<gtk::Label>() {
            labels.push(label.text().to_string());
        }
        child = widget.next_sibling();
    }
    labels
}

/// The baseline: a label bound to a cell, and a button with a sender.
fn a_counter() {
    let (graph, app) = app::build();
    let owner = Owner::new(graph);
    let io = owner.io();
    let label = gtk::Label::new(None);
    let click = gtk::Button::with_label("Click");
    let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
    column.append(&click);
    column.append(&label);
    let _window = window_with(&column);
    bough_gtk::bind_label(&io, app.counter_label, &label).unwrap();
    let clicked = bough_gtk::sender(&io, app.clicks_in);
    click.connect_clicked(move |_| clicked(()));
    for _ in 0..3 {
        click.emit_clicked();
    }
    check(
        "the label after 3 clicks",
        label.text().to_string(),
        "Clicked 3 times".into(),
    );
}

/// Each row is a component built in the listener that hears of it, which
/// registers its label's listener while the graph is busy. Removing a row
/// drops its widget, which drops its listener, and a collection frees the
/// row's nodes.
fn rows_wire_themselves() {
    let (graph, app) = app::build();
    let owner = Owner::new(graph);
    let io = owner.io();
    let list = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let _window = window_with(&list);
    let views = app::row_list(&io, app, &list).unwrap();
    for name in ["alpha", "beta", "gamma"] {
        bough_gtk::send(&io, app.add_in, name.to_string());
    }
    let rows = io.with_sample(app.rows, |rows| rows.clone()).unwrap();
    let bump = views.borrow()[&rows[1]].bump.clone();
    bump.emit_clicked();
    bump.emit_clicked();
    let labels = |rows: &[Row]| -> Vec<String> {
        rows.iter()
            .map(|row| views.borrow()[row].label.text().to_string())
            .collect()
    };
    check(
        "the rows' labels",
        labels(&rows),
        strings(&["alpha: 0", "beta: 2", "gamma: 0"]),
    );

    let collect = || {
        io.with_graph(|graph| {
            graph.collect_garbage();
            graph.live_nodes()
        })
        .unwrap()
    };
    let before = collect();
    let alpha = views.borrow()[&rows[0]].label.downgrade();
    let remove = views.borrow()[&rows[0]].remove.clone();
    remove.emit_clicked();
    drop(remove);
    let after = collect();
    let left = io.with_sample(app.rows, |rows| rows.clone()).unwrap();
    check(
        "the rows left",
        labels(&left),
        strings(&["beta: 2", "gamma: 0"]),
    );
    check(
        "alpha's label is finalized",
        alpha.upgrade().is_none(),
        true,
    );
    println!("live nodes: {before} before the removal, {after} after");
    assert!(after < before, "the removed row's nodes are freed");
}

/// The model follows the `rows` cell from a listener, and GTK binds the
/// new row inside that listener, while the graph is busy. The factory's
/// bind registers through the handle, so the registration waits for the
/// transaction, and the labels are right before the main loop runs.
fn a_list_view_binds_rows_inside_a_listener() {
    let (graph, app) = app::build();
    let owner = Owner::new(graph);
    let io = owner.io();
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let busy_binds = Rc::new(StdCell::new(0));
    let factory = bough_gtk::list_factory(
        &io,
        || gtk::Label::new(None),
        clone!(
            #[strong]
            busy_binds,
            move |io: &Io, row: &Row, label: &gtk::Label| {
                // A read cannot wait, so it tells whether the graph is busy.
                if io.with_sample(row.label, |_| ()) == Err(NowError::Busy) {
                    busy_binds.set(busy_binds.get() + 1);
                }
                let shown = io.listen_cell(
                    row.label,
                    clone!(
                        #[weak]
                        label,
                        move |text: &String| label.set_text(text)
                    ),
                );
                shown.into_iter().collect()
            }
        ),
    );
    let view = gtk::ListView::new(
        Some(gtk::NoSelection::new(Some(store.clone()))),
        Some(factory),
    );
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_child(Some(&view));
    scroller.set_vexpand(true);
    let _window = window_with(&scroller);
    io.listen_cell(
        app.rows,
        clone!(
            #[weak]
            store,
            move |rows: &Vec<Row>| bough_gtk::sync_store(&store, rows)
        ),
    )
    .unwrap()
    .keep();
    for name in ["alpha", "beta", "gamma"] {
        bough_gtk::send(&io, app.add_in, name.to_string());
    }
    println!("binds while the graph was busy: {}", busy_binds.get());
    assert!(busy_binds.get() > 0, "GTK bound a row inside a listener");
    check(
        "the labels right after the sends, before the main loop runs",
        list_view_labels(&view),
        strings(&["alpha: 0", "beta: 0", "gamma: 0"]),
    );
    let rows = io.with_sample(app.rows, |rows| rows.clone()).unwrap();
    bough_gtk::send(&io, rows[2].clicks_in, ());
    bough_gtk::send(&io, app.remove_in, rows[0]);
    settle();
    check(
        "the labels after a click on gamma and removing alpha",
        list_view_labels(&view),
        strings(&["beta: 0", "gamma: 1"]),
    );
}

/// `bind_entry` blocks the entry's handler while it writes, so the write
/// sends nothing back.
fn a_two_way_entry_settles() {
    let (graph, app) = app::build();
    let owner = Owner::new(graph);
    let io = owner.io();
    let entry = gtk::Entry::new();
    let _window = window_with(&entry);
    let steps: Rc<RefCell<Vec<String>>> = Rc::default();
    io.listen_steps(
        app.shout,
        clone!(
            #[strong]
            steps,
            move |text: &String| steps.borrow_mut().push(text.clone())
        ),
    )
    .unwrap()
    .keep();
    bough_gtk::bind_entry(&io, app.shout, app.text_in, &entry).unwrap();
    entry.set_text("hello");
    settle();
    check("the entry shows", entry.text().to_string(), "HELLO".into());
    check(
        "the shouted cell's steps",
        steps.borrow().clone(),
        strings(&["HELLO"]),
    );
}

/// Registering a listener calls it at once, and here that call writes a
/// check button, whose handler sends while the graph is busy. With a
/// `RefCell` this aborted; through the handle the send waits.
fn a_handler_the_first_call_sets_off() {
    let (graph, app) = app::build();
    let owner = Owner::new(graph);
    let io = owner.io();
    let check_button = gtk::CheckButton::new();
    let _window = window_with(&check_button);
    bough_gtk::send(&io, app.flag_in, true);
    let toggles: Rc<RefCell<Vec<bool>>> = Rc::default();
    check_button.connect_toggled(clone!(
        #[strong]
        io,
        #[strong]
        toggles,
        move |check| {
            toggles.borrow_mut().push(check.is_active());
            bough_gtk::send(&io, app.flag_in, check.is_active());
        }
    ));
    // Not blocked, on purpose.
    io.listen_cell(
        app.flag,
        clone!(
            #[weak]
            check_button,
            move |on: &bool| check_button.set_active(*on)
        ),
    )
    .unwrap()
    .keep();
    check(
        "toggles during registration",
        toggles.borrow().clone(),
        vec![true],
    );
    check("the flag", io.with_sample(app.flag, |on| *on), Ok(true));
}

/// Another thread sends through a `Remote`; the driver, a future on the
/// main loop, pumps when the send wakes it.
fn a_remote_send_from_a_thread() {
    let (graph, app) = app::build();
    let remote = graph.remote();
    let owner = Owner::new(graph);
    let io = owner.io();
    let label = gtk::Label::new(None);
    let _window = window_with(&label);
    bough_gtk::bind_label(&io, app.clock, &label).unwrap();
    let _driver = bough_gtk::spawn_driver(&io);
    settle(); // the driver's first poll registers its waker
    std::thread::spawn(move || {
        for n in 1..=3 {
            remote.send(app.ticks_in, n);
        }
    })
    .join()
    .unwrap();
    check(
        "the label before the main loop runs",
        label.text().to_string(),
        "tick 0".into(),
    );
    settle();
    check("the label after", label.text().to_string(), "tick 3".into());
}

/// Two remote units, one pump. Nothing in the graph holds a counter, so
/// only what I/O code wires keeps one alive, and a collection runs before
/// every transaction. The listener wires each counter through the handle,
/// and the queue runs between the units, so both survive.
fn rows_one_pump_opens() {
    let (mut graph, (open_in, opened)) = Runtime::build(|b| {
        let (open, open_in) = b.input::<u32>();
        let opened = open.construct(b, |b, start| {
            let (bumps, bumps_in) = b.input::<u32>();
            (bumps_in, bumps.accumulate(b, start, |n, c| c + n))
        });
        (open_in, opened)
    });
    graph.set_collect_after_every_transaction(true);
    let remote = graph.remote();
    let owner = Owner::new(graph);
    let io = owner.io();
    let list = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let _window = window_with(&list);
    let inputs: Rc<RefCell<Vec<Input<u32>>>> = Rc::default();
    io.listen(
        opened,
        clone!(
            #[strong]
            io,
            #[strong]
            inputs,
            #[weak]
            list,
            move |(bumps_in, count): (Input<u32>, Cell<u32>)| {
                let label = gtk::Label::new(None);
                let shown = io.listen_cell(
                    count,
                    clone!(
                        #[weak]
                        label,
                        move |n: &u32| label.set_text(&n.to_string())
                    ),
                );
                match shown {
                    Ok(listener) => bough_gtk::tie(&label, listener),
                    Err(error) => glib::g_critical!("scenario", "not wired: {error:?}"),
                }
                list.append(&label);
                inputs.borrow_mut().push(bumps_in);
            }
        ),
    )
    .unwrap()
    .keep();
    let _driver = bough_gtk::spawn_driver(&io);
    settle();
    remote.send(open_in, 10);
    remote.send(open_in, 20);
    settle();
    check("the labels", box_labels(&list), strings(&["10", "20"]));
    for input in inputs.borrow().iter() {
        // A counter that was collected would make this a stale send.
        bough_gtk::send(&io, *input, 1);
    }
    check(
        "the labels after a bump each",
        box_labels(&list),
        strings(&["11", "21"]),
    );
}

/// A plain two-way binding, without blocking its own handler. The echo
/// no longer aborts, and each call returns, but it never settles: the
/// driver runs it on, a little at every turn of the main loop, until the
/// handler here stops at 40 steps. Only blocking the handler fixes it.
fn an_echo_is_not_cured_by_waiting() {
    let (graph, app) = app::build();
    let owner = Owner::new(graph);
    let io = owner.io();
    let entry = gtk::Entry::new();
    let _window = window_with(&entry);
    let steps: Rc<RefCell<Vec<String>>> = Rc::default();
    io.listen_steps(
        app.shout,
        clone!(
            #[strong]
            steps,
            move |text: &String| steps.borrow_mut().push(text.clone())
        ),
    )
    .unwrap()
    .keep();
    let _driver = bough_gtk::spawn_driver(&io);
    settle();
    entry.connect_changed(clone!(
        #[strong]
        io,
        #[strong]
        steps,
        move |entry| {
            if steps.borrow().len() < 40 {
                bough_gtk::send(&io, app.text_in, entry.text().to_string());
            }
        }
    ));
    io.listen_cell(
        app.shout,
        clone!(
            #[weak]
            entry,
            move |text: &String| {
                if entry.text() != text.as_str() {
                    entry.set_text(text);
                }
            }
        ),
    )
    .unwrap()
    .keep();
    entry.set_text("hello");
    let first = steps.borrow().len();
    println!("steps when set_text returned: {:?}", steps.borrow());
    settle();
    let last = steps.borrow().len();
    println!(
        "steps once the main loop ran: {last}, starting {:?}",
        &steps.borrow()[..8.min(last)]
    );
    assert!(first < 40, "set_text returned before the cap");
    assert!(last >= 40, "the driver ran the echo on until the cap");
}

/// The window closes: the owner goes, its drop wakes the driver, and the
/// driver ends. A handler that runs after it finds the graph gone.
fn dropping_the_owner_ends_the_driver() {
    let (graph, app) = app::build();
    let owner = Owner::new(graph);
    let io = owner.io();
    let click = gtk::Button::with_label("Click");
    let _window = window_with(&click);
    let clicked = bough_gtk::sender(&io, app.clicks_in);
    click.connect_clicked(move |_| clicked(()));
    let ended = Rc::new(StdCell::new(false));
    let driver = bough_gtk::spawn_driver(&io);
    glib::spawn_future_local(clone!(
        #[strong]
        ended,
        async move {
            let _ = driver.await;
            ended.set(true);
        }
    ));
    settle();
    check("the driver ended while the owner lives", ended.get(), false);
    drop(owner);
    click.emit_clicked(); // ignored: the graph is gone
    settle();
    check("the driver ended once the owner went", ended.get(), true);
    check("a send now", io.send(app.clicks_in, ()), Err(IoError::Gone));
}
