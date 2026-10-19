{-# LANGUAGE GADTs #-}
-- Expected values for bough's stage 6 construct tests, computed over an
-- unchanged copy of Reactive.Sodium.Denotational (Sodium 1.1; SHA-256
-- d6772fb8...c822b, the file vendored in bough-oracle/haskell) with the
-- oracle's patches F6 (a switch cell created at t0 switches over its outer
-- chopped at t0) and F7 (Split's output sorted stably by time), exactly
-- bough-oracle's Oracle/Derived.hs `switchCell` and `split`.
--
-- `construct` is Execute over MapS, as Derived.hs `construct`, with a
-- creation time: the text's Execute has none (F44), so a construct created
-- at t0 would also run its body at its source's events before t0, which
-- nothing created at t0 can observe. `constructAt t0` keeps the events at or
-- after t0. SwitchS is the text's, which has no creation time either
-- (risk 5): a switch_stream created at t0 is compared at t0 and after.
--
-- Loops are solved by explicit fixed-point iteration (fact 2), as in
-- stage3-ghc to stage5-ghc: a stream loop replays the previous iterate's
-- events from `MkStream []`; a cell loop replays the previous iterate's
-- steps from a cell that does not step. A loop declared inside a construct
-- body is solved inside the body, at the body's instant, as Derived.hs
-- `cellLoop` solves a loop at the time of its declaring scope.
--
-- Build is instant [0]; transaction k is [k]; the children of t are
-- t ++ [n]. A sample from I/O after transaction k reads `at (steps c) [k+1]`;
-- a sample inside a body at t reads `at (steps c) t`.
--
-- Build: ghc -i. -outputdir build -o stage6 Stage6.hs && ./stage6
module Main (main) where

import Data.List (sortOn)
import Data.Maybe (fromJust, isJust)
import Reactive.Sodium.Denotational

-- | F6 (Oracle/Derived.hs): the outer chopped at the creation time.
switchCell :: Cell (Cell a) -> T -> Cell a
switchCell c t0 = SwitchC (concrete (chopFront (steps c) t0)) t0

-- | The text's SwitchC, to show where F6 matters.
switchCellText :: Cell (Cell a) -> T -> Cell a
switchCellText = SwitchC

concrete :: C a -> Cell a
concrete (a, sts) = Hold a (MkStream sts) []

-- | F7 (Oracle/Derived.hs): the children sorted stably by time.
split :: Stream [a] -> Stream a
split s = MkStream (sortOn fst (occs (Split s)))

defer :: Stream a -> Stream a
defer s = split (MapS (: []) s)

-- | A construct created at t0: Execute over its source's events at or
-- after t0.
constructAt :: T -> Stream a -> (a -> Reactive b) -> Stream b
constructAt t0 s f = Execute (MapS f (MkStream (occsFrom t0 s)))

occsFrom :: T -> Stream a -> S a
occsFrom t0 s = filter ((>= t0) . fst) (occs s)

-- | accumulate created at t0: the hold snapshots itself.
accumulateAt :: T -> Stream a -> s -> (a -> s -> s) -> Cell s
accumulateAt t0 s initial f = let c = Hold initial (Snapshot f s c) t0 in c

-- | scan (Sodium's collect) created at t0.
scanAt :: T -> Stream a -> s -> (a -> s -> (b, s)) -> Stream b
scanAt t0 s initial f =
  let state = Hold initial (MapS snd pairs) t0
      pairs = Snapshot f s state
   in MapS fst pairs

-- | once created at t0 (Derived.hs `once`).
onceAt :: T -> Stream a -> Stream a
onceAt t0 s = MkStream (take 1 (occsFrom t0 s))

filterMap :: (a -> Maybe b) -> Stream a -> Stream b
filterMap f s = MapS fromJust (Filter isJust (MapS f s))

-- | A cell loop by fixed point. `def` maps the forward to the definition
-- and to what else its scope builds from the forward; the forward replays
-- the definition's steps. Returns the forward at the fixed point, the rest,
-- and the number of rounds.
fixC :: Eq a => (Cell a -> (Cell a, r)) -> (Cell a, r, Int)
fixC def = go 1 (concrete (fst (steps (fst (def unread))), []))
  where
    unread = concrete (error "a loop's initial value read its own loop", [])
    go k fwd =
      let next = steps (fst (def fwd))
       in if next == steps fwd then (fwd, snd (def fwd), k) else go (k + 1) (concrete next)

-- | A stream loop by fixed point, from a stream that never fires.
solveS :: Eq a => (Stream a -> Stream a) -> (S a, Int)
solveS def = go 1 []
  where
    go k cur
      | k > 100 = error "no fixed point in 100 rounds"
      | otherwise =
          let next = occs (def (MkStream cur))
           in if next == cur then (cur, k) else go (k + 1) next

b0 :: T
b0 = [0]

ev :: [(Int, a)] -> Stream a
ev xs = MkStream [([k], a) | (k, a) <- xs]

-- | Samples from I/O after the build and after transactions 1 to n.
samples :: Cell a -> Int -> [a]
samples c n = [at (steps c) [k + 1] | k <- [0 .. n]]

-- | Samples from I/O after transactions k to n.
samplesFrom :: Cell a -> Int -> Int -> [a]
samplesFrom c k n = [at (steps c) [j + 1] | j <- [k .. n]]

-- | The steps of a cell at or after t0.
stepsFrom :: T -> Cell a -> S a
stepsFrom t0 c = filter ((>= t0) . fst) (snd (steps c))

line :: Show a => String -> a -> IO ()
line name v = putStrLn (name ++ ": " ++ show v)

-- 1. Claim 4: a construct at [1] builds a hold over e, which fires at [1]
--    too. The hold picks e's event up at its creation instant; a sample
--    inside the body reads the value before [1].
claim4 :: IO ()
claim4 = do
  let e = ev [(1, 7), (2, 1)]
      go = ev [(1, 100)]
      body k = Reactive (\t -> let h = Hold 0 (MapS (+ k) e) t in (h, at (steps h) t))
      made = Hold (Constant 0, 0) (constructAt b0 go body) b0
      h = fst (at (steps made) [2])
  line "claim4: sample inside" (snd (at (steps made) [2]))
  line "claim4: samples h after [1], [2]" (samplesFrom h 1 2)

-- 2. R1 and R1c (review-fidelity/hs/Review.hs): a cell loop declared in the
--    body, a map_cell over the forward built before the definition, and a
--    snapshot of the map_cell at the body's instant.
r1 :: IO ()
r1 = do
  let s = ev [(1, 5), (2, 7)]
      body _ = Reactive $ \t ->
        let def fwd = (Hold 0 (Snapshot (\x tt -> tt + x) s fwd) t, ())
            (total, (), rounds) = fixC def
            label = MapC (* 10) total
            seen = Hold 99 (Snapshot (\_ l -> l) s label) t
         in (total, label, seen, rounds)
      (_, (total, label, seen, rounds)) = head (occs (constructAt b0 s body))
  line "R1: rounds" rounds
  line "R1: samples total, label, seen after [1], [2]" (samplesFrom total 1 2, samplesFrom label 1 2, samplesFrom seen 1 2)
  line "R1: steps label" (steps label)

-- 3. R2 and R2b: a stream loop in the body, a hold over the forward built
--    before the definition, which fires at the body's instant. The two
--    differ only in the order Bough builds them; they denote one program.
r2 :: IO ()
r2 = do
  let s = ev [(1, 5), (2, 7)]
      body n = Reactive $ \t ->
        let (fwd, _) = solveS (\_ -> MapS (+ n * 20) s)
         in Hold 0 (MkStream fwd) t
      helds = map snd (occs (constructAt b0 s body))
  line "R2: samples of each held after [1], [2]" [samplesFrom (helds !! 0) 1 2, samplesFrom (helds !! 1) 2 2]

-- 4. R6 (Review.hs, with F6): a switch cell built in the body over an outer
--    and an inner both built there, the inner's input firing then.
r6 :: IO ()
r6 = do
  let go = ev [(1, ())]
      xs = ev [(1, 4), (2, 5)]
      body _ = Reactive $ \t ->
        let fresh = Hold 1 (MapS (* 3) xs) t
            outer = Hold (Constant 0) (MapS (const fresh) go) t
            sw = switchCell outer t
         in (Hold 99 (Updates sw) t, sw)
      (h, sw) = snd (head (occs (constructAt b0 go body)))
  line "R6: samples h after [1], [2]" (samplesFrom h 1 2)
  line "R6: steps sw" (steps sw)

-- 5. F6: a switch cell built in a body at [2] over an outer that switched
--    at [1]. The patched SwitchC starts from the outer's value at [2]; the
--    text's scans the outer from its initial value, emits a step at [2]
--    carrying the deselected inner's value and a step at [1] before its
--    creation.
f6 :: IO ()
f6 = do
  let sel = ev [(1, ())]
      go = ev [(2, ())]
      x = ev [(3, 30)]
      c2 = Hold 2 x b0
      outer = Hold (Constant 1) (MapS (const c2) sel) b0
      body sc () = Reactive $ \t ->
        let sw = sc outer t
         in (sw, at (steps sw) t, Hold 99 (Updates sw) t)
      made sc = snd (head (occs (constructAt b0 go (body sc))))
      (sw, pre, seen) = made switchCell
      (swText, preText, seenText) = made switchCellText
  line "F6: sample inside" pre
  line "F6: samples seen after [2], [3]" (samplesFrom seen 2 3)
  line "F6: steps sw" (steps sw)
  line "F6 (text): sample inside" preText
  line "F6 (text): samples seen after [2], [3]" (samplesFrom seenText 2 3)
  line "F6 (text): steps sw" (steps swText)

-- 6. Switch streams built in bodies: one at [2], the instant its outer
--    steps, which forwards the inner selected before [2] at [2]; one at [3],
--    when the outer is quiet. Events at or after each creation.
streamAt :: IO ()
streamAt = do
  let a = ev [(1, 'a'), (2, 'b'), (3, 'c'), (4, 'd'), (5, 'e')]
      z = ev [(1, 'X'), (2, 'Y'), (3, 'Z'), (4, 'W'), (5, 'V')]
      sel = ev [(2, True), (4, False)]
      outer = Hold a (MapS (\p -> if p then z else a) sel) b0
      go = ev [(2, ()), (3, ())]
  line "switch_stream at t" [(t, occsFrom t (SwitchS outer)) | (t, ()) <- occs go]

-- 7. Construct inside construct: each body at [k] builds a construct over
--    the same stream, which fires at [k] too and at every later instant,
--    each building a hold at its instant; a switch cell over the holds; and
--    a switch cell at [0] over the switches the bodies build.
nested :: IO ()
nested = do
  let s = ev [(1, 1), (2, 2), (3, 3)]
      c0 = Constant 0
      inner n m = Reactive (\t -> Hold 0 (MapS (\x -> x + 10 * m + 100 * n) s) t)
      outerBody n = Reactive $ \t ->
        let latest = Hold c0 (constructAt t s (inner n)) t
         in switchCell latest t
      made = constructAt b0 s outerBody
      top = switchCell (Hold c0 made b0) b0
  line "nested: steps top" (steps top)
  line "nested: samples top" (samples top 3)
  line "nested: samples of each body's switch, from its instant" [samplesFrom sw (head t) 3 | (t, sw) <- occs made]

-- 8. Construct in child transactions: a split feeds a construct, whose body
--    at [k, n] builds a hold over the split's items and a hold over a defer
--    of them, both from [k, n]. And a construct built in the build over a
--    split of a steps_with_current of a constant, which runs its body in the
--    build's children [0, n].
children :: IO ()
children = do
  let lists = ev [(1, [1, 2, 3]), (2, [4])]
      items = split lists
      c0 = Constant 0
      body v = Reactive $ \t ->
        (Hold 0 (MapS (\x -> x * 10 + v) items) t, Hold 0 (defer (MapS (+ (v * 100)) items)) t)
      made = constructAt b0 items body
      top = switchCell (Hold c0 (MapS fst made) b0) b0
      later = switchCell (Hold c0 (MapS snd made) b0) b0
  line "children: steps top" (steps top)
  line "children: steps later" (steps later)
  line "children: samples top, later" (samples top 2, samples later 2)
  line "children: samples of each body's hold, from its transaction" [samplesFrom (fst c) (head t) 2 | (t, c) <- occs made]
  let seed = split (Value (Constant [5, 6]) b0)
      made0 = constructAt b0 seed (\v -> Reactive (\t -> Hold v (MapS (+ v) items) t))
      top0 = switchCell (Hold c0 made0 b0) b0
  line "children: build's children: steps top0" (steps top0)
  line "children: build's children: samples top0" (samples top0 2)

-- 9. RFD 4's main dynamic pattern closed as RFD 2's navigation loop: every
--    navigation event builds a screen, a stream of the clicks it sees
--    numbered by a counter built with it; a hold keeps the current screen
--    and a switch stream reads its events; a click of 0 navigates to the
--    next screen.
navigation :: IO ()
navigation = do
  let clicks = ev [(1, 5), (2, 0), (3, 7), (4, 0), (5, 9), (6, 0), (7, 4)]
      screen n t =
        let count = accumulateAt t clicks 0 (\_ c -> c + 1)
         in Snapshot (\c k -> (n, c, k + 1)) clicks count
      system nav =
        let screens = constructAt b0 nav (\n -> Reactive (screen n))
         in SwitchS (Hold (screen 0 b0) screens b0)
      next = filterMap (\(n, c, _) -> if c == 0 then Just (n + 1) else Nothing)
      (navs, rounds) = solveS (next . system)
  line "navigation: rounds" rounds
  line "navigation: nav" navs
  line "navigation: out" (occs (system (MkStream navs)))

-- 10. The loop through a switch stream's selection (review-fidelity/hs/
--     SwitchS.hs) in its construct form: every event of the switch builds
--     the stream it follows from the next instant on.
selectionLoop :: IO ()
selectionLoop = do
  let ticks = ev [(n, 1) | n <- [1 .. 4]]
      system s = SwitchS (Hold (MapS (+ 0) ticks) (constructAt b0 s (\v -> Reactive (\_ -> MapS (+ v) ticks))) b0)
  line "selection loop" (solveS system)

-- 11. An input built in a body at [1]: I/O sends to it at [2] and [3],
--     after receiving its token; an accumulator over it built with it, and
--     a stream over it.
inputInside :: IO ()
inputInside = do
  let go = ev [(1, 100)]
      sends = ev [(2, 5), (3, 6)]
      body k = Reactive (\t -> (accumulateAt t sends k (+), MapS (* 2) sends))
      (total, doubled) = snd (head (occs (constructAt b0 go body)))
  line "input inside: samples total after [1], [2], [3]" (samplesFrom total 1 3)
  line "input inside: doubled" (occs doubled)

-- 12. A body builds a hold over the construct's own events, through a
--     stream loop: the event it returns exists only once it has returned,
--     and the hold picks it up at the body's instant.
ownOutput :: IO ()
ownOutput = do
  let s = ev [(1, 1), (2, 2)]
      system outs = constructAt b0 s (\v -> Reactive (\t -> (v * 10, Hold 0 (MapS (+ v) outs) t)))
      (outs, rounds) = solveS (MapS fst . system)
      made = system (MkStream outs)
      shown = switchCell (Hold (Constant 0) (MapS snd made) b0) b0
  line "own output: rounds" rounds
  line "own output: outs" outs
  line "own output: steps shown" (steps shown)
  line "own output: samples of each hold, from its transaction" [samplesFrom h (head t) 2 | (t, (_, h)) <- occs made]

-- 13. Switch cells built in one body at [2]: over an outer that stepped at
--     [1], one that steps at [2], one that steps at [3]; over an outer and
--     an inner both built at [2]; and a switch over a hold of switches, both
--     built at [2], which moves at [3] to the first switch.
switchMatrix :: IO ()
switchMatrix = do
  let x = ev [(1, 10), (2, 20), (3, 30)]
      y = ev [(2, 200), (3, 300)]
      c1 = Hold 1 x b0
      c2 = Hold 2 y b0
      c3 = Constant 3
      outerBy sel = Hold c1 (MapS (const c2) (ev [(sel, ())])) b0
      go = ev [(2, ())]
      selC = ev [(3, ())]
      body () = Reactive $ \t ->
        let before = switchCell (outerBy 1) t
            atT = switchCell (outerBy 2) t
            after = switchCell (outerBy 3) t
            fresh = Hold 5 (MapS (+ 1) x) t
            viaFresh = switchCell (Hold c3 (MapS (const fresh) go) t) t
            top = switchCell (Hold atT (MapS (const before) selC) t) t
         in [before, atT, after, viaFresh, top]
      sws = snd (head (occs (constructAt b0 go body)))
      t2 = [2]
  line "switch matrix: samples inside" [at (steps sw) t2 | sw <- sws]
  line "switch matrix: steps" [stepsFrom t2 sw | sw <- sws]
  line "switch matrix: samples after [2], [3]" [samplesFrom sw 2 3 | sw <- sws]

-- 14. Everything a body at [2] builds exists from [2]: a hold, an
--     accumulator, a scan, a once over s, which fires at [2]; steps and
--     steps_with_current of c, which steps at [2]; a map_cell of c; a split
--     and a defer of s; a snapshot of c, which reads c before [2]; and
--     steps_with_current of q, which is quiet at [2].
creation :: IO ()
creation = do
  let s = ev [(1, 1), (2, 2), (3, 3)]
      c = Hold 10 (ev [(1, 11), (2, 12), (3, 13)]) b0
      q = Hold 40 (ev [(3, 43)]) b0
      go = ev [(2, ())]
      body () = Reactive $ \t ->
        let held = Hold 0 s t
            acc = accumulateAt t s 0 (+)
            scanned = Hold 0 (scanAt t s 0 (\v st -> (v * 100 + st, st + 1))) t
            first = Hold 0 (onceAt t s) t
            withCurrent = Hold 0 (Value c t) t
            updates = Hold 0 (Updates c) t
            mapped = MapC (* 2) c
            mappedSteps = Hold 0 (Updates mapped) t
            pieces = accumulateAt t (split (MapS (\v -> [v, v * 10]) s)) 0 (+)
            later = Hold 0 (defer (MapS (+ 1000) s)) t
            snap = Hold 0 (Snapshot (\v w -> v * 100 + w) s c) t
            quiet = Hold 0 (Value q t) t
         in ([held, acc, scanned, first, withCurrent, updates, mappedSteps, pieces, later, snap, quiet], at (steps c) t)
      (cells, inside) = snd (head (occs (constructAt b0 go body)))
  line "creation: sample inside" inside
  line "creation: samples after [2], [3]" [samplesFrom cell 2 3 | cell <- cells]

main :: IO ()
main = do
  claim4
  r1
  r2
  r6
  f6
  streamAt
  nested
  children
  navigation
  selectionLoop
  inputInside
  ownOutput
  switchMatrix
  creation
