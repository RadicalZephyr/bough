# Verification of research-loop-shapes

Every value the research reports holds up. The corrections are about
citations, one overstated upstream claim, the reasoning behind the fixed
point's uniqueness, and the generator family. The review also turned up
one defect in `Denotational.hs` itself: `SwitchC` gives wrong steps when
its t0 comes after an outer step.

Method:

- I read the PDF (all 14 pages) and compared every equation with
  `Denotational.hs`.
- I rebuilt and reran `Shapes.hs` from a copy.
- I wrote `Verify.hs`, which has an imperative model of the slice that
  shares no code with the research. It also has a separate fixed-point
  solver and 41 new checks.
- I type-checked and ran the Bough sketch.
- I ran the reproduction and the reconstructed reductions on the real
  `sodium-rust` 2.1.3 from crates.io.

Nothing under `/home/user/bough`, `/home/user/rfd` or `/home/user/sodiumfrp`
changed. All three `git status` checks are clean, and no `.o` or `.hi` files
were written there.

Files in this directory:

| File | What it is |
|---|---|
| `Shapes.hs`, `shapes.out` | the research's program and its output, copied unchanged |
| `shapes.rerun.out` | my rerun of `Shapes.hs`, byte-identical to `shapes.out` |
| `Verify.hs`, `verify.out` | the new checks: 38 pass, and 3 fail as expected because they document the `SwitchC` defect |
| `sodium-rust-run/`, `sodium-rust.out` | the #52 program and its variants on `sodium-rust` 2.1.3 |
| `bough-sketch/` | a copy of the sketch, with its own `target` |

## 1. The semantics: the PDF against `Denotational.hs`

| Item | Verdict | Evidence |
|---|---|---|
| `T = [Int]`, children `t ++ [n]` are after t and before t's next sibling (p. 2) | confirmed | `[1] < [1,0] < [1,0,0] < [2]`, checked in C |
| `S a`, `C a` with increasing times (p. 3) | confirmed | same text in both |
| `at` uses steps with `tt < t`, so a step is visible only after its instant (p. 3) | confirmed | same text in both. This is where "before the instant" comes from. |
| Never, MapS, Snapshot, Filter, SwitchS, Execute, Updates, Value with `chopFront`, Split, Constant, Hold, MapC, Apply, SwitchC with `normalize`/`chopBack`, Sample (pp. 3–14) | confirmed | the equations match the `.hs` character for character, apart from the PDF typos in the next row |
| Merge equation (p. 5) | confirmed in `.hs`. The PDF has a typo. | The PDF writes `occs (Merge sa sb) = coalesce f …` with `f` unbound. The `.hs` has `occs (Merge sa sb f)`. |
| Other PDF slips | noted | The primitive list types `Sample :: Behavior a → T → a`. Merge's prose says a stream "can have simultaneous events", which revision 1.1 removed. Updates' prose names a `Coalesce` primitive that does not exist. None of these changes a result. |
| `normalize` compares with the outer `t0`, not scan's local `t0` | confirmed, harmless at later switches | the outer `coalesce (flip const)` removes the duplicate at a switch instant (C, E) |
| **SwitchC with t0 after an outer step** | **defect in both the PDF and the `.hs`** | See §6. `steps (SwitchC outer [3])`, where the outer switched to c2 at `[1]`, gives `('X',[([3],'c'),([1],'W'),([2],'X'),([3],'Y'),([4],'Z')])`. The list is not increasing. It has steps before t0. Its step at t0 carries the old inner's value `'c'`. |

## 2. Section numbers

| Claim | Verdict | Correction and evidence |
|---|---|---|
| "5.14" for Apply, "5.3" for Snapshot | **corrected** | The PDF has no numbered sections. Its headings are unnumbered and its figures are E.1–E.20. The numbers come from bevy-sodium's markdown rendering, `docs/reference/sodium/denotational-semantics.md`, where 5.3 is Snapshot and 5.14 is Apply; the research says so in its sources. Bough has not vendored a rendering yet: `bough-oracle/haskell` holds only the `.hs` files and the README, although RFD 1 promises one. Until it does, cite the PDF by heading and figure: Apply, p. 10–11, fig. E.15; Snapshot, p. 4, fig. E.3. |
| "The clamp reads max_health from before the instant (5.3)" | corrected (precision) | The rule is `at` (PDF p. 3; §4 of the rendering). Snapshot (5.3) inherits it. Cite both. |

## 3. Sources

| Claim | Verdict | Evidence |
|---|---|---|
| Records 0001 and 0002 are on `main` (`f682088`). Record 0003 and the experiments are only on `decision-0003-leverage` (PR #5, `8e5cccf`). Stage 1 is at `20dfa33`. | confirmed | `git log` in both checkouts. The experiments' path is `docs/decisions/experiments/src/…`, not `experiments/src/…`. `main`'s `lib.rs` has only a doc comment. |
| The stage 1 program: `heal.snapshot3(health, max, (cur+h).min(max)).hold(60)` | confirmed | `git show 20dfa33:…/0003-health-shield-frp.rs` |
| The GitHub API and the GitHub MCP tool refuse #52. WebFetch reaches it. | confirmed | MCP: "not configured for this session". API: 403. WebFetch: title, author, 2026-09-19, open, `2acb289`, 2.1.3. |
| The issue's code matches `0003-lift2-loop-bug.rs` line for line | unverifiable | WebFetch paraphrases. It says the code is included "verbatim" but does not show it. |
| The issue does not include the reduced programs | unverifiable, consistent | WebFetch mentions only the main program |
| The handoff says "four attempts to reduce it further stopped reproducing" and "merge is necessary; a third cell is not" | confirmed | `HANDOFF.md` lines 66–68 |
| The records flag exactly one loop shape as a correctness problem. Frame batching and the listener deadlock (R5) are not loop shapes. | confirmed | records 0002 and 0003, research note R5 and R6 |
| "RFD 7's fold law already tests" frame batching | **corrected** | The fold law covers one input slot's bursts between pumps. Frame batching is ruled out by "two slots are never simultaneous", because `pump` runs each pending slot as its own transaction, and by "never a frame batched into one transaction" (RFD 7, input slots and Hosts). |
| `TurnResolved` is one host message carrying heal, extra_max and damage. Under RFD 7 it is one unit. | confirmed | `0003-health-shield-bevy.rs` lines 23–69; RFD 7 Hosts, "every host message its own unit" |
| Arm A gives health 80 or 100 by observer order. It fires 3 times with states that never existed. | confirmed | record 0003, arm A output |
| "Arm B counts this as 'derived fired 2x'" | unverifiable as a recorded output | Record 0003 prints no arm B output. 2x is what `0003-health-shield-frp.rs`'s counter would print, and it prints 2x even with the bug. |

## 4. The shapes

I checked every table value three ways. The first is the research's
Haskell, rerun. The second is my imperative model, which walks each
instant with the values from before that instant. The third is my own
fixed-point solver over `Denotational.hs`. It agrees with the model on
4 drives × 3 schedules. The third schedule is new: a simultaneous
level-up and damage without a heal, a level-up of 0, and a heal far past
the maximum.

| Claim | Verdict | Evidence |
|---|---|---|
| Shape 1 program over the constructors, including `took` reading the held `shield` rather than the forward token | confirmed | matches `0003-lift2-loop-bug.rs` |
| Shape 1 values: max 200, shield 0, took −20, delta 80, health 100, fraction 0.5, effective 100 | confirmed | A |
| Health is 100, not 140, because the clamp reads the maximum from before the instant | confirmed | A |
| sodium-rust 2.1.3 rows: 100, 100, 60, 60 | confirmed | I ran it on 2.1.3 (`sodium-rust.out`). With intermediates kept alive it gives the same result. |
| "Lifting health together with shield stopped health stepping **at all**" | **corrected** | Health loses only its step at instants where both merge inputs fire, heal and damage together. On the long schedule, sodium-rust with the effective lift gives `[60,60,30,200,250,0,40]` against the oracle's `[60,100,70,200,250,0,40]`. It diverges at `[2]` and `[3]` and steps normally elsewhere. One instant at a time: heal+damage loses the step, and heal+level_up, damage+level_up, heal alone and damage alone do not. Record 0003, R6 and RFD 1 carry the same overstatement from the one-instant test. |
| Lifts step once per instant (no-glitch), at the union of their inputs' step times | confirmed | research checks, plus E (the apply check includes a step to an equal value) |
| Snapshot3 as two nested snapshots equals a snapshot of a lift2 pair cell | confirmed | research check |
| The four "row" checks | confirmed, but vacuous | Each row recomputes the same `sliceByFixpoint Full`. The research says so. They test nothing row-specific. |
| 2a (stage 1): health 100, max 200, fraction 0.5 | confirmed | research check, hand computation |
| 2b table, every cell, including the steps to equal values at `[1]`, `[3]` and `[6]` and health 200 at `[4]` | confirmed | A: `model Full longRun ==` the research's vectors |
| The Double quotients are exact, and the sketch's f32 equality is exact | confirmed | Every expected value is a correctly rounded quotient of the same integers. |
| 3b NoMaxRead: 140; long run 60,130,100,600,700,400,440 | confirmed | A |
| 3b NoMerge: 40; long run 60,30,0,0 at `[1]`, `[2]`, `[3]`, `[6]` | confirmed | A |
| 3b upstream column: NoMaxRead still reproduced, NoMerge did not | confirmed on real sodium-rust with the research's reconstructions | NoMaxRead with the effective lift gives 60 (oracle 140). NoMerge gives 40 in all rows. |
| "Two inputs: not reconstructable" | confirmed, with a finding | Both natural readings of a two-input version reproduce on 2.1.3: health 60 against 140. One drops level_up and max_health. The other also replaces shield's loop with a plain hold. So the issue's two-sink program differed in some way it does not describe, and the bug is less fragile than the handoff says. |
| 3c: no step at `[1]`; otherwise as in 2b. Fraction loses `[1]`, effective keeps it. | confirmed | A |
| 3c: the lazy knot does not return | confirmed | Under `ulimit -v 3000000` it runs out of memory after 12 s. |
| 3c is also a #52 shape | added | On sodium-rust with the effective lift, 3c gives 60 at the record instant. |
| Generator family: "X is itself a hold through its own loop" | **corrected** | X need not be looped. Two inputs, health through a loop, shield as a plain `hold`, a merge, and `lift2(health, shield)` still reproduce: 60 against 140. The generator should vary whether X is looped, not assume it. |

## 5. The fixed-point method

| Claim | Verdict | Evidence |
|---|---|---|
| `fixLoops` implements fact 2: the forward token is replaced with `Hold a (MkStream sts) []`, the start never steps, and the text is re-evaluated unchanged | confirmed | code read; my solver agrees on every slice run |
| Capped counter: 12 evaluations, the 11th right and the 12th confirming. This matches fact 2's 11 iterations. | confirmed | B: the first right iterate is the 11th |
| Evaluation counts: 2 for shape 1, 3 for 2b, 7 for NoMaxRead long, 3 for NoMerge long, 3 for 3c | confirmed | A |
| The fixed-point iteration equals the lazy knot wherever the knot terminates | confirmed for every shape here | research checks |
| "Every path around a loop passes through a hold, **so** a loop's step at t depends only on steps strictly before t" | **corrected** | A hold alone does not delay a dependency. The strictness comes from how the forward token is read: through `at` (`Snapshot`, `sample`), or at a child time (`split`, `defer`). A hold read back through `Updates` (Bough's `Cell::steps`) or `Value` (`steps_with_current`) satisfies the cycle rule as RFD 2 and the glossary state it, yet it depends on itself at the same instant. For `hold 0 (updates c)`, every step list is a fixed point; the iteration returns the least one, `(0,[])`. `hold 0 (map (+1) (value c))` has no fixed point: the iterates are `[0]1`, `[0]2`, `[0]3`, …, and the lazy knot runs out of memory. Uniqueness holds for every shape in this research, because each forward token is read only by `Snapshot`. The cycle rule itself may need tightening; that is a question for RFD 2. |
| "Reached within (number of step times + 1) evaluations" | **corrected** (wording) | This holds only if "step times" means the times at which the loop's feeding stream fires, the candidate times. It fails if it means the answer's steps. The parity loop `hold 0 (map fst (filter (even . snd) (snapshot ticks c (,))))` has 1 answer step and needs 4 evaluations: right at the 3rd, confirmed at the 4th. |
| `fixLoops` solves loops of one type only, and cell loops only | missing | 1. The slice's loops are all `Int`, so this is fine here. A generator needs mixed-type loops as a tuple or an existential. 2. Stream loops, which RFD 2 allows through `split` and `defer`, are not covered. A terminating stream loop through `defer` always has a filter on values, so the lazy knot can never compute one. C's countdown `s = merge input (filter (>0) (map pred (defer s)))` runs out of memory by knot. By fixed point it gives `[1]3 [1,0]2 [1,0,0]1 [2]1` in 4 evaluations. `solveS` in `Verify.hs` is the stream version: replay the previous iterate's events with `MkStream`, starting from none. |

## 6. Missing cases, now added in `Verify.hs` (all pass except the three documenting the defect)

Creation time (D):

- A hold keeps an event at t0 and drops earlier ones.
- `Value c t0` fires once at t0. If c steps at t0 it carries the post-step value. If c is quiet at t0 it carries the current value.
- `SwitchC` over a quiet constant inner still steps at creation.
- A `SwitchC` created in the instant its outer switches starts with the old inner's value from before the instant. Its first step, at t0, is the new inner's post-instant value.
- A loop whose hold is created at `[3]`, as in a construct, is solved by the fixed point. Its steps are `[3]1 … [6]4`.

The `SwitchC` defect:

- When the outer cell stepped before t0, `Denotational.hs` gives the right initial value but wrong steps (§1).
- `Updates` exposes a step at t0 with the old inner's value.
- The oracle cannot port `SwitchC` unchanged for a `switch_cell` built by a construct at a later instant.
- Candidate fix: take the outer's steps as `chopFront (steps c) t0` instead of `steps c`. It gives `('X',[3]Y [4]Z)`. It leaves figures E.16 and E.19 unchanged. It agrees with the `.hs` when the outer switches exactly at t0. It handles outer switches before and after t0.
- Adopting the fix is a semantics decision, not an oracle detail.

Child transactions (C):

- Split children are `[1,0] [1,1] [2,0]`.
- Two defers in one instant share child index `[1,0]`, and the merge keeps the left event. This is common-tests' `deferSimultaneous`.
- The parent instant runs before its child. This is `defer2`.
- A snapshot at `[1,0]` sees the step made at `[1]`.
- A hold on a split steps twice inside top-level instant `[1]`.
- A cell loop over split children counts `[1,0]1 [1,1]2 [1,2]3`.

Simultaneous events and RFD 1's corners (E):

- `merge` calls `f(left, right)`: `(-)` gives 17 at `[2]`.
- Merging a stream with itself calls `f(a, a)`.
- `switch_stream` uses the old stream at the switch instant.
- `switch_cell` uses the new cell's post-instant value.
- A hold steps to an equal value.

For the fixed tests: every shape here merges with `(+)`, which is
commutative. The #52 fixed test cannot catch an engine that swaps merge's
arguments. The generator should use a non-commutative combining function.

## 7. The Bough sketch

| Claim | Verdict | Evidence |
|---|---|---|
| It type-checks against the skeleton, and clippy is clean | confirmed | `cargo check --offline --tests` and `cargo clippy --offline --tests` on the copy |
| The four tests stop at the skeleton's `todo!()` | confirmed | all four panic at `graph.rs:44`, which is `Graph::build` |
| Build is transaction zero, `[0]`. `listen_steps` fires nothing at registration. Listeners run after commit. | confirmed | `graph.rs` doc comments. `run`'s instant counter is therefore right. |
| The expected vectors equal the oracle's | confirmed | A |

## Commands

```text
ghc -O0 -outputdir build -i/home/user/sodiumfrp/sodium/denotational Shapes.hs -o shapes && ./shapes > shapes.rerun.out   # exit 0, identical
ghc -O0 -outputdir build -i/home/user/sodiumfrp/sodium/denotational Verify.hs -o verify && ./verify > verify.out       # exit 0: 38 PASS, 3 expected FAIL
( ulimit -v 3000000; timeout 15 ./verify knot-defer )   # out of memory, 13 s
( ulimit -v 3000000; timeout 15 ./verify knot-value )   # out of memory, 9 s
( ulimit -v 3000000; timeout 15 ./shapes knot-3c )      # out of memory, 12 s
cd bough-sketch && cargo check --offline --tests && cargo clippy --offline --tests && cargo test --offline   # 4 panics at graph.rs:44
cd sodium-rust-run && cargo run -q > ../sodium-rust.out   # sodium-rust =2.1.3 from crates.io
```
