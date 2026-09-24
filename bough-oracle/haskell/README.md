# The Sodium denotational semantics, vendored

`Reactive/Sodium/Denotational.hs` and `sodium.hs` are copied unchanged from
the Sodium repository, <https://github.com/SodiumFRP/sodium>, directory
`denotational/`, at commit `a6f5b3186389e86d949a2965b8d1ed3f522fb2e0`. They are
the executable version of *Denotational Semantics of Sodium*, version 1.1,
by Stephen Blackheath, under the BSD licence in `LICENSE`.

Nothing here is edited. The oracle builds on these files and never changes
them, so a test that cites the semantics cites this text.

`sodium.hs` is the semantics' own test vectors. To run them:

```sh
cd bough-oracle/haskell
ghc -outputdir /tmp/sodium-vectors -o /tmp/sodium-vectors/run sodium.hs
/tmp/sodium-vectors/run
```

With GHC 9.4.7 all twenty cases pass. `sodium.hs` needs the `HUnit` package.
