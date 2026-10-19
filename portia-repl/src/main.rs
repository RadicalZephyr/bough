//! Reads commands from stdin, one per line, and prints what they print.
//!
//! The main thread is the driver: it owns the runtime and handles one event
//! at a time, a line from the editor thread or a wake from a tick's send,
//! which it answers with a pump. End of input ends the program.
//!
//! On a terminal, the editor gives line editing and history, and what the
//! driver prints goes above the prompt, so a tick's watch lines never break
//! into the line being typed. Piped, it reads plain lines and prints no
//! prompt, so a script's output is just what the commands print.

use rustyline::error::ReadlineError;
use rustyline::{DefaultEditor, ExternalPrinter};
use std::io::{self, BufRead, IsTerminal};
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::task::{Wake, Waker};
use std::thread;

use portia_repl::Repl;

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
    let mut printer = None;
    if io::stdin().is_terminal() {
        let mut editor = DefaultEditor::new().expect("an editor over stdin");
        // An unsupported terminal has no printer; then we print plainly.
        printer = editor.create_external_printer().ok();
        thread::spawn(move || {
            loop {
                match editor.readline("> ") {
                    Ok(line) => {
                        let _ = editor.add_history_entry(&line);
                        if events.send(Event::Line(line)).is_err() {
                            return;
                        }
                    }
                    // Ctrl-C drops the line being typed, not the running graph.
                    Err(ReadlineError::Interrupted) => continue,
                    Err(_) => break,
                }
            }
            let _ = events.send(Event::End);
        });
    } else {
        // Not through the editor, which prints its prompt to stdout under
        // some values of TERM even when stdin is a pipe.
        thread::spawn(move || {
            for line in io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                if events.send(Event::Line(line)).is_err() {
                    return;
                }
            }
            let _ = events.send(Event::End);
        });
    }
    for event in inbox {
        let printed = match event {
            Event::Line(line) => repl.run(&line),
            Event::Wake => repl.pump(),
            Event::End => break,
        };
        for line in printed {
            match &mut printer {
                // Between two readlines the printer writes the line as is,
                // and the next prompt clears a line with no newline.
                Some(printer) => printer
                    .print(format!("{line}\n"))
                    .expect("printing above the prompt"),
                None => println!("{line}"),
            }
        }
    }
}
