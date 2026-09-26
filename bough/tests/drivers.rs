//! RFD 6's drivers, written as an integration would write them, and the
//! claim that integrations compose because none of them owns anything.
//!
//! - The thread driver RFD 6 says the core ships: a dedicated thread that
//!   builds the graph, hands back the edge and a `Remote`, and blocks on a
//!   condition variable until woken. It is here rather than in the core,
//!   because RFD 6 does not say how it stops or what it does when a
//!   transaction panics; this one stops when its handle is dropped.
//! - The future driver: each poll stores the task's waker, pumps, and
//!   returns pending. A minimal executor, one task on one thread, stands
//!   in for tokio.
//!
//! Each driver sits between `// ---- begin` and `// ---- end` markers, so
//! that its lines can be counted.
#![cfg(feature = "std")]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::{Future, poll_fn};
use std::pin::pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use bough::{InputSlot, Mode, Remote, RemoteSendError, Runtime, Source, Threaded};

// ---- begin thread driver

/// What a thread driver blocks on: a flag and a condition variable, made a
/// waker through `Wake`.
#[derive(Default)]
struct Signal {
    woken: Mutex<bool>,
    ready: Condvar,
    stop: AtomicBool,
}

impl Wake for Signal {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        *self.woken.lock().unwrap() = true;
        self.ready.notify_one();
    }
}

impl Signal {
    /// Blocks until woken, then clears the flag.
    fn wait(&self) {
        let mut woken = self.woken.lock().unwrap();
        while !*woken {
            woken = self.ready.wait(woken).unwrap();
        }
        *woken = false;
    }
}

/// The driver thread's handle: dropping it stops the thread and joins it.
struct Driver {
    signal: Arc<Signal>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Driver {
    fn drop(&mut self) {
        self.signal.stop.store(true, Ordering::SeqCst);
        Waker::from(self.signal.clone()).wake();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Runs `make` on a thread of its own, which keeps the graph and pumps it
/// whenever a remote send or a slot write wakes it, and hands back the
/// edge and a remote. The graph may be `Local`: it never leaves its thread,
/// and `make` attaches its listeners there.
fn spawn_driver<M, R>(
    make: impl FnOnce() -> (Runtime<M>, R) + Send + 'static,
) -> (R, Remote, Driver)
where
    M: Mode,
    R: Send + 'static,
{
    let signal = Arc::new(Signal::default());
    let (handoff, handed) = mpsc::sync_channel(1);
    let driving = signal.clone();
    let thread = thread::spawn(move || {
        let (mut graph, edge) = make();
        graph.set_waker(Waker::from(driving.clone()));
        handoff.send((edge, graph.remote())).unwrap();
        while !driving.stop.load(Ordering::SeqCst) {
            graph.pump();
            driving.wait();
        }
    });
    let (edge, remote) = handed.recv().unwrap();
    let thread = Some(thread);
    (edge, remote, Driver { signal, thread })
}

// ---- end thread driver

// ---- begin future driver

/// RFD 6's future driver: each poll stores the task's waker, pumps, and
/// returns pending, so every remote send and slot write wakes the task.
fn drive<M: Mode>(mut graph: Runtime<M>) -> impl Future<Output = ()> {
    poll_fn(move |cx| {
        graph.set_waker(cx.waker().clone());
        graph.pump();
        Poll::Pending
    })
}

// ---- end future driver

// ---- begin executor

/// A minimal executor in place of tokio: polls one task whenever its waker
/// is woken, until `done` says to stop.
fn run_until(task: impl Future<Output = ()>, done: impl Fn() -> bool) {
    let signal = Arc::new(Signal::default());
    let waker = Waker::from(signal.clone());
    let mut cx = Context::from_waker(&waker);
    let mut task = pin!(task);
    while task.as_mut().poll(&mut cx).is_pending() && !done() {
        signal.wait();
    }
}

// ---- end executor

/// The thread driver with a `Local` graph built on its thread: its
/// listener keeps an `Rc`, and reports through a channel. Producers on
/// other threads send through clones of the remote, and a slot too.
#[test]
fn the_thread_driver_builds_a_local_graph_and_pumps_when_woken() {
    static TICKS: InputSlot<u32> = InputSlot::new(|a, b| a + b);
    let (report, reports) = mpsc::channel();
    let (numbers_in, remote, driver) = spawn_driver(move || {
        let (mut graph, (numbers_in, total)) = Runtime::build(|b| {
            let (numbers, numbers_in) = b.input::<u32>();
            let (ticks, ticks_in) = b.input::<u32>();
            b.connect(ticks_in, &TICKS);
            let total = numbers.or_else(b, ticks).accumulate(b, 0u32, |n, t| t + n);
            (numbers_in, total)
        });
        let steps = Rc::new(RefCell::new(0));
        graph
            .listen_steps(total, move |t| {
                *steps.borrow_mut() += 1;
                report.send((*t, *steps.borrow())).unwrap();
            })
            .keep();
        (graph, numbers_in)
    });
    let producers: Vec<_> = (0..3)
        .map(|_| {
            let remote = remote.clone();
            thread::spawn(move || {
                for n in 1..=10 {
                    remote.send(numbers_in, n);
                }
            })
        })
        .collect();
    TICKS.send(1000);
    for producer in producers {
        producer.join().unwrap();
    }
    let mut last = (0, 0);
    while last.0 != 3 * 55 + 1000 {
        last = reports.recv_timeout(Duration::from_secs(10)).unwrap();
    }
    assert_eq!(
        last.1, 31,
        "thirty units and one slot, one transaction each"
    );
    drop(driver);
    assert_eq!(
        remote.try_send(numbers_in, 1),
        Err(RemoteSendError::GraphDropped),
        "the driver stopped and dropped the graph"
    );
}

/// The future driver on the minimal executor: the graph moves into the
/// task, a `Local` graph included, and producers on other threads wake it.
#[test]
fn the_future_driver_pumps_at_each_poll() {
    let (mut graph, (numbers_in, total)) = Runtime::build(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        (numbers_in, numbers.accumulate(b, 0u32, |n, t| t + n))
    });
    let seen = Rc::new(RefCell::new(0u32));
    let sink = seen.clone();
    graph
        .listen_cell(total, move |t| *sink.borrow_mut() = *t)
        .keep();
    let remote = graph.remote();
    let producer = thread::spawn(move || {
        for n in 1..=100 {
            remote.send(numbers_in, n);
            if n % 10 == 0 {
                thread::sleep(Duration::from_millis(1));
            }
        }
    });
    run_until(drive(graph), || *seen.borrow() == 5050);
    producer.join().unwrap();
    assert_eq!(*seen.borrow(), 5050);
}

/// RFD 6: integrations coexist because none of them owns anything. A
/// "GUI" thread owns a `Local` graph whose listener keeps non-`Send` state,
/// and drives it with the future driver; a second executor on another
/// thread runs a task that holds a remote, standing in for tokio's network
/// tasks; a plain thread holds another; and a slot takes a third producer's
/// writes. Every producer's events arrive, each in its own order.
#[test]
fn integrations_share_one_graph_through_remotes() {
    static SENSOR: InputSlot<Vec<(char, u32)>> = InputSlot::new(|mut a, b| {
        a.extend(b);
        a
    });
    const EACH: u32 = 200;
    let (mut graph, (events_in, events)) = Runtime::build(|b| {
        let (events, events_in) = b.input::<(char, u32)>();
        let (sensor, sensor_in) = b.input::<Vec<(char, u32)>>();
        b.connect(sensor_in, &SENSOR);
        let events = events.map(|e| vec![e]).or_else(b, sensor).node(b);
        (events_in, events)
    });
    let widgets: Rc<RefCell<BTreeMap<char, Vec<u32>>>> = Rc::default();
    let sink = widgets.clone();
    graph
        .listen(events, move |events| {
            for (who, n) in events {
                sink.borrow_mut().entry(who).or_default().push(n);
            }
        })
        .keep();
    let remote = graph.remote();
    // A second executor, on its own thread, whose task sends through a
    // remote each time a timer future of its own completes.
    let networked = {
        let remote = remote.clone();
        thread::spawn(move || {
            let mut next = 0;
            let task = poll_fn(move |cx| {
                remote.send(events_in, ('n', next));
                next += 1;
                if next == EACH {
                    return Poll::Ready(());
                }
                cx.waker().wake_by_ref();
                Poll::Pending
            });
            run_until(task, || false);
        })
    };
    let plain = {
        let remote = remote.clone();
        thread::spawn(move || {
            for n in 0..EACH {
                remote.transaction(move |tx| tx.send(events_in, ('p', n)));
            }
        })
    };
    let interrupt = thread::spawn(|| {
        for n in 0..EACH {
            SENSOR.send(vec![('s', n)]);
        }
    });
    let done = {
        let widgets = widgets.clone();
        move || {
            let widgets = widgets.borrow();
            ['n', 'p', 's']
                .iter()
                .all(|who| widgets.get(who).is_some_and(|v| v.len() == EACH as usize))
        }
    };
    run_until(drive(graph), done);
    for producer in [networked, plain, interrupt] {
        producer.join().unwrap();
    }
    for (who, seen) in widgets.borrow().iter() {
        assert_eq!(*seen, (0..EACH).collect::<Vec<_>>(), "{who}'s order");
    }
}

/// A `Threaded` graph is built on one thread, listened to there, and moved
/// with its remote's inbox into the thread that drives it. The guard
/// follows the thread that runs the transaction: graph code there is
/// refused, and the thread that built the graph queues.
#[test]
fn a_threaded_graph_moves_into_its_driver_thread_with_a_remote() {
    fn assert_send<T: Send>() {}
    assert_send::<Runtime<Threaded>>();
    let (mut graph, (numbers_in, remotes_in, found, total)) = Runtime::build_threaded(|b| {
        let (numbers, numbers_in) = b.input::<u32>();
        let (remotes, remotes_in) = b.input::<Remote>();
        let found = remotes
            .map(move |r| r.try_send(numbers_in, 1) == Err(RemoteSendError::InsideTransaction))
            .hold(b, false);
        let total = numbers.accumulate(b, 0u32, |n, t| t + n);
        (numbers_in, remotes_in, found, total)
    });
    let (report, reports) = mpsc::channel();
    let totals = report.clone();
    graph
        .listen_steps(total, move |t| totals.send(Some(*t)).unwrap())
        .keep();
    graph
        .listen_steps(found, move |inside| {
            report.send(if *inside { None } else { Some(0) }).unwrap()
        })
        .keep();
    let remote = graph.remote();
    let signal = Arc::new(Signal::default());
    let driving = signal.clone();
    let driver = thread::spawn(move || {
        graph.set_waker(Waker::from(driving.clone()));
        while !driving.stop.load(Ordering::SeqCst) {
            graph.pump();
            driving.wait();
        }
        *graph.sample(total)
    });
    for n in 1..=4 {
        remote.send(numbers_in, n);
    }
    remote.send(remotes_in, remote.clone());
    let heard: Vec<Option<u32>> = (0..5)
        .map(|_| reports.recv_timeout(Duration::from_secs(10)).unwrap())
        .collect();
    signal.stop.store(true, Ordering::SeqCst);
    Waker::from(signal.clone()).wake();
    assert_eq!(
        driver.join().unwrap(),
        10,
        "the refused send queued nothing"
    );
    assert_eq!(
        heard,
        [Some(1), Some(3), Some(6), Some(10), None],
        "the driver thread's graph code was refused"
    );
}
