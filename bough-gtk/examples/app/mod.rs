//! The app the demo shows and the scenarios drive. First the graph, which
//! knows nothing of GTK; then a row component that wires itself.

// The demo and the scenarios each use part of this module.
#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use bough::{Cell, Graph, Input, Io, IoError, Shared, Source, Trace};
use gtk::glib::{self, clone};
use gtk::prelude::*;

/// A row, built at run time by `construct`, with an input and a cell of
/// its own. Tokens are `Copy + Eq + Hash`, so a row is its own identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Trace)]
pub struct Row {
    pub clicks_in: Input<()>,
    pub label: Cell<String>,
}

#[derive(Clone)]
pub enum Edit {
    Add(Row),
    Remove(Row),
}

/// The edge of the graph, and its permanent roots.
#[derive(Clone, Copy, Trace)]
pub struct App {
    pub clicks_in: Input<()>,
    pub counter_label: Cell<String>,
    pub add_in: Input<String>,
    pub remove_in: Input<Row>,
    /// Each row as it is built: how I/O code first hears of its tokens.
    pub opened: Shared<Row>,
    /// The rows now, in order. The cell's value holds the rows' tokens,
    /// which keeps them alive until a row is removed.
    pub rows: Cell<Vec<Row>>,
    pub text_in: Input<String>,
    /// The entry's text, shouted: the entry shows it, both ways.
    pub shout: Cell<String>,
    pub flag_in: Input<bool>,
    pub flag: Cell<bool>,
    /// Ticks another thread sends through a `Remote`.
    pub ticks_in: Input<u64>,
    pub clock: Cell<String>,
}

pub fn build() -> (Graph, App) {
    Graph::build(|b| {
        let (clicks, clicks_in) = b.input::<()>();
        let count = clicks.accumulate(b, 0u32, |_, n| n + 1);
        let counter_label = count.map_cell(b, |n| format!("Clicked {n} times"));

        let (adds, add_in) = b.input::<String>();
        let (removes, remove_in) = b.input::<Row>();
        let opened = adds
            .construct(b, |b, name: String| {
                let (clicks, clicks_in) = b.input::<()>();
                let count = clicks.accumulate(b, 0u32, |_, n| n + 1);
                let label = count.map_cell(b, move |n| format!("{name}: {n}"));
                Row { clicks_in, label }
            })
            .share(b);
        let rows = opened
            .map(Edit::Add)
            .or_else(b, removes.map(Edit::Remove))
            .accumulate(b, Vec::new(), |edit, rows: &Vec<Row>| {
                let mut rows = rows.clone();
                match edit {
                    Edit::Add(row) => rows.push(row),
                    Edit::Remove(row) => rows.retain(|r| *r != row),
                }
                rows
            });

        let (text, text_in) = b.input_cell(String::new());
        let shout = text.map_cell(b, |s: &String| s.to_uppercase());
        let (flag, flag_in) = b.input_cell(false);
        let (ticks, ticks_in) = b.input::<u64>();
        let clock = ticks
            .map(|n| format!("tick {n}"))
            .hold(b, "tick 0".to_string());
        App {
            clicks_in,
            counter_label,
            add_in,
            remove_in,
            opened,
            rows,
            text_in,
            shout,
            flag_in,
            flag,
            ticks_in,
            clock,
        }
    })
}

/// A row that wires itself. It takes the handle, not the graph, so it can
/// be made anywhere, including in the listener that hears of the row.
pub struct RowView {
    pub line: gtk::Box,
    pub label: gtk::Label,
    pub bump: gtk::Button,
    pub remove: gtk::Button,
}

impl RowView {
    pub fn new(io: &Io, app: &App, row: Row) -> Result<RowView, IoError> {
        let label = gtk::Label::new(None);
        let bump = gtk::Button::with_label("+1");
        let remove = gtk::Button::with_label("Remove");
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        line.append(&label);
        line.append(&bump);
        line.append(&remove);
        bough_gtk::bind_label(io, row.label, &label)?;
        let click = bough_gtk::sender(io, row.clicks_in);
        bump.connect_clicked(move |_| click(()));
        let removal = bough_gtk::sender(io, app.remove_in);
        remove.connect_clicked(move |_| removal(row));
        Ok(RowView {
            line,
            label,
            bump,
            remove,
        })
    }
}

/// The rows as components in `list`: each is built in the listener that
/// hears of it, and taken out when the `rows` cell no longer holds it.
/// Taking out the widget drops its listener, and the row's nodes are
/// freed at the next collection.
pub fn row_list(
    io: &Io,
    app: App,
    list: &gtk::Box,
) -> Result<Rc<RefCell<HashMap<Row, RowView>>>, IoError> {
    let views: Rc<RefCell<HashMap<Row, RowView>>> = Rc::default();
    io.listen(
        app.opened,
        clone!(
            #[strong]
            io,
            #[strong]
            views,
            #[weak]
            list,
            move |row: Row| match RowView::new(&io, &app, row) {
                Ok(view) => {
                    list.append(&view.line);
                    views.borrow_mut().insert(row, view);
                }
                Err(error) => glib::g_critical!("app", "a row was not wired: {error:?}"),
            }
        ),
    )?
    .keep();
    io.listen_cell(
        app.rows,
        clone!(
            #[strong]
            views,
            #[weak]
            list,
            move |rows: &Vec<Row>| {
                let gone: Vec<RowView> = {
                    let mut views = views.borrow_mut();
                    let gone: Vec<Row> = views
                        .keys()
                        .filter(|row| !rows.contains(row))
                        .copied()
                        .collect();
                    gone.iter().filter_map(|row| views.remove(row)).collect()
                };
                // Outside the borrow: removing a widget can run handlers.
                for view in gone {
                    list.remove(&view.line);
                }
            }
        ),
    )?
    .keep();
    Ok(views)
}
