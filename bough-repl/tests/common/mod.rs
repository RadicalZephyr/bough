//! The scripted-input harness the completion tests share.

#![allow(dead_code)]

use bough_repl::Repl;

/// Runs each command and checks what it printed. Lines are compared as
/// sorted lists: two watches that step in one transaction print in the
/// order their listeners run, which RFD 2 says not to rely on.
pub fn transcript(repl: &mut Repl, steps: &[(&str, &[&str])]) {
    for (command, expected) in steps {
        let mut printed = repl.run(command);
        let mut expected: Vec<&str> = expected.to_vec();
        printed.sort();
        expected.sort();
        assert_eq!(printed, expected, "after `{command}`");
    }
}

/// The `graph` listing, in definition order.
pub fn listing(repl: &mut Repl) -> Vec<String> {
    repl.run("graph")
}
