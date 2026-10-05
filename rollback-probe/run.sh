#!/bin/sh
# Runs both experiments of the rollback probe and prints what each
# mechanism did: the native binary built with panic = "abort", and the
# wasm module under Node. Needs the wasm32-unknown-unknown target and node.
set -e
cd "$(dirname "$0")"
(cd abort && cargo build -q)
for mode in undo stage; do
  echo "== abort-probe $mode"
  RUST_BACKTRACE=0 ./abort/target/debug/abort-probe "$mode" 2>&1 && status=0 || status=$?
  echo "exit status $status"
done
(cd wasm && cargo build -q --release --target wasm32-unknown-unknown)
echo "== wasm-probe under node"
(cd wasm && node run.mjs)
