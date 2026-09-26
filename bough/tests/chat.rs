//! RFD 6's chat room, run against the engine with standard threads and
//! channels in place of tokio: a `Remote` per user, the routing table a
//! cell in the graph, and the one outbound listener attached before the
//! graph moves into the thread that drives it. The build closure is RFD 6's,
//! with `#[derive(Trace)]` and a standard `Sender`, whose `send` is
//! tokio's `try_send`.
#![cfg(all(feature = "derive", feature = "std"))]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Condvar, Mutex, mpsc};
use std::task::{Wake, Waker};
use std::thread;
use std::time::Duration;

use bough::{Runtime, Source, Trace};

type User = String;

#[derive(Trace)]
struct Members {
    #[trace(skip)]
    by_user: HashMap<User, mpsc::Sender<String>>,
}

/// The driver thread's waker.
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
    fn wait(&self) {
        let mut woken = self.woken.lock().unwrap();
        while !*woken {
            woken = self.ready.wait(woken).unwrap();
        }
        *woken = false;
    }
}

const USERS: usize = 4;
const LINES: usize = 50;

#[test]
fn rfd_6_s_chat_room_runs_over_remotes() {
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

    // The driver: the graph moves into a thread that pumps whenever a
    // remote send wakes it, as RFD 6's future does in a tokio task.
    let remote = graph.remote();
    let signal = Arc::new(Signal::default());
    let driving = signal.clone();
    let driver = thread::spawn(move || {
        graph.set_waker(Waker::from(driving.clone()));
        while !driving.stop.load(Ordering::SeqCst) {
            graph.pump();
            driving.wait();
        }
    });

    // Each connection: a thread with a remote of its own, which registers
    // its channel, waits until its own greeting comes back, which says it
    // is a member, and then, once every user is, sends its lines and reads
    // everyone's.
    let everyone = Arc::new(Barrier::new(USERS));
    let connections: Vec<_> = (0..USERS)
        .map(|k| {
            let (remote, everyone) = (remote.clone(), everyone.clone());
            thread::spawn(move || {
                let user = format!("user{k}");
                let (sender, inbox) = mpsc::channel::<String>();
                remote.send(joins, (user.clone(), sender));
                remote.send(messages, (user.clone(), "hello".to_string()));
                let greeting = format!("{user}: hello");
                while inbox.recv_timeout(Duration::from_secs(10)).unwrap() != greeting {}
                everyone.wait();
                for line in 0..LINES {
                    remote.send(messages, (user.clone(), format!("line {line}")));
                }
                let mut heard: HashMap<String, Vec<usize>> = HashMap::new();
                let mut count = 0;
                while count < USERS * LINES {
                    let text = inbox.recv_timeout(Duration::from_secs(10)).unwrap();
                    if let Some((from, line)) = text.split_once(": line ") {
                        heard
                            .entry(from.to_string())
                            .or_default()
                            .push(line.parse().unwrap());
                        count += 1;
                    }
                }
                heard
            })
        })
        .collect();
    for connection in connections {
        let heard = connection.join().unwrap();
        assert_eq!(heard.len(), USERS);
        for lines in heard.values() {
            assert_eq!(*lines, (0..LINES).collect::<Vec<_>>(), "each user's order");
        }
    }
    signal.stop.store(true, Ordering::SeqCst);
    Waker::from(signal.clone()).wake();
    driver.join().unwrap();
}
