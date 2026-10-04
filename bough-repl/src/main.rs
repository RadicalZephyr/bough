//! Reads commands from stdin, one per line, and prints what they print.

use std::io::{self, BufRead, Write};

use bough_repl::Repl;

fn main() {
    let mut repl = Repl::new();
    let stdout = io::stdout();
    for line in io::stdin().lock().lines() {
        let line = line.expect("bough-repl: stdin is readable");
        let mut out = stdout.lock();
        for printed in repl.run(&line) {
            writeln!(out, "{printed}").expect("bough-repl: stdout is writable");
        }
    }
}
