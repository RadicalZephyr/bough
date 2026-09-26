//! What exists where, and what can cross threads, checked at compile time
//! (RFD 6, RFD 7).
//!
//! Each module below is one condition the RFDs name, written out here
//! rather than copied from the items' own `cfg`, and holds the rows for
//! the types that exist under it. This is library code, so the
//! `cargo check --target` that CI runs for every target checks it too: a
//! type missing where its row says it exists breaks the build on that
//! target, and so does a type that is `Send` where its row says it isn't,
//! or the reverse. Nothing here runs.
//!
//! | Type | Exists | `Send` |
//! | --- | --- | --- |
//! | `Runtime<Local>` | everywhere | no |
//! | `Input`, `Stream`, `Cell`, `State`, `Shared` | everywhere | yes, whatever they carry |
//! | `Listener`, `Anchor`, in either mode | everywhere | with pointer atomics |
//! | `Runtime<Threaded>` | with pointer atomics | yes |
//! | `InputSlot` | with a lock | `Sync`, since it lives in a `static` |
//! | `Remote` | with pointer atomics and a lock | yes |
//! | `Io`, `Owner` | with `std` and pointer atomics | no |
//!
//! A lock is `std` or `critical-section`. The Cortex-M0 has no pointer
//! atomics, so it has the first three rows, with guards that aren't `Send`,
//! and the slot with `critical-section`.
//!
//! Absence isn't checked: a type that exists where its row says it doesn't
//! compiles unnoticed. Today the code can't compile there anyway, since
//! there's no `Arc` without pointer atomics and no inbox without a lock.

/// Each type must be `Send`.
macro_rules! send {
    ($($t:ty),+ $(,)?) => {
        const _: () = {
            const fn check<T: Send + ?Sized>() {}
            $(check::<$t>();)+
        };
    };
}

/// Each type must be `Sync`. Only the slot's row uses it, so without a
/// lock it's unused.
#[cfg_attr(
    not(any(feature = "std", feature = "critical-section")),
    allow(unused_macros)
)]
macro_rules! sync {
    ($($t:ty),+ $(,)?) => {
        const _: () = {
            const fn check<T: Sync + ?Sized>() {}
            $(check::<$t>();)+
        };
    };
}

/// No type may be `Send`. A `Send` type has two impls of `AmbiguousIfSend`,
/// so naming its `item` is ambiguous and doesn't compile; any other type
/// has one. It's the trick `static_assertions` uses.
macro_rules! not_send {
    ($($t:ty),+ $(,)?) => {
        $(const _: fn() = || {
            trait AmbiguousIfSend<A> {
                fn item() {}
            }
            impl<T: ?Sized> AmbiguousIfSend<()> for T {}
            #[allow(dead_code)]
            struct IsSend;
            impl<T: ?Sized + Send> AmbiguousIfSend<IsSend> for T {}
            let _ = <$t as AmbiguousIfSend<_>>::item;
        };)+
    };
}

/// Everywhere, the Cortex-M0 included.
mod everywhere {
    use alloc::rc::Rc;

    use crate::{Cell, Input, Local, Runtime, Shared, State, Stream};

    not_send!(Runtime<Local>);
    // A token is an integer, whatever it carries.
    send!(
        Input<Rc<u8>>,
        Stream<Rc<u8>>,
        Cell<Rc<u8>>,
        State<Rc<u8>>,
        Shared<Rc<u8>>,
    );
}

/// Where the target has pointer atomics.
#[cfg(target_has_atomic = "ptr")]
mod with_atomics {
    use crate::{Anchor, Listener, Local, Runtime, Threaded};

    send!(
        Runtime<Threaded>,
        Listener<Local>,
        Anchor<Local>,
        Listener<Threaded>,
        Anchor<Threaded>,
    );
}

/// Where the target has no pointer atomics.
#[cfg(not(target_has_atomic = "ptr"))]
mod without_atomics {
    use crate::{Anchor, Listener, Local};

    not_send!(Listener<Local>, Anchor<Local>);
}

/// Where there's a lock: `std` or `critical-section`.
#[cfg(any(feature = "std", feature = "critical-section"))]
mod with_a_lock {
    use crate::InputSlot;

    sync!(InputSlot<u8>);
}

/// Where the target has pointer atomics and there's a lock.
#[cfg(all(
    target_has_atomic = "ptr",
    any(feature = "std", feature = "critical-section")
))]
mod with_atomics_and_a_lock {
    use crate::Remote;

    send!(Remote);
}

/// Where there's `std` and the target has pointer atomics.
#[cfg(all(feature = "std", target_has_atomic = "ptr"))]
mod with_std_and_atomics {
    use crate::{Io, Owner};

    not_send!(Io, Owner);
}
