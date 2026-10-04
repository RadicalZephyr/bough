//! Reads commands from stdin, one per line, and prints what they print.
//!
//! The main thread is the driver: it owns the runtime and handles one event
//! at a time, a line from the stdin thread or a wake from a tick's send,
//! which it answers with a pump. End of input ends the program.

use std::io::{self, BufRead};
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::task::{Wake, Waker};
use std::thread;

use bough_repl::Repl;

enum Event {
    Line(String),
    Wake,
    End,
}

/// The waker a tick's send wakes: an event on the driver's channel.
struct Wakeup(Sender<Event>);

impl Wake for Wakeup {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        let _ = self.0.send(Event::Wake);
    }
}

fn main() {
    let (events, inbox) = mpsc::channel();
    let mut repl = Repl::new();
    repl.set_waker(Waker::from(Arc::new(Wakeup(events.clone()))));
    thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if events.send(Event::Line(line)).is_err() {
                return;
            }
        }
        let _ = events.send(Event::End);
    });
    for event in inbox {
        let printed = match event {
            Event::Line(line) => repl.run(&line),
            Event::Wake => repl.pump(),
            Event::End => break,
        };
        for line in printed {
            println!("{line}");
        }
    }
}
