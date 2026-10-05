#!/bin/bash
# Prints the instruction counts of the regression and probe benchmarks,
# one line a benchmark: the label, the benchmark, its Callgrind
# instructions. Each configuration starts from no stored run, so nothing
# is compared and nothing fails.
#   ./measure.sh <checkout> <label> [cargo bench args]
# The table in instructions.tsv came from, at the repository's root:
#   git worktree add ../base 2b72e30, with bough-bench's src/lib.rs,
#     benches/probe.rs and its [[bench]] entry copied over: "unchanged";
#   then this checkout with no features, --features force, undo, stage,
#     each with and without rollback-off, and undo,stage.
dir=$1; label=$2; shift 2
cd "$dir"
rm -rf target/gungraun
for bench in regression probe; do
  cargo bench -q -p bough-bench --bench "$bench" "$@" 2>/dev/null | awk -v label="$label" '
    /^[a-z]+::/ {name=$0}
    /Instructions:/ {split($2, a, "|"); print label "\t" name "\t" a[1]}'
done
