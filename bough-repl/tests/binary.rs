//! The binary as a user runs it: commands on stdin, a real timer thread,
//! and the driver pumping when a tick's send wakes it.

use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

/// The values printed for `name`, in order.
fn values(stdout: &str, name: &str) -> Vec<i64> {
    let prefix = format!("{name} = ");
    stdout
        .lines()
        .filter_map(|line| line.strip_prefix(&prefix))
        .map(|value| value.parse().expect("an Int"))
        .collect()
}

#[test]
fn a_redefinition_under_a_real_timer_neither_restarts_nor_skips_it() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_bough-repl"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("the binary runs");
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(b"tick t 20\nwatch t\ndef out t\nwatch out\n")
        .unwrap();
    thread::sleep(Duration::from_millis(300));
    stdin.write_all(b"def out add t 1000\n").unwrap();
    thread::sleep(Duration::from_millis(300));
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();

    // The timer's count, 0 and then every send, with no gap and no restart.
    let t = values(&stdout, "t");
    assert!(t.len() > 3, "{stdout}");
    assert_eq!(t, (0..t.len() as i64).collect::<Vec<_>>(), "{stdout}");

    // `out` follows `t`, and from the redefinition on is `t + 1000`: its
    // count never goes back and never skips, and repeats once, at the
    // redefinition's own step.
    let out = values(&stdout, "out");
    let moved = out.iter().position(|&v| v >= 1000).expect("redefined");
    assert!(
        moved < out.len() - 1,
        "a tick after the redefinition: {stdout}"
    );
    let counts: Vec<i64> = out.iter().map(|v| v % 1000).collect();
    for (i, pair) in counts.windows(2).enumerate() {
        let step = if i + 1 == moved { 0 } else { 1 };
        assert_eq!(pair[1] - pair[0], step, "{stdout}");
    }
    assert!(out[moved..].iter().all(|&v| v >= 1000), "{stdout}");
}
