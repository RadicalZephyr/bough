//! The rollback probe's staging, under `--features stage`, with rollback
//! on and nothing failing: a node made at an instant waits for commit to
//! join an older node's dependents, and a switch that moves at the same
//! instant still finds its link.

#![cfg(feature = "stage")]

use std::cell::RefCell;
use std::rc::Rc;

use bough::{Runtime, Source};

/// A switch built at an instant at which its outer steps links its first
/// inner, an older cell, and that link waits for commit; the switch then
/// moves at the same instant, before commit, and the move takes the
/// waiting link rather than looking for the switch among the old inner's
/// dependents.
#[test]
fn a_switch_built_and_moved_at_one_instant_finds_its_waiting_link() {
    let (mut runtime, edge) = Runtime::build(|b| {
        let (picks, picks_in) = b.input::<bool>();
        let picks = picks.share(b);
        let (a, a_in) = b.input_cell(1i64);
        let (c, c_in) = b.input_cell(10i64);
        let chosen = picks.map(move |p| if p { c } else { a }).hold(b, a);
        let made = picks.construct(b, move |b, _| {
            let switched = chosen.switch_cell(b);
            b.anchor(switched)
        });
        // The closures capture the cells, which nothing else reaches.
        (picks_in, a_in, c_in, made, [a, c, chosen.switch_cell(b)])
    });
    runtime.set_rollback(true);
    let (picks_in, a_in, c_in, made, _cells) = edge.keep();
    let latest = Rc::new(RefCell::new(None));
    let slot = latest.clone();
    runtime
        .listen(made, move |cell| *slot.borrow_mut() = Some(cell))
        .keep();
    runtime.send(picks_in, true);
    let switched = latest.borrow_mut().take().expect("built").keep();
    assert_eq!(*runtime.sample(switched), 10);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let log = seen.clone();
    runtime
        .listen_steps(switched, move |n| log.borrow_mut().push(*n))
        .keep();
    runtime.send(c_in, 11);
    runtime.send(a_in, 2);
    assert_eq!(*seen.borrow(), [11], "it reads c, linked at commit, not a");
}
