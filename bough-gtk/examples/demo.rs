//! The demo window: `cargo run --example demo`. What the scenarios check,
//! in one window to click through.

mod app;

use std::time::Duration;

use bough::Owner;
use gtk::glib::{self, clone};
use gtk::prelude::*;
use gtk::{gio, glib::ExitCode};

use app::Row;

fn main() -> ExitCode {
    let application = gtk::Application::builder()
        .application_id("org.bough.GtkSpike")
        .build();
    application.connect_activate(activate);
    application.run()
}

fn activate(application: &gtk::Application) {
    let (graph, app) = app::build();
    let remote = graph.remote();
    let owner = Owner::new(graph);
    let io = owner.io();

    // A counter: a label bound to a cell, and a button with a sender.
    let counter = gtk::Label::new(None);
    let click = gtk::Button::with_label("Click");
    bough_gtk::bind_label(&io, app.counter_label, &counter).unwrap();
    let clicked = bough_gtk::sender(&io, app.clicks_in);
    click.connect_clicked(move |_| clicked(()));

    // Two-way text: what you type comes back shouted.
    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some("Type here"));
    bough_gtk::bind_entry(&io, app.shout, app.text_in, &entry).unwrap();

    // Two-way flag. Registering the listener writes the check button at
    // once, and its handler would send while the graph is busy; the
    // handle makes that send wait, and blocking the handler makes it moot.
    let flag = gtk::CheckButton::with_label("A flag in the graph");
    let toggled = flag.connect_toggled(clone!(
        #[strong]
        io,
        move |check| bough_gtk::send(&io, app.flag_in, check.is_active())
    ));
    let shown = io
        .listen_cell(
            app.flag,
            clone!(
                #[weak]
                flag,
                move |on: &bool| {
                    flag.block_signal(&toggled);
                    flag.set_active(*on);
                    flag.unblock_signal(&toggled);
                }
            ),
        )
        .unwrap();
    bough_gtk::tie(&flag, shown);

    // Rows: each built by `construct`, each wired by the listener that
    // hears of it.
    let name = gtk::Entry::new();
    name.set_placeholder_text(Some("A new row's name"));
    let add = gtk::Button::with_label("Add row");
    add.connect_clicked(clone!(
        #[strong]
        io,
        #[weak]
        name,
        move |_| {
            bough_gtk::send(&io, app.add_in, name.text().to_string());
            name.set_text("");
        }
    ));
    let list = gtk::Box::new(gtk::Orientation::Vertical, 4);
    app::row_list(&io, app, &list).unwrap();

    // The same rows in a list view, whose rows GTK binds when it likes,
    // here inside the listener that updates the model.
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let factory = bough_gtk::list_factory(
        &io,
        || gtk::Label::new(None),
        |io, row: &Row, label: &gtk::Label| {
            let shown = io.listen_cell(
                row.label,
                clone!(
                    #[weak]
                    label,
                    move |text: &String| label.set_text(text)
                ),
            );
            shown.into_iter().collect()
        },
    );
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
    let view = gtk::ListView::new(Some(gtk::NoSelection::new(Some(store))), Some(factory));
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_child(Some(&view));
    scroller.set_vexpand(true);

    // A clock another thread ticks through a `Remote`, which the driver
    // pumps.
    let clock = gtk::Label::new(None);
    bough_gtk::bind_label(&io, app.clock, &clock).unwrap();
    bough_gtk::spawn_driver(&io);
    std::thread::spawn(move || {
        for n in 1.. {
            std::thread::sleep(Duration::from_secs(1));
            if remote.try_send(app.ticks_in, n).is_err() {
                break;
            }
        }
    });

    let row = |widgets: &[&gtk::Widget]| {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        for widget in widgets {
            line.append(*widget);
        }
        line
    };
    let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
    column.set_margin_top(12);
    column.set_margin_bottom(12);
    column.set_margin_start(12);
    column.set_margin_end(12);
    column.append(&clock);
    column.append(&row(&[click.upcast_ref(), counter.upcast_ref()]));
    column.append(&entry);
    column.append(&flag);
    column.append(&row(&[name.upcast_ref(), add.upcast_ref()]));
    column.append(&gtk::Label::new(Some("Rows as components")));
    column.append(&list);
    column.append(&gtk::Label::new(Some("The same rows in a list view")));
    column.append(&scroller);

    let window = gtk::ApplicationWindow::builder()
        .application(application)
        .title("bough-gtk spike")
        .default_width(420)
        .default_height(640)
        .child(&column)
        .build();
    // The window keeps the graph: the owner goes when the window does.
    bough_gtk::tie(&window, owner);
    window.present();
}
