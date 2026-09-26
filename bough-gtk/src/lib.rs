//! Spike, not API: GTK 4 on bough's same-thread handle.
//!
//! What a `bough-gtk` crate would give an app. The app shares its graph
//! through a [`bough::Owner`], and every widget reaches it through a
//! [`bough::Io`]. GTK runs handlers while the graph is busy: a listener
//! writes to a widget, and the widget runs its handler at once, or a list
//! view binds a row inside a model change a listener made. The handle
//! lets that code send and register anyway: its calls wait for the
//! transaction to end.
//!
//! A panic in a GTK handler aborts the process, since gtk-rs calls it
//! from C and cannot unwind. So these helpers report refusals instead of
//! panicking: [`send`] ignores a graph that is gone and logs the rest.

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::poll_fn;
use std::rc::Rc;
use std::task::Poll;

use bough::{CellRef, Input, Io, IoError, Listener, NowError};
use gtk::glib::{self, clone};
use gtk::prelude::*;
use gtk::{gio, glib::object::IsA};

/// Sends from a signal handler. A graph that is gone is ignored, since a
/// window's widgets can run handlers while it closes; other refusals are
/// logged, since a panic here would abort.
pub fn send<A: 'static>(io: &Io, input: Input<A>, value: A) {
    match io.send(input, value) {
        Ok(()) | Err(IoError::Gone) => {}
        Err(error) => glib::g_critical!("bough-gtk", "a send was refused: {error:?}"),
    }
}

/// A relm4-style sender: a `'static` closure that sends to one input, to
/// move into a signal handler.
pub fn sender<A: 'static>(io: &Io, input: Input<A>) -> impl Fn(A) + Clone + 'static {
    let io = io.clone();
    move |value| send(&io, input, value)
}

/// Keeps `handle`, a [`Listener`] or an [`Anchor`](bough::Anchor), for as
/// long as `widget` lives, and drops it when the widget is finalized.
pub fn tie<T: 'static>(widget: &impl IsA<glib::Object>, handle: T) {
    widget.add_weak_ref_notify_local(move || drop(handle));
}

/// Shows a string cell in a label. The listener holds the label weakly,
/// and lives as long as the label.
pub fn bind_label<C>(io: &Io, cell: C, label: &gtk::Label) -> Result<(), IoError>
where
    C: CellRef<Value = String> + 'static,
{
    let listener = io.listen_cell(
        cell,
        clone!(
            #[weak]
            label,
            move |text: &String| label.set_text(text)
        ),
    )?;
    tie(label, listener);
    Ok(())
}

/// Binds an entry both ways: typing sends its text to `input`, and the
/// entry shows `cell`. The listener blocks the entry's own handler while it
/// writes. `set_text` emits `changed` twice, first with the empty text,
/// and sending those back would never settle, whether the sends wait or
/// not.
pub fn bind_entry<C>(
    io: &Io,
    cell: C,
    input: Input<String>,
    entry: &gtk::Entry,
) -> Result<(), IoError>
where
    C: CellRef<Value = String> + 'static,
{
    let changed = entry.connect_changed(clone!(
        #[strong]
        io,
        move |entry| send(&io, input, entry.text().to_string())
    ));
    let listener = io.listen_cell(
        cell,
        clone!(
            #[weak]
            entry,
            move |text: &String| {
                if entry.text() != text.as_str() {
                    entry.block_signal(&changed);
                    entry.set_text(text);
                    entry.unblock_signal(&changed);
                }
            }
        ),
    )?;
    tie(entry, listener);
    Ok(())
}

/// Spawns the driver on the thread's main context: a future that pumps
/// whenever a remote send, a slot write or a call the handle's queue left
/// over wakes it. It ends when the graph is gone or poisoned.
pub fn spawn_driver(io: &Io) -> glib::JoinHandle<()> {
    let io = io.clone();
    glib::spawn_future_local(poll_fn(move |cx| {
        let polled = io
            .with_graph(|graph| graph.set_waker(cx.waker().clone()))
            .and_then(|()| io.pump());
        match polled {
            Ok(()) => Poll::Pending,
            // A nested main loop inside a handler: try again at its next turn.
            Err(NowError::Busy) => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Err(_) => Poll::Ready(()),
        }
    }))
}

/// A factory for a list view whose model holds `T`s as
/// `glib::BoxedAnyObject`s. `setup` makes a row's widget. `bind` wires it
/// to its item through the handle, and returns the listeners, which are
/// dropped when GTK unbinds the row. GTK binds at its own time, often
/// inside a model change a listener made, which is why registration goes
/// through the handle.
pub fn list_factory<T, W>(
    io: &Io,
    setup: impl Fn() -> W + 'static,
    bind: impl Fn(&Io, &T, &W) -> Vec<Listener> + 'static,
) -> gtk::SignalListItemFactory
where
    T: 'static,
    W: IsA<gtk::Widget>,
{
    let bound: Rc<RefCell<HashMap<gtk::ListItem, Vec<Listener>>>> = Rc::default();
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
        item.set_child(Some(&setup()));
    });
    factory.connect_bind(clone!(
        #[strong]
        io,
        #[strong]
        bound,
        move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
            let boxed = item
                .item()
                .and_downcast::<glib::BoxedAnyObject>()
                .expect("a boxed item");
            let widget = item.child().and_downcast::<W>().expect("the setup widget");
            let listeners = bind(&io, &boxed.borrow::<T>(), &widget);
            bound.borrow_mut().insert(item.clone(), listeners);
        }
    ));
    factory.connect_unbind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
        bound.borrow_mut().remove(item);
    });
    factory
}

/// Brings a store of `glib::BoxedAnyObject`s up to date with `items`: it
/// removes what `items` no longer holds and appends what is new. It keeps
/// the order of what stays, which is enough for a list that only appends.
pub fn sync_store<T: Clone + PartialEq + 'static>(store: &gio::ListStore, items: &[T]) {
    let present: Vec<T> = (0..store.n_items())
        .map(|i| {
            let boxed = store
                .item(i)
                .and_downcast::<glib::BoxedAnyObject>()
                .expect("a boxed item");
            boxed.borrow::<T>().clone()
        })
        .collect();
    for i in (0..present.len()).rev() {
        if !items.contains(&present[i]) {
            store.remove(i as u32);
        }
    }
    for item in items {
        if !present.contains(item) {
            store.append(&glib::BoxedAnyObject::new(item.clone()));
        }
    }
}
