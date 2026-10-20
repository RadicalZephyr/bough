//! Reads lines from stdin and prints a running word count and the longest
//! line so far, after every line.

use portia::prelude::*;

fn main() -> std::io::Result<()> {
    let mut rt = Runtime::local();
    let (lines_in, summary) = rt.build(|b| {
        let (lines, lines_in) = b.input::<String>();
        let lines = lines.share(b); // two consumers below
        let words = lines.accumulate_mut(b, 0_usize, |line, n: &mut usize| {
            *n += line.split_whitespace().count();
        });
        let longest = lines.accumulate_mut(b, String::new(), |line, best: &mut String| {
            if line.chars().count() > best.chars().count() {
                *best = line;
            }
        });
        let summary = (words, longest).lift(b, |n, l| format!("{n} words, longest: {l:?}"));
        (lines_in, summary)
    });

    // Fires once now, with the initial values, then again on every step.
    let _listener = rt.listen_cell(summary, |s| println!("{s}"));
    for line in std::io::stdin().lines() {
        rt.send(lines_in, line?);
    }
    Ok(())
}
