// SPDX-License-Identifier: MPL-2.0

//! `Trace`: how the collector finds tokens inside values (RFD 3).
//!
//! Every type held in a cell implements `Trace`. It is a safe trait: a wrong
//! implementation frees a node early, and the next use of a token naming it
//! fails the generation check; no memory is ever touched through a stale
//! token. A derive lands with the memory-model increment.

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

use crate::token::{Cell, Input, Shared, Stream, TokenRef};

/// Visits the tokens a value holds.
pub struct Tracer {
    visited: Vec<crate::token::Token>,
}

impl Tracer {
    /// Records that the value being traced holds this token.
    pub fn visit(&mut self, token: &impl TokenRef) {
        todo!()
    }
}

/// A value the collector can look inside.
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
