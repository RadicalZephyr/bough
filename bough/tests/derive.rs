//! `#[derive(Trace)]` (RFD 3): every field traced, `#[trace(skip)]` for a
//! field that cannot hold tokens, and RFD 6's chat room, whose members'
//! table is a cell of senders the collector must not look into.
#![cfg(feature = "derive")]

use std::cell::{Cell as StdCell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::mpsc;

use bough::{
    Cell, CollectionPolicy, Input, Runtime, Shared, Source, State, Stream, TokenError, Trace,
};

/// A type with no `Trace`, which only a skipped field may have.
struct Opaque(u64);

/// Token fields, tokens nested in collections, and two skipped fields: one
/// whose type has no `Trace`, and one that holds a token, which the
/// collector then does not see.
#[derive(Trace)]
struct Screen {
    title: String,
    clicks_in: Input<u32>,
    events: Shared<u32>,
    panels: Vec<Option<Cell<u32>>>,
    by_name: BTreeMap<String, (Cell<u32>, Vec<State<u32>>)>,
    #[trace(skip)]
    opaque: Opaque,
    #[trace(skip)]
    hidden: Cell<u32>,
}

/// A derived value in a constant is traced at every collection: every
/// token its fields hold, nested ones included, stays alive with the
/// constant, and the token in a skipped field is collected, since nothing
/// else names it, and is stale.
#[test]
fn a_derived_struct_roots_its_tokens_and_skips_what_it_is_told() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (clicks, clicks_in) = b.input::<u32>();
        let events = clicks.share(b);
        let first = events.hold(b, 0u32);
        let second = events.map(|c| c * 2).hold(b, 0u32);
        let counted = events.accumulate_mut(b, 0u32, |_, n: &mut u32| *n += 1);
        let named = b.constant(7u32);
        let hidden = b.constant(9u32);
        let screen = Screen {
            title: "home".to_string(),
            clicks_in,
            events,
            panels: vec![Some(first), None, Some(second)],
            by_name: BTreeMap::from([("named".to_string(), (named, vec![counted]))]),
            opaque: Opaque(42),
            hidden,
        };
        b.constant(screen)
    });
    let screen = edge.keep();
    graph.collect_garbage();
    let screen = graph.sample(screen);
    assert_eq!((screen.title.as_str(), screen.opaque.0), ("home", 42));
    let (clicks_in, events, hidden) = (screen.clicks_in, screen.events, screen.hidden);
    let first = screen.panels[0].expect("a panel");
    let second = screen.panels[2].expect("a panel");
    let (named, counted) = {
        let (named, states) = &screen.by_name["named"];
        (*named, states[0])
    };
    assert_eq!(graph.try_sample(hidden).err(), Some(TokenError::Stale));
    assert_eq!(*graph.sample(named), 7);
    let heard = Rc::new(StdCell::new(0));
    let writer = heard.clone();
    graph
        .listen(events, move |c| writer.set(writer.get() + c))
        .keep();
    graph.send(clicks_in, 3);
    assert_eq!((*graph.sample(first), *graph.sample(second)), (3, 6));
    assert_eq!(*graph.sample(counted), 1);
    assert_eq!(heard.get(), 3);
}

/// Tuple structs, enums with unit, tuple and struct variants, and generics:
/// a type parameter only a skipped field names needs no `Trace`.
#[derive(Trace)]
struct Pair(Cell<u32>, #[trace(skip)] Opaque, Stream<u32>);

#[derive(Trace)]
enum Route {
    Home,
    Page(Cell<u32>),
    Dialog {
        title: String,
        answer: Input<bool>,
        #[trace(skip)]
        cancel: Cell<u32>,
    },
}

#[derive(Trace)]
struct Tagged<T, U> {
    value: T,
    #[trace(skip)]
    tag: U,
}

/// A hold of a derived enum keeps what its current variant names, and
/// only that: a page no value names is collected, and so is the page the
/// route has moved on from. The pages reach I/O code through a side
/// channel, since what the build closure returns is a root.
#[test]
fn a_derived_enum_in_a_hold_roots_what_its_current_variant_names() {
    let pages_out = Rc::new(RefCell::new(None));
    let pages_in = pages_out.clone();
    let (mut graph, edge) = Runtime::build(move |b| {
        let (routes, routes_in) = b.input::<Route>();
        let route = routes.hold(b, Route::Home);
        let pages = [b.constant(1u32), b.constant(2u32), b.constant(3u32)];
        let (_, answer) = b.input::<bool>();
        let tagged = b.constant(Tagged {
            value: (pages[2], answer),
            tag: Opaque(0),
        });
        let (numbers, _numbers_in) = b.input::<u32>();
        let pair = b.constant(Pair(pages[2], Opaque(1), numbers));
        *pages_in.borrow_mut() = Some(pages);
        (routes_in, route, tagged, pair)
    });
    let (routes_in, route, tagged, pair) = edge.keep();
    graph.set_collection_policy(CollectionPolicy::Manual);
    let [one, two, three] = pages_out.borrow().expect("the build ran");
    graph.send(routes_in, Route::Page(one));
    graph.collect_garbage();
    assert_eq!(*graph.sample(one), 1);
    assert_eq!(
        graph.try_sample(two).err(),
        Some(TokenError::Stale),
        "named by nothing"
    );
    graph.send(routes_in, Route::Page(three));
    graph.collect_garbage();
    assert_eq!(
        graph.try_sample(one).err(),
        Some(TokenError::Stale),
        "moved on from"
    );
    assert_eq!(*graph.sample(three), 3, "a Tagged and a Pair name it too");
    let answer = graph.sample(tagged).value.1;
    let dialog = Route::Dialog {
        title: "sure?".to_string(),
        answer,
        cancel: three,
    };
    graph.send(routes_in, dialog);
    graph.collect_garbage();
    assert!(matches!(
        graph.sample(route),
        Route::Dialog { title, cancel, .. } if title == "sure?" && *cancel == three
    ));
    assert_eq!(graph.try_send(answer, true), Ok(()));
    let pair = graph.sample(pair);
    assert_eq!((pair.0, (pair.1).0), (three, 1));
    assert_eq!(graph.sample(tagged).tag.0, 0);
}

/// Shapes whose derived code must still compile without a warning: no
/// fields, only skipped fields, no variants, only unit variants, and a
/// field named like the method's own parameter.
#[derive(Trace)]
struct Unit;

#[derive(Trace)]
struct AllSkipped {
    #[trace(skip)]
    opaque: Opaque,
}

#[derive(Trace)]
enum Empty {}

#[derive(Trace)]
enum Flag {
    On,
    Off,
}

#[derive(Trace)]
struct Shadow {
    tracer: Cell<u32>,
}

#[test]
fn degenerate_shapes_derive_and_trace() {
    let (mut graph, edge) = Runtime::build(|b| {
        let unit = b.constant(Unit);
        let skipped = b.constant(AllSkipped { opaque: Opaque(5) });
        let empty = b.constant(None::<Empty>);
        let flags = b.constant([Flag::On, Flag::Off]);
        let seven = b.constant(7u32);
        let shadow = b.constant(Shadow { tracer: seven });
        (unit, skipped, empty, flags, shadow)
    });
    let cells = edge.keep();
    graph.collect_garbage();
    let (_, skipped, empty, flags, shadow) = cells;
    assert_eq!(graph.sample(skipped).opaque.0, 5);
    assert!(graph.sample(empty).is_none());
    assert!(matches!(graph.sample(flags), [Flag::On, Flag::Off]));
    let seven = graph.sample(shadow).tracer;
    assert_eq!(
        *graph.sample(seven),
        7,
        "traced through a field named `tracer`"
    );
}

type User = String;

/// RFD 6's chat room: the members' table, a cell in the graph, maps users
/// to the channels that reach their sockets, which hold no tokens and have
/// no `Trace` of their own.
#[derive(Trace)]
struct Members {
    #[trace(skip)]
    by_user: HashMap<User, mpsc::Sender<String>>,
}

/// RFD 6's chat room, built as the RFD writes it, in a `Threaded` graph,
/// with the one outbound listener attached before the sends; the members
/// are an in-place accumulator of a derived type. Driven directly here,
/// since `Remote` and `pump` are the I/O edge's stage.
#[test]
fn rfd_6_s_chat_room_members_derive_trace_and_route_every_line() {
    let (mut graph, edge) = Runtime::build_threaded(|b| {
        let (joins, joins_in) = b.input::<(User, mpsc::Sender<String>)>();
        let (messages, messages_in) = b.input::<(User, String)>();
        let members = joins.accumulate_mut(
            b,
            Members {
                by_user: HashMap::new(),
            },
            |(user, sender), m| {
                m.by_user.insert(user, sender);
            },
        );
        let outbound = messages
            .snapshot(members, |(user, line), m| {
                let recipients: Vec<_> = m.by_user.values().cloned().collect();
                (recipients, format!("{user}: {line}"))
            })
            .node(b);
        (joins_in, messages_in, outbound)
    });
    let (joins, messages, outbound) = edge.keep();
    graph
        .listen(outbound, |(recipients, text)| {
            for sender in recipients {
                let _ = sender.send(text.clone());
            }
        })
        .keep();
    graph.set_collect_after_every_transaction(true);
    let (ada, ada_inbox) = mpsc::channel();
    let (bo, bo_inbox) = mpsc::channel();
    graph.send(joins, ("ada".to_string(), ada));
    graph.send(messages, ("ada".to_string(), "alone".to_string()));
    graph.send(joins, ("bo".to_string(), bo));
    graph.send(messages, ("bo".to_string(), "hi".to_string()));
    assert_eq!(
        ada_inbox.try_iter().collect::<Vec<_>>(),
        ["ada: alone", "bo: hi"]
    );
    assert_eq!(bo_inbox.try_iter().collect::<Vec<_>>(), ["bo: hi"]);
}
