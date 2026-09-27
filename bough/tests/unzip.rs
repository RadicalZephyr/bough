//! `unzip`: a stream of pairs into two linear streams, without cloning
//! either half (RFD 4). It denotes two maps, one taking each half.

use std::cell::RefCell;
use std::rc::Rc;

use bough::{Anchored, Input, Runtime, Source};

/// A shared log and a closure that appends to it.
fn recorder<T: 'static>() -> (Rc<RefCell<Vec<T>>>, impl FnMut(T) + 'static) {
    let log = Rc::new(RefCell::new(Vec::new()));
    let writer = log.clone();
    (log, move |value| writer.borrow_mut().push(value))
}

/// A value with no `Clone`.
#[derive(Debug, PartialEq)]
struct Solo(u32);

#[test]
fn unzip_moves_each_half_into_its_own_stream() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let (firsts, seconds) = numbers.map(|n| (Solo(n), Solo(n * 10))).unzip(b);
        (numbers_in, firsts, seconds)
    });
    let (numbers_in, firsts, seconds) = edge.keep();
    let (first_seen, on_first) = recorder();
    let (second_seen, on_second) = recorder();
    graph.listen(firsts, on_first).keep();
    graph.listen(seconds, on_second).keep();
    graph.send(numbers_in, 1);
    assert_eq!(*first_seen.borrow(), [Solo(1)]);
    assert_eq!(*second_seen.borrow(), [Solo(10)], "in the same transaction");
    graph.send(numbers_in, 2);
    assert_eq!(*first_seen.borrow(), [Solo(1), Solo(2)]);
    assert_eq!(*second_seen.borrow(), [Solo(10), Solo(20)]);
}

/// Over a run of sends, some of which the pairs stream skips, each half
/// fires exactly when a map taking that half of the same pairs does, with
/// the same value.
#[test]
fn unzip_denotes_two_maps() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let pairs = numbers.filter(|n| n % 3 != 0).map(|n| (n, n * n)).share(b);
        let (firsts, seconds) = pairs.unzip(b);
        let fsts = pairs.map(|(first, _)| first).node(b);
        let snds = pairs.map(|(_, second)| second).node(b);
        (numbers_in, (firsts, seconds), (fsts, snds))
    });
    let (numbers_in, (firsts, seconds), (fsts, snds)) = edge.keep();
    let logs: Vec<_> = [firsts, seconds, fsts, snds]
        .into_iter()
        .map(|stream| {
            let (seen, on) = recorder::<(u32, u32)>();
            let mut on = on;
            let sends = Rc::new(RefCell::new(0u32));
            let at = sends.clone();
            graph.listen(stream, move |v| on((*at.borrow(), v))).keep();
            (seen, sends)
        })
        .collect();
    for n in 0..40 {
        for (_, sends) in &logs {
            *sends.borrow_mut() = n;
        }
        graph.send(numbers_in, n);
    }
    let [firsts, seconds, fsts, snds] = &logs[..] else {
        unreachable!()
    };
    assert_eq!(*firsts.0.borrow(), *fsts.0.borrow());
    assert_eq!(*seconds.0.borrow(), *snds.0.borrow());
    assert_eq!(firsts.0.borrow().len(), 26, "the sends the filter kept");
}

/// F59: one `construct` makes a screen and its input together. The screens
/// go into a hold that a `switch_stream` reads, and the inputs go out to
/// I/O code, anchored, which sends through one and sees the event come out
/// of the switch.
#[test]
fn a_screen_goes_into_a_hold_while_its_input_goes_out() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (opens, opens_in) = b.input::<u32>();
        let (idle, _idle_in) = b.input::<u32>();
        let made = opens.construct(b, |b, id: u32| {
            let (keys, keys_in) = b.input::<u32>();
            let screen = keys.map(move |key| id * 100 + key).node(b);
            (screen, b.anchor(keys_in))
        });
        let (screens, inputs) = made.unzip(b);
        let shown = screens.hold(b, idle).switch_stream(b);
        (opens_in, inputs, shown)
    });
    let (opens_in, inputs, shown) = edge.keep();
    let (received, on_input) = recorder::<Anchored<Input<u32>>>();
    let (seen, on_shown) = recorder();
    graph.listen(inputs, on_input).keep();
    graph.listen(shown, on_shown).keep();
    graph.send(opens_in, 7);
    let keys_in = received
        .borrow_mut()
        .pop()
        .expect("the screen's input came out");
    graph.send(*keys_in, 3);
    assert_eq!(*seen.borrow(), [703]);
}

/// Either half reaches the pairs node, and through it the chain and the
/// cells the chain samples, so one half lives on when the other goes. A
/// half nothing reaches is collected, and so are the rest once neither is
/// reached.
#[test]
fn unzip_nodes_live_while_either_half_is_reached() {
    let (mut graph, edge) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let (offsets, offsets_in) = b.input::<u32>();
        let offset = offsets.hold(b, 100u32);
        let (firsts, seconds) = numbers
            .snapshot(offset, |n, offset| (n + *offset, n))
            .unzip(b);
        (numbers_in, offsets_in, firsts, seconds)
    });
    let ((numbers_in, _offsets_in, firsts, _seconds), edge) = edge.into_parts();
    let numbers_in = graph.anchor(numbers_in).keep();
    let (seen, on) = recorder();
    let listener = graph.listen(firsts, on);
    let before = graph.live_nodes();
    drop(edge);
    graph.collect_garbage();
    assert_eq!(
        graph.live_nodes(),
        before - 1,
        "the second half went, and nothing else"
    );
    graph.send(numbers_in, 5);
    assert_eq!(
        *seen.borrow(),
        [105],
        "the first half still reads the offset"
    );
    drop(listener);
    graph.collect_garbage();
    assert_eq!(
        graph.live_nodes(),
        1,
        "only the input anchored for good is left"
    );
}
