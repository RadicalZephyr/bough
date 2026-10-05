# rollback-probe

What the rollback probe, in the RFD repository's
`notes/2026-10-05-rollback-probe.md`, ran outside the workspace's tests:
two experiments on targets where a panic can't be caught, and the
instruction counts behind its table of costs. The mechanisms themselves
are features of `bough`: `force`, `undo` (mechanism A) and `stage`
(mechanism B).

## Where a panic can't be caught

`./run.sh` runs both, and `results.txt` is its output.

- **`abort`** is a binary built with `panic = "abort"`. Under `undo`, the
  send that fails aborts the process, exit status 134, though the frames
  that would catch it are on the stack: there is nothing to unwind into.
  Under `stage`, the same failure as a construct closure's error comes
  back as a refusal, and the next send goes through.
- **`wasm`** is a module for `wasm32-unknown-unknown`, where a panic is a
  trap, run under Node. `stage_probe` returns 0, refused and recovered;
  `undo_probe` traps with `RuntimeError: unreachable`. The instance ran
  `stage_probe` again after the trap, which says only that this trap left
  nothing that call needed in a bad state, not that a trapped instance is
  safe to use.

## What the mechanisms cost

`instructions.tsv` holds Callgrind instruction counts from
`./measure.sh`, for the regression gate's four shapes and the probe's
two, in `bough-bench/benches/probe.rs`, under the unchanged spike and
each feature, with rollback on and off. The note turns them into its
table.
