# The Sodium denotational semantics, vendored, and the oracle over it

`Reactive/Sodium/Denotational.hs` and `sodium.hs` are copied unchanged from
the Sodium repository, <https://github.com/SodiumFRP/sodium>, directory
`denotational/`, at commit `a6f5b3186389e86d949a2965b8d1ed3f522fb2e0`. They are
the executable version of *Denotational Semantics of Sodium*, version 1.1,
by Stephen Blackheath, under the BSD licence in `LICENSE`.

Nothing there is edited. The oracle builds on these files and never changes
them, so a test that cites the semantics cites this text.

## The oracle

| File | What it is |
|---|---|
| `Oracle/Program.hs` | The program description: inputs, one `Def` per node, the nodes to observe, and the sends of each transaction. Its derived `Read` syntax is the protocol, and `../src/program.rs` prints the same types. |
| `Oracle/Derived.hs` | Every Bough operation over the constructors of `Denotational.hs`, from the spike's research (research-derived-ops, with its verification's corrections), and the two patches below. |
| `Oracle/Interpret.hs` | Checks a program, interprets it into terms of the semantics, computes loops by fixed-point iteration, and renders the answer. |
| `Oracle.hs` | The process: one program per line on standard input, one answer per line on standard output. |
| `OracleTests.hs` | The HUnit tests of all of the above. |

The answers:

```text
OK [[0,[[[1],6]]],[1,0,[[[1],1]]]]    a stream's events; a cell's value and steps
ERR node 1 (SMap): N 0 carries TList   a malformed program, or evaluating it failed
TIMEOUT                                longer than five seconds
```

A stream is `[0, [[t, v], ...]]` and a cell is `[1, v0, [[t, v], ...]]`, one
element per observed node. With the window `FromFirstTransaction`, `v0` is
the cell's value after transaction zero and its child transactions, and the
events and steps are those from `[1]`; with `Everything`, they are the
initial value and every step and event. A loop that does not converge in 200
rounds answers `ERR`, and so does a heap overflow (`ERR heap`): the process
is built with `-threaded`, so the five-second limit interrupts pure code,
and linked with `-with-rtsopts=-M1g`. It lives on after either.

Loops are computed by explicit fixed-point iteration, never by lazy
knot-tying: the text does not terminate on a loop whose spine depends on its
values, such as a filter inside a loop through a hold. Each loop node is
bound to a concrete term, starting from a cell that never steps and a stream
that never fires, and the whole scope is interpreted again until every
loop's iterate reproduces itself. The iteration is the semantics for loops
whose back edge is a read before the instant (`snapshot`, `gate`, `sample`,
the selection of `switch_stream`) or a child instant (`split`, `defer`). A
back edge through a steps view is a same-instant cycle; programs for the
oracle must not contain one.

## Two patches to the text

Both live in `Oracle/Derived.hs`, in the derived layer, and leave
`Denotational.hs` untouched. `OracleTests.hs` shows each one's case under the
text and under the patch, and shows the twenty vectors of `sodium.hs`
unchanged under both.

**F6, `switchCell`.** The text's `SwitchC c t0` scans the outer cell `c` from
its initial value, not from `t0`. A switch cell created at `[3]` over an
outer that switched from `c1` to `c2` at `[1]` gets the steps
`[([3],'a'),([1],'x')]`: a step to the deselected inner at its creation,
then a step before it existed. When `c2` itself steps at `[3]`, the text
gives `('x',[([3],'a'),([1],'y')])`, and `sample` at `[3]` reads `'y'` where
the value is `'x'`. The patch chops the outer cell at the creation time:

```haskell
switchCell c = Reactive $ \t0 -> SwitchC (concrete (D.chopFront (D.steps c) t0)) t0
```

It gives `('x',[([3],'y')])`. Java's `Cell.switchC` starts from the outer's
value at creation, which is the chopped cell. The two agree whenever the
outer has no step before `t0`, which covers every vector in the text.

**F7, `split`.** The text's `Split` concatenates each parent event's children
in the order of the parents. That is time order unless the split is fed by
its own children: in the fidelity review's program (`SplitSort.hs`), the
children of `[0,0]` come after `[0,1]`, and a merge downstream sees two
events at `[0,0,0]`, 100 and 10, where the engine gives one, 110. The patch
sorts the children stably by time:

```haskell
split s = MkStream (sortOn fst (D.occs (Split s)))
```

A sort changes nothing on a stream the text already orders, which covers
every vector.

## Running it

`cargo test -p bough-oracle` builds `Oracle.hs` with GHC into
`target/tmp/bough-oracle` and runs every test that needs it, including one
that builds and runs `OracleTests.hs` and `sodium.hs`. It needs GHC with the
HUnit package: `apt-get install ghc libghc-hunit-dev`, or GHC through ghcup
and then `cabal install --lib HUnit`. `BOUGH_GHC` names a GHC other than the
`ghc` on the `PATH`. Without GHC those tests panic with that hint;
`BOUGH_ORACLE=skip` makes each say that it skipped and return instead, as
continuous integration does.

By hand, from this directory, with the build products kept out of it:

```sh
mkdir -p /tmp/oracle /tmp/oracle-tests /tmp/sodium-vectors
ghc -O1 -threaded -rtsopts -with-rtsopts=-M1g -outputdir /tmp/oracle -o /tmp/oracle/bough-oracle Oracle.hs
echo 'Program Everything [] [SLit TInt [([0],I 5)],SMap (Add Arg (Lit 1)) (N 0)] [1] []' | /tmp/oracle/bough-oracle

ghc -outputdir /tmp/oracle-tests -o /tmp/oracle-tests/run OracleTests.hs
/tmp/oracle-tests/run

ghc -outputdir /tmp/sodium-vectors -o /tmp/sodium-vectors/run sodium.hs
/tmp/sodium-vectors/run
```

With GHC 9.4.7 the program answers `OK [[0,[[[0],6]]]]`, all 173 cases of
`OracleTests.hs` pass, and all twenty cases of `sodium.hs` pass.
