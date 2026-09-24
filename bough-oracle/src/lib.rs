//! The Sodium denotational semantics (version 1.1) as an executable oracle,
//! run by GHC.
//!
//! The oracle is the vendored `haskell/Reactive/Sodium/Denotational.hs`,
//! unchanged, driven by a small Haskell program. A [`Program`] describes a
//! graph as the engine would build it: its inputs, one [`Definition`] per
//! node, the nodes to observe, and a schedule of sends, one entry per
//! external transaction. The Rust side prints it as one line in the syntax
//! of Haskell's derived `Read`; `haskell/Oracle.hs` checks it, interprets it
//! into terms of the semantics, and answers one line, which
//! [`Answer::parse`] reads:
//!
//! ```text
//! OK [[0,[[[1],6]]],[1,0,[[[1],1]]]]    a stream's events; a cell's value and steps
//! ERR node 1 (SMap): N 0 carries TList   a malformed program, or evaluating it failed
//! TIMEOUT                                longer than five seconds
//! ```
//!
//! [`Oracle::answer`] takes a program and returns the parsed answer or an
//! [`Error`].
//!
//! # Running it
//!
//! Every test that needs GHC is in `tests/oracle.rs`, one test binary, and
//! `cargo test` runs them. They need GHC with the HUnit package: `apt-get
//! install ghc libghc-hunit-dev`, or GHC through ghcup and then `cabal
//! install --lib HUnit`. `BOUGH_GHC` names a GHC other than the `ghc` on
//! the `PATH`.
//!
//! Each such test starts from [`for_tests`]. The first call builds
//! `Oracle.hs` into a directory under `CARGO_TARGET_TMPDIR`, once per
//! process and under a lock file, and GHC's recompilation check makes a
//! later build quick. Without GHC every such test panics with the install
//! hint. With `BOUGH_ORACLE=skip` each says that it skipped and returns
//! instead; continuous integration sets it, because GHC does not run there
//! (RFD 1).
//!
//! # The interpreter
//!
//! Top-level nodes are created at `[0]`, and a construct body's nodes at
//! its event's instant, which is the `t0` of every hold, `steps_with_current`,
//! `switch_cell`, `once`, `scan` and accumulator in it. The answer is cut to
//! the program's [`Window`]: from the first transaction for the engine
//! comparison, or everything for vectors whose events sit at `[0]`.
//!
//! Loops are computed by explicit fixed-point iteration, never by lazy
//! knot-tying, because the text does not terminate on a loop whose spine
//! depends on its values: `c = hold 0 (filter (<= 10) (snapshot ticks c
//! (+1)))` never returns (finding F1). Each loop node is bound to a concrete
//! term, starting from a cell that never steps and a stream that never
//! fires. Each round interprets the whole program again, and every loop
//! iterates in the same rounds, the top level's and those of each run of a
//! construct body alike, until every iterate reproduces itself. The fixed
//! point is the semantics only for loops whose back edge is a read before
//! the instant (`snapshot`, `gate`, `sample`, the selection of
//! `switch_stream`) or a child instant (`split`, `defer`); a back edge
//! through a steps view is a same-instant cycle, which programs for the
//! oracle must not contain (finding F3).
//!
//! A loop time is one loop's value before its first step, or its step or
//! event at one time. A round settles at least one more loop time of a
//! well-founded loop, so a loop needs about a round for each loop time its
//! answer chains through: the counter over n transactions needs n + 1
//! rounds, and a running sum over a split's children needs one per child.
//! The rounds allowed are 200, and two for every loop time any round's
//! state has held, which leaves a loop that settles room twice over,
//! however far it chains. They count the loop times held, not the size of
//! the largest state, since a loop whose one step moves an instant later
//! every round stays small however many rounds it needs. One that never
//! settles and holds the same few loop times answers `ERR`, naming a loop
//! that still changes. A loop that grows without end, such as a stream loop
//! through `defer` with no filter to stop it (finding F22), holds new loop
//! times every round and answers `TIMEOUT`, and so does a long enough
//! chain: each round costs more as the loops grow, and the counter takes
//! about a second at 500 transactions, and at 1000 it would need nine.
//! `accumulate` and `scan` tie knots of their own and take no rounds, so
//! they answer far longer runs than the same state written as a cell loop,
//! which denotes the same cell.
//!
//! # Two patches to the text
//!
//! Both live in `haskell/Oracle/Derived.hs`, in the derived layer; the
//! vendored file stays untouched. Each has a test in `OracleTests.hs` that
//! shows the text's answer and the patched one on the case it fixes, and one
//! that shows the twenty vectors of `sodium.hs` unchanged under both.
//!
//! - **F6, `switch_cell`:** `SwitchC (concrete (chopFront (steps c) t0))
//!   t0`, with `concrete (a, sts) = Hold a (MkStream sts) []`. The text's
//!   `SwitchC` scans the outer cell from its initial value, not from `t0`. A
//!   switch cell created at `[3]` over an outer that switched from `c1` to
//!   `c2` at `[1]` gets the steps `[([3],'a'),([1],'x')]`: a step to the
//!   deselected inner at its creation, then a step before it existed. When
//!   `c2` itself steps at `[3]`, the text moves that step to `[1]`, so even
//!   `sample` reads the wrong value. Java's `Cell.switchC` starts from the
//!   outer's value at creation, which is the chopped cell (research
//!   verification, point 2).
//! - **F7, `split`:** `MkStream (sortOn fst (occs (Split s)))`, a stable
//!   sort by time. The text concatenates each parent event's children in
//!   the order of the parents, which is time order unless the split is fed
//!   by its own children. There the children of `[0,0]` come after `[0,1]`,
//!   and a merge downstream sees two events at `[0,0,0]`, 100 and 10, where
//!   the engine gives one, 110 (the fidelity review's `SplitSort.hs`).
//!
//! # The pool
//!
//! An [`Oracle`] keeps idle processes behind a mutex, so parallel test
//! threads never share one. A process answers `TIMEOUT` after five seconds
//! and `ERR heap` past its heap limit, 1 GB, and lives on. A process that
//! dies is replaced, and the [`Error`] names the program it was answering,
//! with the end of its standard error and whatever it wrote of its answer.
//! One that stops answering is killed after [`Oracle::with_watchdog`]'s
//! limit, and one whose answer does not follow the protocol is stopped;
//! both are reported the same way.

#![warn(missing_docs)]

mod answer;
mod ghc;
mod program;

pub use answer::{Answer, Datum, MalformedAnswer, Observation};
pub use ghc::{
    Error, INSTALL_HINT, Oracle, Plan, build, compile_haskell, for_tests, ghc_command,
    haskell_directory, plan,
};
pub use program::{
    Body, BodyResult, Definition, Expression, Input, Program, Reference, Time, Type, Value, Window,
};
