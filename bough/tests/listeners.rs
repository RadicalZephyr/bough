//! Listeners and their handles (RFD 2, RFD 3, RFD 5): what they see, when
//! they run, how they stop, and what a panic in one does.

use std::any::Any;
use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use bough::{Listener, PoisonedError, Runtime, SendError, Source, TokenError, Trace, Tracer};

/// A shared log and a closure that appends to it.
fn recorder<T: 'static>() -> (Rc<RefCell<Vec<T>>>, impl FnMut(T) + 'static) {
    let log = Rc::new(RefCell::new(Vec::new()));
    let writer = log.clone();
    (log, move |v| writer.borrow_mut().push(v))
}

/// The message of a caught panic.
fn panic_text(result: Result<impl Sized, Box<dyn Any + Send>>) -> String {
    let payload = match result {
        Ok(_) => panic!("expected a panic"),
        Err(payload) => payload,
    };
    if let Some(text) = payload.downcast_ref::<&str>() {
        text.to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        String::new()
    }
}

#[test]
fn a_stream_listener_takes_each_event() {
    let (mut graph, (numbers_in, doubled)) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.map(|n| n * 2).node(b))
    });
    let (seen, on) = recorder();
    graph.listen(doubled, on).keep();
    graph.send(numbers_in, 1);
    graph.send(numbers_in, 2);
    assert_eq!(*seen.borrow(), [2, 4]);
}

#[test]
fn a_listener_on_an_input_sees_the_folded_event_of_a_coalescing_input() {
    let (mut graph, (words, words_in)) =
        Runtime::build(|b| b.input_coalescing(|a: String, b| a + &b));
    let (seen, on) = recorder();
    graph.listen(words, on).keep();
    graph.transaction(|tx| {
        tx.send(words_in, "bo".to_string());
        tx.send(words_in, "ugh".to_string());
    });
    assert_eq!(*seen.borrow(), ["bough".to_string()]);
}

#[test]
fn every_listener_of_a_shared_stream_gets_a_clone_in_registration_order() {
    let (mut graph, (words_in, words)) = Runtime::build(|b| {
        let (words, words_in) = b.input::<String>();
        (words_in, words.share(b))
    });
    let (seen, on) = recorder::<(u8, String)>();
    let on = Rc::new(RefCell::new(on));
    for id in 0..3u8 {
        let on = on.clone();
        graph
            .listen(words, move |w| (on.borrow_mut())((id, w)))
            .keep();
    }
    graph.send(words_in, "hi".to_string());
    let hi = || "hi".to_string();
    assert_eq!(*seen.borrow(), [(0, hi()), (1, hi()), (2, hi())]);
}

#[test]
fn a_listener_on_never_never_runs() {
    let (mut graph, (numbers_in, nothing)) = Runtime::build(|b| {
        let (_numbers, numbers_in) = b.input::<u32>();
        (numbers_in, b.never::<u32>())
    });
    let (seen, on) = recorder::<u32>();
    graph.listen(nothing, on).keep();
    graph.send(numbers_in, 1);
    assert!(seen.borrow().is_empty());
}

#[test]
fn listen_cell_fires_at_registration_and_on_every_step_including_to_an_equal_value() {
    let (mut graph, (level, level_in)) = Runtime::build(|b| b.input_cell(5u32));
    let (seen, mut on) = recorder();
    graph.listen_cell(level, move |v| on(*v)).keep();
    assert_eq!(*seen.borrow(), [5], "fires at registration");
    graph.send(level_in, 6);
    graph.send(level_in, 6);
    graph.send(level_in, 5);
    assert_eq!(*seen.borrow(), [5, 6, 6, 5]);
}

#[test]
fn listen_steps_does_not_fire_at_registration() {
    let (mut graph, (level, level_in)) = Runtime::build(|b| b.input_cell(5u32));
    let (seen, mut on) = recorder();
    graph.listen_steps(level, move |v| on(*v)).keep();
    assert!(seen.borrow().is_empty());
    graph.send(level_in, 5);
    assert_eq!(*seen.borrow(), [5], "a step to an equal value");
}

#[test]
fn a_listener_on_a_constant_fires_only_at_registration() {
    let (mut graph, (numbers_in, three)) = Runtime::build(|b| {
        let (_numbers, numbers_in) = b.input::<u32>();
        (numbers_in, b.constant(3u32))
    });
    let (seen, mut on) = recorder();
    graph.listen_cell(three, move |v| on(*v)).keep();
    graph.send(numbers_in, 1);
    assert_eq!(*seen.borrow(), [3]);
}

/// A cell value that logs its drop, so a test can see when commit replaced
/// it.
struct Loud(u32, Rc<RefCell<Vec<String>>>);

impl Drop for Loud {
    fn drop(&mut self) {
        self.1.borrow_mut().push(format!("drop {}", self.0));
    }
}

impl Trace for Loud {
    fn trace(&self, _tracer: &mut Tracer) {}
}

#[test]
fn listeners_run_after_commit_and_see_committed_values() {
    let log = Rc::new(RefCell::new(Vec::<String>::new()));
    let initial = Loud(0, log.clone());
    let (mut graph, (loud_in, ticks_in, held, ticks)) = Runtime::build(move |b| {
        let (loud, loud_in) = b.input::<Loud>();
        let held = loud.hold(b, initial);
        let (ticks, ticks_in) = b.input::<u32>();
        (loud_in, ticks_in, held, ticks)
    });
    let writer = log.clone();
    graph
        .listen_steps(held, move |v| {
            writer.borrow_mut().push(format!("saw {}", v.0))
        })
        .keep();
    let writer = log.clone();
    graph
        .listen(ticks, move |t| {
            writer.borrow_mut().push(format!("tick {t}"))
        })
        .keep();
    graph.transaction(|tx| {
        tx.send(ticks_in, 7);
        tx.send(loud_in, Loud(1, log.clone()));
    });
    // Commit replaced the old value before any listener ran, the input's
    // listener included, though the input fired at its send.
    assert_eq!(*log.borrow(), ["drop 0", "tick 7", "saw 1"]);
}

#[test]
fn a_marked_hold_that_did_not_step_stays_quiet() {
    // F4: a hold behind a filter that rejects is reached by marking and
    // does not step, so its cell listeners do not fire.
    let (mut graph, (numbers_in, big)) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.filter(|n| *n > 5).hold(b, 0u32))
    });
    let (cell_seen, mut on_cell) = recorder();
    graph.listen_cell(big, move |v| on_cell(*v)).keep();
    let (steps_seen, mut on_steps) = recorder();
    graph.listen_steps(big, move |v| on_steps(*v)).keep();
    #[cfg(feature = "statistics")]
    let before = graph.statistics();
    graph.send(numbers_in, 3);
    #[cfg(feature = "statistics")]
    {
        let after = graph.statistics();
        assert_eq!(after.ordered - before.ordered, 1, "the hold was marked");
        assert_eq!(after.evaluations - before.evaluations, 1, "and evaluated");
        assert_eq!(after.commits - before.commits, 0, "and did not step");
        assert_eq!(after.listener_calls - before.listener_calls, 0);
    }
    assert_eq!(*cell_seen.borrow(), [0]);
    assert!(steps_seen.borrow().is_empty());
    assert_eq!(*graph.sample(big), 0);
    graph.send(numbers_in, 9);
    assert_eq!(*cell_seen.borrow(), [0, 9]);
    assert_eq!(*steps_seen.borrow(), [9]);
}

#[test]
fn keep_keeps_a_listener_and_unlisten_or_dropping_the_handle_stops_one() {
    let (mut graph, (numbers_in, numbers)) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.share(b))
    });
    let (kept, on_kept) = recorder();
    let (unlistened, on_unlistened) = recorder();
    let (dropped, on_dropped) = recorder();
    graph.listen(numbers, on_kept).keep();
    let unlisten = graph.listen(numbers, on_unlistened);
    let handle = graph.listen(numbers, on_dropped);
    graph.send(numbers_in, 1);
    unlisten.unlisten();
    drop(handle);
    graph.send(numbers_in, 2);
    assert_eq!(*kept.borrow(), [1, 2]);
    assert_eq!(*unlistened.borrow(), [1]);
    assert_eq!(*dropped.borrow(), [1]);
}

#[test]
fn a_listener_dropped_inside_another_listener_stops_at_once() {
    let (mut graph, (numbers_in, numbers)) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.share(b))
    });
    let victim: Rc<RefCell<Option<Listener>>> = Rc::new(RefCell::new(None));
    let slot = victim.clone();
    graph
        .listen(numbers, move |n| {
            if n == 2 {
                slot.borrow_mut().take();
            }
        })
        .keep();
    let (seen, on) = recorder();
    *victim.borrow_mut() = Some(graph.listen(numbers, on));
    graph.send(numbers_in, 1);
    // The first listener drops the second in the same transaction, before
    // the second's turn.
    graph.send(numbers_in, 2);
    graph.send(numbers_in, 3);
    assert_eq!(*seen.borrow(), [1]);
}

#[test]
fn a_listener_may_drop_its_own_handle() {
    let (mut graph, (numbers_in, numbers)) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.share(b))
    });
    let own: Rc<RefCell<Option<Listener>>> = Rc::new(RefCell::new(None));
    let slot = own.clone();
    let (seen, mut on) = recorder();
    *own.borrow_mut() = Some(graph.listen(numbers, move |n| {
        on(n);
        slot.borrow_mut().take();
    }));
    graph.send(numbers_in, 1);
    graph.send(numbers_in, 2);
    assert_eq!(*seen.borrow(), [1]);
}

/// Every later call on a poisoned graph fails.
fn assert_poisoned(graph: &mut Runtime, numbers_in: bough::Input<u32>, level: bough::Cell<u32>) {
    assert_eq!(graph.try_send(numbers_in, 9), Err(SendError::Poisoned));
    assert_eq!(graph.try_transaction(|_| ()), Err(PoisonedError));
    assert_eq!(graph.try_sample(level).err(), Some(TokenError::Poisoned));
    assert_eq!(
        graph.try_listen_cell(level, |_| ()).err(),
        Some(TokenError::Poisoned)
    );
    assert_eq!(
        graph.try_listen_steps(level, |_| ()).err(),
        Some(TokenError::Poisoned)
    );
    assert!(
        panic_text(catch_unwind(AssertUnwindSafe(|| graph.send(numbers_in, 9))))
            .contains("poisoned")
    );
    assert!(
        panic_text(catch_unwind(AssertUnwindSafe(|| *graph.sample(level)))).contains("poisoned")
    );
}

#[test]
fn a_panic_in_a_listener_poisons_the_graph() {
    let (mut graph, (numbers_in, level)) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.hold(b, 0u32))
    });
    graph
        .listen_steps(level, |v| {
            if *v == 2 {
                panic!("listener");
            }
        })
        .keep();
    graph.send(numbers_in, 1);
    let result = catch_unwind(AssertUnwindSafe(|| graph.send(numbers_in, 2)));
    assert_eq!(panic_text(result), "listener");
    assert_poisoned(&mut graph, numbers_in, level);
}

#[test]
fn a_panic_in_a_function_of_the_graph_poisons_it() {
    let (mut graph, (numbers_in, level)) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let level = numbers
            .map(|n| if n == 2 { panic!("user function") } else { n })
            .hold(b, 0u32);
        (numbers_in, level)
    });
    graph.send(numbers_in, 1);
    let result = catch_unwind(AssertUnwindSafe(|| graph.send(numbers_in, 2)));
    assert_eq!(panic_text(result), "user function");
    assert_poisoned(&mut graph, numbers_in, level);
}

#[test]
fn a_panic_in_a_listener_at_registration_leaves_the_graph_usable() {
    let (mut graph, (level, level_in)) = Runtime::build(|b| b.input_cell(1u32));
    let result = catch_unwind(AssertUnwindSafe(|| {
        graph.listen_cell(level, |_| panic!("at registration"))
    }));
    assert_eq!(panic_text(result), "at registration");
    assert_eq!(graph.try_send(level_in, 2), Ok(()));
    assert_eq!(*graph.sample(level), 2);
}

#[test]
fn a_foreign_token_is_an_error_from_the_listen_and_sample_entries() {
    let (mut graph, _) = Runtime::build(|b| b.input::<u32>().1);
    let (_other, (stream, shared, level)) = Runtime::build(|b| {
        let (numbers, _in) = b.input::<u32>();
        let shared = b.input::<u32>().0.share(b);
        (numbers, shared, b.constant(1u32))
    });
    assert_eq!(
        graph.try_listen(stream, |_| ()).err(),
        Some(TokenError::ForeignGraph)
    );
    assert_eq!(
        graph.try_listen_cell(level, |_| ()).err(),
        Some(TokenError::ForeignGraph)
    );
    assert_eq!(
        graph.try_listen_steps(level, |_| ()).err(),
        Some(TokenError::ForeignGraph)
    );
    assert_eq!(
        graph.try_sample(level).err(),
        Some(TokenError::ForeignGraph)
    );
    let result = catch_unwind(AssertUnwindSafe(|| graph.listen(shared, |_| ())));
    assert!(panic_text(result).contains("a token from another graph"));
    let result = catch_unwind(AssertUnwindSafe(|| *graph.sample(level)));
    assert!(panic_text(result).contains("a token from another graph"));
}

#[test]
fn listeners_of_a_threaded_graph_run_on_the_driving_thread() {
    use std::sync::{Arc, Mutex};
    let (mut graph, (numbers_in, numbers, total)) = Runtime::build_threaded(|b| {
        let (numbers, numbers_in) = b.input::<u64>();
        let numbers = numbers.share(b);
        let total = numbers.map(|n| n * 10).hold(b, 0u64);
        (numbers_in, numbers, total)
    });
    let seen = Arc::new(Mutex::new(Vec::new()));
    let writer = seen.clone();
    graph
        .listen(numbers, move |n| writer.lock().unwrap().push(n))
        .keep();
    let writer = seen.clone();
    graph
        .listen_cell(total, move |t| writer.lock().unwrap().push(*t))
        .keep();
    graph.send(numbers_in, 4);
    assert_eq!(*seen.lock().unwrap(), [0, 4, 40]);
}
