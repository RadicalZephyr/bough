# The GHC programs behind the engine's fixed tests

Several of the engine's fixed tests in `bough/tests` quote values that GHC
computed over the vendored semantics, `Reactive/Sodium/Denotational.hs`,
unchanged. The programs that computed them are kept here, each beside the
output it gave when the tests were written, so a quoted value can always be
derived again. `tests/probes.rs` builds every program and checks it still
gives that output.

| Program | What it computes | Quoted by |
|---|---|---|
| `loop-shapes/Shapes.hs` | The sodium-rust#52 shape and its reductions, and the health-and-shield slice; `loop-shapes.md` explains each shape | `bough/tests/loop_shapes.rs` |
| `stage3/Stage3.hs`, `Boundary.hs`, `Slice.hs` | Loops: counters, accumulators read through loops, diamonds through loops, the slice's edge schedule | `bough/tests/loops.rs`, `bough/tests/loop_shapes.rs` |
| `stage4/Stage4.hs` | Child transactions: shared child indices, depth first, transaction zero's children | `bough/tests/children.rs` |
| `stage5/Stage5.hs` | Switches: the five `sodium.hs` switch vectors moved to `[k+1]`, and the switch probes | `bough/tests/switches.rs` |
| `stage6/Stage6.hs` | `construct`: nodes created at an instant, switches created inside a closure, the dynamic pattern | `bough/tests/construct.rs` |

Loops are computed by explicit fixed-point iteration, as the oracle does,
because the text does not terminate on a loop whose events depend on values.
Where a program uses the oracle's two patches, it says so: a switch cell
created after its outer cell stepped starts from the outer's value at its
creation (F6), and split's events are sorted by time (F7).

To run one by hand, from `bough-oracle/haskell`:

```sh
ghc -O0 -i. -outputdir /tmp/probe-objects -o /tmp/probe probes/stage4/Stage4.hs
/tmp/probe | diff - probes/stage4/Stage4.out
```

`Shapes.hs` also takes an argument, `knot-3c`, that runs one lazy knot the
research expected not to return.
