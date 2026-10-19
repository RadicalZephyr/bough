//! The GHC programs behind the engine's fixed tests still give the output
//! the tests quote.
//!
//! Several engine tests in `bough/tests` quote values GHC computed over the
//! vendored semantics. The programs are in `haskell/probes`, each beside the
//! output it gave when the tests were written. Each test here builds one
//! program and compares what it prints with that output, so every quoted
//! value can be derived again. Without GHC the tests follow the oracle's
//! rule: they panic with the install hint unless `BOUGH_ORACLE=skip` is set.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use bough_oracle::{compile_haskell, for_tests, haskell_directory};

/// The directory the oracle's own tests build into, so the oracle binary
/// `for_tests` wants is usually built already.
fn directory() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("bough-oracle")
}

/// Builds `probes/<program>.hs` and checks it prints `probes/<program>.out`.
#[track_caller]
fn reproduces(program: &str) {
    let Some(_oracle) = for_tests(directory()) else {
        return;
    };
    let name = format!("probe-{}", program.replace('/', "-"));
    let source = format!("probes/{program}.hs");
    let binary = compile_haskell(&directory(), &source, &name, &["-O0"])
        .unwrap_or_else(|error| panic!("cannot build {source}: {error}"));
    let output = Command::new(&binary)
        .output()
        .unwrap_or_else(|error| panic!("cannot run {}: {error}", binary.display()));
    assert!(
        output.status.success(),
        "{source} exited with {}:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let recorded_path = haskell_directory().join(format!("probes/{program}.out"));
    let recorded = fs::read_to_string(&recorded_path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", recorded_path.display()));
    let printed = String::from_utf8_lossy(&output.stdout);
    if printed != recorded {
        let first = printed
            .lines()
            .zip(recorded.lines())
            .position(|(now, then)| now != then)
            .unwrap_or_else(|| printed.lines().count().min(recorded.lines().count()));
        panic!(
            "{source} no longer prints {}: the first difference is at line {}\n\
             printed:  {:?}\nrecorded: {:?}",
            recorded_path.display(),
            first + 1,
            printed.lines().nth(first),
            recorded.lines().nth(first),
        );
    }
}

#[test]
fn the_loop_shapes_reproduce() {
    reproduces("loop-shapes/Shapes");
}

#[test]
fn the_stage_three_loops_reproduce() {
    reproduces("stage3/Stage3");
}

#[test]
fn the_stage_three_boundary_reproduces() {
    reproduces("stage3/Boundary");
}

#[test]
fn the_stage_three_slice_reproduces() {
    reproduces("stage3/Slice");
}

#[test]
fn the_children_reproduce() {
    reproduces("stage4/Stage4");
}

#[test]
fn the_switches_reproduce() {
    reproduces("stage5/Stage5");
}

#[test]
fn the_constructs_reproduce() {
    reproduces("stage6/Stage6");
}
