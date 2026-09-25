# Loop shapes for the oracle's fixed tests

The exact programs behind two promises. RFD 1: the `lift2` shape filed as
`sodium-rust#52` is a fixed test written against the specification. RFD 7:
the health-and-shield slice is tested in the oracle long before the Bevy
adapter exists. Each shape below has the program in Sodium terms, the
program over the constructors of `Denotational.hs`, an input schedule, the
expected outputs from GHC, and a sketch in Bough's API.

Every expected value here came out of GHC 9.4.7 running `Shapes.hs`. That
file imports `Reactive.Sodium.Denotational` unchanged. It computes every
loop twice, once by explicit fixed-point iteration and once by lazy
knot-tying, and checks that the two agree. The one exception is shape 3c,
where the knot does not return.

## Sources, and what was actually read

- **bevy-sodium.** `main` (`f682088`) has only records 0001 and 0002, and
  `experiments/src/` holds only an empty `lib.rs`. Record 0003 and the three
  experiments live on branch `decision-0003-leverage` (PR #5, unmerged, at
  `8e5cccf`). I checked that branch out as a worktree at
  `/home/user/radicalzephyr/bevy-sodium-0003`. Files read:
  `records/0003-order-independence-is-the-value-proposition.md`,
  `experiments/src/bin/0003-lift2-loop-bug.rs`, `0003-health-shield-frp.rs`
  (stage 2, plus stage 1 from commit `20dfa33`), `0003-health-shield-bevy.rs`,
  record 0002, `HANDOFF.md` from `main`, and the vendored
  `docs/reference/sodium/denotational-semantics.md`, which is where the
  section numbers come from.
- **sodium-rust#52.** WebFetch worked. The GitHub API and the GitHub MCP tool
  both refused, because the repo is not enabled for this session. WebFetch
  goes through a summarizing model, so its prose is paraphrase. Its Rust
  block matches `0003-lift2-loop-bug.rs` line for line. What the issue says,
  as fetched: it is open, filed by RadicalZephyr on 2026-09-19, and reproduces
  on 2.1.3 and on `main` at `2acb289`. The title is "lift2 on a CellLoop-held
  cell stops it updating when its drive stream reaches the other cell through
  a merge". It reports these delta-debugging results: the `merge` is
  necessary; reading a third cell is not ("removing the third cell from the
  snapshot still causes divergence"); a two-input version with the same
  merge-and-loop shape behaves correctly. The issue does not include the
  reduced programs. Shape 3's reductions are reconstructed from that prose.
- **Research note** `2026-09-23-frp-host-embedding-requirements.md`, R6.

## The fixed-point method (fact 2, as implemented)

`fixLoops n def` solves n loops of the same type. `def` is the program
text, with the n forward tokens as parameters. Each forward token becomes
`Hold a (MkStream sts) []`, a concrete cell that replays the previous
iterate's steps. The first iterate is each loop's own initial value, never
stepping. The program text is re-evaluated unchanged until the loops' steps
reproduce themselves. Every other read of a looped cell reads the held cell
itself, as the Rust does. So `took` reads `shield`, not
`shield_loop.cell()`.

A self-reproducing iterate is the answer, not merely an answer. Every path
around a loop passes through a hold, so a loop's step at time `t` depends
only on the loop's steps strictly before `t`: `Snapshot` reads `at c t`,
and `at` filters `tt < t`. Over the finitely many times in a finite schedule,
induction makes the fixed point unique. It is reached within (number of step
times + 1) evaluations, and often sooner, because clamps and saturation make
values independent of early errors.

The count reported is the number of evaluations of the program text,
including the last one, which confirms the fixed point. On fact 1's capped
counter, `fixLoops` needs 12 evaluations: the 11th is right, and the 12th
reproduces it. That is the same as fact 2's "11 iterations".

The lazy knot is `s = slice drive inputs (loopsOf s)`. It terminates on
every shape here whose event spine does not depend on a loop's values,
which is all of them except 3c. `steps` is not memoized in `Denotational.hs`,
so the knot recomputes; for these schedules that costs nothing.

## Shape 1: sodium-rust#52

A cell held through a loop, health, is driven by a stream that reads
another looped cell, shield, through a merge. Lifting health together with
shield stopped health stepping at all in sodium-rust 2.1.3. Health stayed at
60, the hold's initial value. `lift2` is `Apply` (5.14), a pure function of
two cells, so no lift can change health.

### In Sodium terms (the reproduction, verbatim in substance)

```text
heal, damage, level_up : StreamSink<u32>
max_health = hold 100 (snapshot level_up max_health (\e c -> c + e))       -- CellLoop
shield     = hold 30  (snapshot damage shield (\d s -> s -| d))             -- CellLoop, -| = saturating_sub
healed     = map (as i64) heal
took       = snapshot damage shield (\d s -> -(d -| s))
delta      = merge healed took (+)
health     = hold 60 (snapshot3 delta health max_health
                        (\d cur max -> clamp 0 max (cur + d)))              -- CellLoop
row 0: nothing else
row 1: fraction  = lift2 max_health health (\m h -> h / m)
row 2: effective = lift2 health shield (+)
row 3: both
one transaction: heal 100, level_up 100, damage 50
sodium-rust 2.1.3: health 100, 100, 60, 60 (rows 0 to 3). Expected: 100 in every row.
```

### Over the Denotational constructors (`Shapes.hs`, `slice Full`)

```haskell
slice drive Inputs {..} ~[maxFwd, shieldFwd, healthFwd] = Slice {..}
  where
    maxHealth = Hold 100 (Snapshot (\e c -> c + e) (MkStream levelUp) maxFwd) [0]
    shield    = Hold 30 (Snapshot (\d s -> s `satSub` d) (MkStream damage) shieldFwd) [0]
    healed    = MapS id (MkStream heal)
    took      = Snapshot (\d s -> negate (d `satSub` s)) (MkStream damage) shield
    delta     = Merge healed took (+)
    health    = Hold 60 (Snapshot (\(d, cur) m -> clampTo m (cur + d))
                                  (Snapshot (,) delta healthFwd) maxHealth) [0]
    fraction  = Apply (MapC (\m h -> fromIntegral h / fromIntegral m) maxHealth) health
    effective = Apply (MapC (+) health) shield
```

`snapshot3` is not a constructor, so it is written as two nested
`Snapshot`s. `FullPairCell` writes it instead as a snapshot of
`Apply (MapC (,) healthFwd) maxHealth`. GHC checks that the two agree on
both schedules. Build is at `[0]`; transaction k is `[k]`.

### Schedule and expected outputs (record 0003's instant)

Initial state: health 60, max 100, shield 30. At `[1]`: heal 100,
level_up 100, damage 50.

| | initial | `[1]` |
|---|---|---|
| max_health | 100 | 200 |
| shield | 30 | 0 |
| took (event) | | -20 |
| delta (event) | | 80 |
| **health** | 60 | **100** |
| fraction | 0.6 | 0.5 |
| effective | 90 | 100 |

Health is 100 in all four rows. Two things the test pins down:

- The clamp reads max_health from before the instant (5.3). Health is
  `clamp 0 100 (60 + 80) = 100`, not 140, even though max_health steps to
  200 in the same instant. Took likewise reads the shield from before the
  instant, so 30 is absorbed.
- Apply's no-glitch rule (5.14) applies. Fraction and effective each step
  exactly once at `[1]`, although both of their inputs step there. Arm B
  counts this as "derived fired 2x"; arm A fired 3 times with states that
  never existed.

For comparison, Bevy arm A gives health 80 or 100 depending on observer
order. The oracle says 100.

The oracle computes health the same way in every row, because the lifts
are separate definitions. The fixed test's value is against the engine,
and each row is its own graph. Two evaluations reach the fixed point. The
lazy knot terminates and agrees.

### In Bough (excerpt, the `Drive::Full` arm; the whole file `bough-sketch/src/lib.rs` type-checks against the skeleton)

```rust
pub fn slice(b: &mut Build, drive: Drive, lifts: Lifts) -> Slice {
    let (heal_s, heal) = b.input::<u32>();
    let (damage_s, damage) = b.input::<u32>();
    let (level_up_s, level_up) = b.input::<u32>();
    let damage_s = damage_s.share(b);            // two consumers: shield and took

    let (max_fwd, max_loop) = b.cell_loop::<u32>();
    let max_health = level_up_s.snapshot(max_fwd, |e, c| c + e).hold(b, 100);
    max_loop.close(b, max_health);

    let (shield_fwd, shield_loop) = b.cell_loop::<u32>();
    let shield = damage_s.snapshot(shield_fwd, |d, s| s.saturating_sub(d)).hold(b, 30);
    shield_loop.close(b, shield);

    let healed = heal_s.map(|h| h as i64);
    let took = damage_s.snapshot(shield, |d, s| -(d.saturating_sub(*s) as i64));

    let (health_fwd, health_loop) = b.cell_loop::<u32>();
    let health = healed
        .merge(b, took, |x, y| x + y)
        .snapshot(health_fwd, |d, cur| (d, *cur))                 // snapshot3 is two adapters,
        .snapshot(max_health, |(d, cur), max| clamp(d, cur, *max)) // fused into the hold's node
        .hold(b, 60);
    health_loop.close(b, health);

    let fraction = matches!(lifts, Lifts::Fraction | Lifts::Both)
        .then(|| (max_health, health).lift(b, |m, h| *h as f32 / *m as f32));
    let effective = matches!(lifts, Lifts::Effective | Lifts::Both)
        .then(|| (health, shield).lift(b, |h, s| h + s));
    Slice { heal, damage, level_up, max_health, shield, health, fraction, effective }
}

// the fixed test: one graph per row, one transaction for the instant
for lifts in [Lifts::None, Lifts::Fraction, Lifts::Effective, Lifts::Both] {
    let (mut graph, edge) = Graph::build(|b| slice(b, Drive::Full, lifts));
    let steps = run(&mut graph, &edge, &RECORD_0003, &[edge.max_health, edge.shield, edge.health]);
    assert_eq!(*steps[2].borrow(), [(1, 100)]);            // listen_steps on health
    assert_eq!(*graph.sample(edge.health), 100);
}
```

`run` registers `listen_steps` on each cell and records `(instant, value)`.
Each instant is one `graph.transaction` with that instant's sends. The
cycle rule is satisfied: every path back to each forward token passes
through a hold. `max_health` and `shield` could also be written as
`accumulate`, which is the same accumulator. The fixed test should keep
the explicit `cell_loop`, because that is the shape that failed. The
generator can cover the `accumulate` spelling.

## Shape 2: the health-and-shield slice (RFD 7)

### 2a, stage 1 (commit `20dfa33`): health and max_health only

```text
max_health = hold 100 (snapshot level_up max_health (\e c -> c + e))
health     = hold 60  (snapshot3 heal health max_health (\h cur max -> min max (cur + h)))
fraction   = lift2 max_health health (\m h -> h / m)
instant [1]: heal 100, level_up 100
```

Haskell: `stage1` in `Shapes.hs`, the same constructors with two loops.
GHC gives max_health 100 → `[1]` 200, health 60 → `[1]` 100, and fraction
0.6 → `[1]` 0.5. Two evaluations. The knot agrees. Here the heal and the
level-up meet at one node, and the answer is fixed by the semantics: the
clamp uses the maximum from before the instant.

### 2b, stage 2: the full slice on a longer schedule

The program is shape 1 with both lifts (`slice Full`, `Lifts::Both`). The
schedule covers full absorption, partial absorption, a merged heal and
damage, a clamp by the pre-instant maximum while the maximum steps, steps
to equal values, damage to zero, and a heal from zero:

| instant | heal | damage | level_up |
|---|---|---|---|
| `[1]` | | 10 | |
| `[2]` | 100 | 50 | 100 |
| `[3]` | | 30 | |
| `[4]` | 500 | | 50 |
| `[5]` | 100 | | |
| `[6]` | | 300 | |
| `[7]` | 40 | | |

Expected (GHC; three evaluations; the knot agrees; `FullPairCell` agrees):

| | init | `[1]` | `[2]` | `[3]` | `[4]` | `[5]` | `[6]` | `[7]` |
|---|---|---|---|---|---|---|---|---|
| max_health | 100 | | 200 | | 250 | | | |
| shield | 30 | 20 | 0 | 0 | | | 0 | |
| took (event) | | 0 | -30 | -30 | | | -300 | |
| delta (event) | | 0 | 70 | -30 | 500 | 100 | -300 | 40 |
| health | 60 | 60 | 100 | 70 | 200 | 250 | 0 | 40 |
| fraction | 0.6 | 0.6 | 0.5 | 0.35 | 0.8 | 1.0 | 0.0 | 0.16 |
| effective | 90 | 80 | 100 | 70 | 200 | 250 | 0 | 40 |

Corners this schedule fixes:

- Health steps to an equal value at `[1]`, and shield does at `[3]` and
  `[6]`. `listen_steps` fires on each of those.
- At `[4]`, health is 200, not 250, because the clamp reads max_health
  before it steps.
- The two lifts step exactly at the union of their inputs' step times, once
  per instant. GHC checks both.

For the adapter: in arm A the three inputs arrive as one host message,
`TurnResolved { heal, extra_max, damage }`. Under RFD 7 that message is one
unit, so its three sends are one transaction, as in `run`, or one tuple
input. It is never a frame batched into one transaction.

The Bough test is `health_and_shield_long_run` in the sketch. It uses
`LONG_RUN` and the vectors above, with fraction compared as `f32`: each
expected value is the correctly rounded quotient, so equality is exact.

## Shape 3: other shapes the records flag

**The records flag exactly one loop shape as a correctness problem: shape
1.** Record 0002 flags frame batching, where `merge` combines unrelated
events. That is not a loop shape, and RFD 7's fold law already tests it. The
research note's listener deadlock (R5) is not a loop shape either. What the
records and the issue do add is the #52 shape's neighbourhood, which a
fixed test and the generator should cover because the upstream bug is
fragile. The handoff reports that "four attempts to reduce it further
stopped reproducing".

- **3a, the lift that did not trigger it: `lift2(max_health, health)`.** This
  is also a diamond through a loop: max_health is upstream of health's
  feeding stream, and is lifted together with health. It is row 1 of shape
  1, and the fixed test keeps all four rows.
- **3b, the reductions from #52 (reconstructed from prose), on record 0003's
  instant:**

  | variant | program change | upstream | oracle health at `[1]` |
  |---|---|---|---|
  | `NoMaxRead` | health's snapshot reads only health: `hold 60 (snapshot delta health (\d cur -> max 0 (cur + d)))` | still reproduced | 140 |
  | `NoMerge` | took drives health directly, no merge with heal: `hold 60 (snapshot3 took health max_health clamp)` | did not reproduce | 40 |
  | two inputs | not reconstructable: the issue does not say which input was dropped | did not reproduce | |

  On the long schedule, `NoMaxRead` health is initial 60, then `[1]` 60,
  `[2]` 130, `[3]` 100, `[4]` 600, `[5]` 700, `[6]` 400, `[7]` 440 (seven
  evaluations). `NoMerge` health is initial 60, then `[1]` 60, `[2]` 30,
  `[3]` 0, `[6]` 0 (three evaluations). The knot agrees on all four runs.
- **3c, not in the records: health steps only when its value changes.** I
  added this because it is the ordinary way to suppress no-op writes, and
  because the lazy knot cannot compute it. The event spine through the loop
  depends on the loop's values, which is fact 1's situation.

  ```haskell
  health = Hold 60 (MapS fst (Filter (\(new, cur) -> new /= cur)
             (Snapshot (\(d, cur) m -> (clampTo m (cur + d), cur))
                       (Snapshot (,) delta healthFwd) maxHealth))) [0]
  ```
  ```rust
  healed.merge(b, took, |x, y| x + y)
      .snapshot(health_fwd, |d, cur| (d, *cur))
      .snapshot(max_health, |(d, cur), max| { let new = clamp(d, cur, *max); (new != cur).then_some(new) })
      .filter_map(|o| o)
      .hold(b, 60)
  ```
  On the long schedule, the fixed point needs three evaluations. Health has
  no step at `[1]` (delta 0) and is otherwise as in 2b: `[2]` 100, `[3]` 70,
  `[4]` 200, `[5]` 250, `[6]` 0, `[7]` 40. Fraction loses its `[1]` step.
  Effective keeps its `[1]` step, from the shield. The lazy knot had not
  returned after 20 s, and its resident memory reached 1.2 GB in 8 s.

For RFD 1's generator, the family to produce is: a hold H through a loop,
whose feeding stream reads, by snapshot, a cell X that is itself a hold
through its own loop on a shared input. The generator should vary:

- whether that read passes through a `merge`;
- whether H's snapshot reads a third looped cell;
- whether a `filter` sits inside H's loop;
- the lifts added on top, `(H, X)` and `(Y, H)` with Y upstream of H.

## Commands and output

```text
$ git clone --depth 1 https://github.com/RadicalZephyr/bevy-sodium /home/user/radicalzephyr/bevy-sodium
$ cd /home/user/radicalzephyr/bevy-sodium
$ git fetch --depth 50 origin '+refs/heads/*:refs/remotes/origin/*' '+refs/pull/*/head:refs/remotes/origin/pr/*'
$ git worktree add /home/user/radicalzephyr/bevy-sodium-0003 origin/decision-0003-leverage
HEAD is now at 8e5cccf Record 0003: order independence is the value proposition

$ cd /tmp/claude-0/-home-user/14730645-93aa-5548-9058-aaea78a53772/scratchpad/research-loop-shapes
$ ghc -O0 -outputdir build -i/home/user/sodiumfrp/sodium/denotational Shapes.hs -o shapes
[1 of 3] Compiling Reactive.Sodium.Denotational ( /home/user/sodiumfrp/sodium/denotational/Reactive/Sodium/Denotational.hs, build/Reactive/Sodium/Denotational.o )
[2 of 3] Compiling Main             ( Shapes.hs, build/Main.o )
[3 of 3] Linking shapes
```

`-outputdir build` keeps the `.o` and `.hi` files out of
`/home/user/sodiumfrp`. A `find` afterwards shows none there.

```text
$ ./shapes            # exit 0; the full output, also saved as shapes.out
== calibration: fact 1's capped counter
  initial 0; steps [1] 1 [2] 2 [3] 3 [4] 4 [5] 5 [6] 6 [7] 7 [8] 8 [9] 9 [10] 10
  program text evaluated 12 times
  PASS capped counter steps 1..10 at [1]..[10]

== shape 1: sodium-rust#52 (bevy-sodium 0003-lift2-loop-bug.rs)
inputs:
  heal     [1] 100
  damage   [1] 50
  level_up [1] 100
fixed point (program text evaluated 2 times):
  max_health initial 100; steps [1] 200
  shield     initial 30; steps [1] 0
  took       [1] -20
  delta      [1] 80
  health     initial 60; steps [1] 100
  fraction   initial 0.6; steps [1] 0.5
  effective  initial 90; steps [1] 100
  PASS lazy knot terminates and equals the fixed point
  PASS matches the hand computation
  the four rows of the reproduction, health after the instant:
    no lift2                    health = 100  (lift steps forced: 0)
  PASS row "no lift2": health = 100
    lift2(max_health, health)   health = 100  (lift steps forced: 1)
  PASS row "lift2(max_health, health)": health = 100
    lift2(health, shield)       health = 100  (lift steps forced: 1)
  PASS row "lift2(health, shield)": health = 100
    both                        health = 100  (lift steps forced: 2)
  PASS row "both": health = 100
  PASS snapshot3 as nested snapshots = snapshot of a lift2 pair cell
  PASS fraction and effective each step once in the instant (5.14 no-glitch)

== shape 2a: the health slice, stage 1 (no shield), record 0003's instant without damage
fixed point (program text evaluated 2 times):
  max_health initial 100; steps [1] 200
  health     initial 60; steps [1] 100
  fraction   initial 0.6; steps [1] 0.5
  PASS lazy knot terminates and equals the fixed point
  PASS health 100 (clamped by the maximum from before the instant), max 200, fraction 0.5

== shape 2b: the health-and-shield slice (stage 2), long schedule
inputs:
  heal     [2] 100 [4] 500 [5] 100 [7] 40
  damage   [1] 10 [2] 50 [3] 30 [6] 300
  level_up [2] 100 [4] 50
fixed point (program text evaluated 3 times):
  max_health initial 100; steps [2] 200 [4] 250
  shield     initial 30; steps [1] 20 [2] 0 [3] 0 [6] 0
  took       [1] 0 [2] -30 [3] -30 [6] -300
  delta      [1] 0 [2] 70 [3] -30 [4] 500 [5] 100 [6] -300 [7] 40
  health     initial 60; steps [1] 60 [2] 100 [3] 70 [4] 200 [5] 250 [6] 0 [7] 40
  fraction   initial 0.6; steps [1] 0.6 [2] 0.5 [3] 0.35 [4] 0.8 [5] 1.0 [6] 0.0 [7] 0.16
  effective  initial 90; steps [1] 80 [2] 100 [3] 70 [4] 200 [5] 250 [6] 0 [7] 40
  PASS lazy knot terminates and equals the fixed point
  PASS matches the hand computation
  PASS snapshot3 as nested snapshots = snapshot of a lift2 pair cell
  PASS fraction steps exactly at the union of max_health's and health's step times
  PASS effective steps exactly at the union of health's and shield's step times

== shape 3: the reductions sodium-rust#52 reports (reconstructed), record 0003's instant
NoMaxRead (program text evaluated 2 times):
  max_health initial 100; steps [1] 200
  shield     initial 30; steps [1] 0
  took       [1] -20
  delta      [1] 80
  health     initial 60; steps [1] 140
  fraction   initial 0.6; steps [1] 0.7
  effective  initial 90; steps [1] 140
  PASS NoMaxRead: lazy knot terminates and equals the fixed point
  PASS NoMaxRead: health after the instant = 140
NoMerge (program text evaluated 2 times):
  max_health initial 100; steps [1] 200
  shield     initial 30; steps [1] 0
  took       [1] -20
  delta      [1] -20
  health     initial 60; steps [1] 40
  fraction   initial 0.6; steps [1] 0.2
  effective  initial 90; steps [1] 40
  PASS NoMerge: lazy knot terminates and equals the fixed point
  PASS NoMerge: health after the instant = 40
  (NoMaxRead and NoMerge on the long schedule:)
  NoMaxRead health initial 60; steps [1] 60 [2] 130 [3] 100 [4] 600 [5] 700 [6] 400 [7] 440  [7 evaluations]
  PASS NoMaxRead long: lazy knot equals the fixed point
  NoMerge health initial 60; steps [1] 60 [2] 30 [3] 0 [6] 0  [3 evaluations]
  PASS NoMerge long: lazy knot equals the fixed point

== shape 3c: health steps only when its value changes (not in the records), long schedule
fixed point (program text evaluated 3 times):
  max_health initial 100; steps [2] 200 [4] 250
  shield     initial 30; steps [1] 20 [2] 0 [3] 0 [6] 0
  took       [1] 0 [2] -30 [3] -30 [6] -300
  delta      [1] 0 [2] 70 [3] -30 [4] 500 [5] 100 [6] -300 [7] 40
  health     initial 60; steps [2] 100 [3] 70 [4] 200 [5] 250 [6] 0 [7] 40
  fraction   initial 0.6; steps [2] 0.5 [3] 0.35 [4] 0.8 [5] 1.0 [6] 0.0 [7] 0.16
  effective  initial 90; steps [1] 80 [2] 100 [3] 70 [4] 200 [5] 250 [6] 0 [7] 40
  PASS health: no step at [1] (delta 0), otherwise as shape 2b
  PASS everything but health, fraction and effective as shape 2b
  lazy knot: run `./shapes knot-3c` under a timeout; it is expected not to return.

all checks passed

$ timeout 20 ./shapes knot-3c; echo "exit $?"
shape 3c by lazy knot, health steps:
exit 124 after 21s
# (a second run under ps: RSS 715 MB at 4 s, 1.2 GB at 8 s)

$ cd bough-sketch && cargo check --offline --tests
    Checking bough v0.1.0 (/home/user/bough/bough)
    Checking loop-shapes-sketch v0.0.0 (.../research-loop-shapes/bough-sketch)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.50s
$ cargo clippy --offline --tests      # no warnings
$ cargo test --offline                # all four reach the skeleton's todo!()
thread 'tests::sodium_rust_52_lift_does_not_change_its_inputs' panicked at /home/user/bough/bough/src/graph.rs:44:9:
not yet implemented
(and the same for the other three tests)
```

The sketch crate depends on `/home/user/bough/bough` by path, with its own
target directory. `git -C /home/user/bough status` is clean afterwards.

## Files

- `Shapes.hs`: the programs, the fixed-point solver, the knot, and the checks.
- `shapes.out`: the output above.
- `bough-sketch/src/lib.rs`: the Bough graph code and the four fixed tests,
  which type-check against the skeleton.
- `build/`, `shapes`: GHC build products.
