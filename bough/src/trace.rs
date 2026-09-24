//! `Trace`: how the collector finds tokens inside values (RFD 3).
//!
//! Every type held in a cell implements `Trace`. It is a safe trait: a wrong
//! implementation frees a node early, and the next use of a token naming it
//! fails the generation check; no memory is ever touched through a stale
//! token. `#[derive(Trace)]`, with the `derive` feature, writes one that
//! visits every field but those marked `#[trace(skip)]`.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::rc::Rc;
use alloc::string::String;
#[cfg(target_has_atomic = "ptr")]
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::marker::PhantomData;
#[cfg(feature = "std")]
use std::collections::{HashMap, HashSet};

use crate::token::{Cell, Input, Shared, State, Stream, TokenRef};

/// Visits the tokens a value holds.
pub struct Tracer {
    pub(crate) visited: Vec<crate::token::Token>,
}

impl Tracer {
    pub(crate) fn new() -> Self {
        Tracer {
            visited: Vec::new(),
        }
    }

    /// Records that the value being traced holds this token.
    pub fn visit(&mut self, token: &impl TokenRef) {
        self.visited.push(token.token());
    }
}

/// A value the collector can look inside (RFD 3).
///
/// A node is alive while a root reaches it, and what a stateful cell's
/// committed value names is what it reaches: a hold of a cell token keeps
/// that cell alive, a routing table of screens keeps every screen. So the
/// operations that persist a value, `hold`, `accumulate`,
/// `accumulate_mut`, `scan`, `constant` and `input_cell`, require `Trace`
/// of it, and so does the build closure's return value, which is traced
/// once for the permanent roots. A stream's events need nothing: every
/// slot is emptied before a collection, so an event roots nothing.
///
/// Implementations ship for the tokens, which visit themselves, for the
/// standard library's types and collections, and for tuples and arrays; a
/// foreign type that holds no tokens goes in a [`Leaf`]. A hand-written
/// implementation visits every token the value holds:
///
/// ```
/// use bough::{Cell, Trace, Tracer};
///
/// struct Panel {
///     title: String,
///     count: Cell<u32>,
/// }
///
/// impl Trace for Panel {
///     fn trace(&self, tracer: &mut Tracer) {
///         self.count.trace(tracer); // the title holds no token
///     }
/// }
/// ```
///
/// It is a safe trait. The obligation is real, but an implementation that
/// misses a token only lets the collector free its node early, and the
/// token's next use is a stale-token error: no memory is touched through a
/// stale token.
#[cfg_attr(
    feature = "derive",
    doc = r#"
With the `derive` feature, `#[derive(Trace)]` visits every field, and
`#[trace(skip)]` leaves out one that cannot hold tokens:

```
use std::sync::mpsc;

use bough::{Cell, Input, Trace};

#[derive(Trace)]
struct Screen {
    title: String,
    clicks_in: Input<()>,
    panels: Vec<Option<Cell<u32>>>,
    #[trace(skip)]
    log: mpsc::Sender<String>,
}
```

A field whose type has no `Trace` and no `#[trace(skip)]` does not
compile:

```compile_fail,E0277
use std::sync::mpsc;

use bough::Trace;

#[derive(Trace)]
struct Screen {
    log: mpsc::Sender<String>, // error: Sender<String> is not Trace
}
```

`skip` is the one attribute:

```compile_fail
use bough::Trace;

#[derive(Trace)]
struct Screen {
    #[trace(sometimes)] // error: unknown trace attribute
    title: String,
}
```
"#
)]
pub trait Trace {
    /// Calls [`Tracer::visit`] on every token this value holds, directly or
    /// inside its fields.
    fn trace(&self, tracer: &mut Tracer);
}

/// A value of a foreign type that holds no tokens, wrapped so that it can
/// live in a cell. The orphan rules forbid implementing `Trace` for another
/// crate's type, so a bare `tokio::sync::mpsc::Sender` cannot be a cell
/// value, but `Leaf<Sender>` can: it traces nothing and derefs to the value.
/// Inside a derived type, `#[trace(skip)]` on the field does the same job.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Leaf<T>(pub T);

impl<T> Trace for Leaf<T> {
    fn trace(&self, _tracer: &mut Tracer) {}
}

impl<T> core::ops::Deref for Leaf<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> core::ops::DerefMut for Leaf<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T> From<T> for Leaf<T> {
    fn from(value: T) -> Self {
        Leaf(value)
    }
}

impl<A> Trace for Stream<A> {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.visit(self);
    }
}
impl<A> Trace for Shared<A> {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.visit(self);
    }
}
impl<A> Trace for Cell<A> {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.visit(self);
    }
}
impl<A> Trace for State<A> {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.visit(self);
    }
}
impl<A> Trace for Input<A> {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.visit(self);
    }
}

macro_rules! leaf {
    ($($ty:ty),* $(,)?) => {
        $(impl Trace for $ty {
            fn trace(&self, _tracer: &mut Tracer) {}
        })*
    };
}
leaf!(
    (),
    bool,
    char,
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
    f32,
    f64,
    String,
    &'static str,
    core::time::Duration,
);
#[cfg(feature = "std")]
leaf!(std::time::Instant);

impl<T: ?Sized> Trace for PhantomData<T> {
    fn trace(&self, _tracer: &mut Tracer) {}
}
impl<T: Trace> Trace for Option<T> {
    fn trace(&self, tracer: &mut Tracer) {
        if let Some(value) = self {
            value.trace(tracer);
        }
    }
}
impl<T: Trace, E: Trace> Trace for Result<T, E> {
    fn trace(&self, tracer: &mut Tracer) {
        match self {
            Ok(value) => value.trace(tracer),
            Err(error) => error.trace(tracer),
        }
    }
}
impl<T: Trace + ?Sized> Trace for Box<T> {
    fn trace(&self, tracer: &mut Tracer) {
        (**self).trace(tracer);
    }
}
impl<T: Trace + ?Sized> Trace for Rc<T> {
    fn trace(&self, tracer: &mut Tracer) {
        (**self).trace(tracer);
    }
}
#[cfg(target_has_atomic = "ptr")]
impl<T: Trace + ?Sized> Trace for Arc<T> {
    fn trace(&self, tracer: &mut Tracer) {
        (**self).trace(tracer);
    }
}
impl<T: Trace> Trace for [T] {
    fn trace(&self, tracer: &mut Tracer) {
        for value in self {
            value.trace(tracer);
        }
    }
}
impl<T: Trace, const N: usize> Trace for [T; N] {
    fn trace(&self, tracer: &mut Tracer) {
        self.as_slice().trace(tracer);
    }
}
impl<T: Trace> Trace for Vec<T> {
    fn trace(&self, tracer: &mut Tracer) {
        self.as_slice().trace(tracer);
    }
}
impl<T: Trace> Trace for VecDeque<T> {
    fn trace(&self, tracer: &mut Tracer) {
        for value in self {
            value.trace(tracer);
        }
    }
}
#[cfg(feature = "std")]
impl<K: Trace, V: Trace, S> Trace for HashMap<K, V, S> {
    fn trace(&self, tracer: &mut Tracer) {
        for (key, value) in self {
            key.trace(tracer);
            value.trace(tracer);
        }
    }
}
impl<K: Trace, V: Trace> Trace for BTreeMap<K, V> {
    fn trace(&self, tracer: &mut Tracer) {
        for (key, value) in self {
            key.trace(tracer);
            value.trace(tracer);
        }
    }
}
#[cfg(feature = "std")]
impl<T: Trace, S> Trace for HashSet<T, S> {
    fn trace(&self, tracer: &mut Tracer) {
        for value in self {
            value.trace(tracer);
        }
    }
}
impl<T: Trace> Trace for BTreeSet<T> {
    fn trace(&self, tracer: &mut Tracer) {
        for value in self {
            value.trace(tracer);
        }
    }
}

macro_rules! tuples {
    ($(($($name:ident),+)),* $(,)?) => {
        $(impl<$($name: Trace),+> Trace for ($($name,)+) {
            #[allow(non_snake_case)]
            fn trace(&self, tracer: &mut Tracer) {
                let ($($name,)+) = self;
                $($name.trace(tracer);)+
            }
        })*
    };
}
tuples!(
    (A),
    (A, B),
    (A, B, C),
    (A, B, C, D),
    (A, B, C, D, E),
    (A, B, C, D, E, F)
);
