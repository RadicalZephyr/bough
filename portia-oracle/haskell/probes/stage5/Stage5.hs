{-# LANGUAGE GADTs #-}
-- Expected values for bough's stage 5 switch tests, computed over an
-- unchanged copy of Reactive.Sodium.Denotational (Sodium 1.1; SHA-256
-- d6772fb8...c822b, the file vendored in bough-oracle/haskell) with the
-- oracle's patch F6: a switch cell created at t0 is SwitchC over its outer
-- chopped at t0, exactly bough-oracle's Oracle/Derived.hs `switchCell`.
-- SwitchS is the text's (no creation time).
--
-- Loops are solved by explicit fixed-point iteration (fact 2), the method of
-- stage3-ghc/Stage3.hs and stage4-ghc/Stage4.hs: a stream loop replays the
-- previous iterate's events from `MkStream []`; a cell loop replays the
-- previous iterate's steps from a cell that does not step (15, the
-- reversal), and one whose definition does not read the forward is the
-- definition itself.
--
-- Build is instant [0]; transaction k is [k]. A sample from I/O after
-- transaction k reads `at (steps c) [k + 1]`. A switch built in the build is
-- created at [0].
--
-- Build: ghc -i. -outputdir build -o stage5 Stage5.hs && ./stage5
module Main (main) where

import Data.List (sortOn)
import Reactive.Sodium.Denotational

-- | F6 (Oracle/Derived.hs): the outer chopped at the creation time.
switchCell :: Cell (Cell a) -> T -> Cell a
switchCell c t0 = SwitchC (concrete (chopFront (steps c) t0)) t0

-- | The text's SwitchC, for the vectors, where the two agree.
switchCellText :: Cell (Cell a) -> T -> Cell a
switchCellText = SwitchC

concrete :: C a -> Cell a
concrete (a, sts) = Hold a (MkStream sts) []

solveS :: Eq a => Int -> (Stream a -> Stream a) -> Maybe (S a, Int)
solveS cap def = go 1 []
  where
    go k cur
      | k > cap = Nothing
      | otherwise =
          let next = occs (def (MkStream cur))
           in if next == cur then Just (cur, k) else go (k + 1) next

b0 :: T
b0 = [0]

ev :: [(Int, a)] -> Stream a
ev xs = MkStream [([k], a) | (k, a) <- xs]

-- | Samples from I/O after the build and after transactions 1 to n.
samples :: Cell a -> Int -> [a]
samples c n = [at (steps c) [k + 1] | k <- [0 .. n]]

line :: Show a => String -> a -> IO ()
line name v = putStrLn (name ++ ": " ++ show v)

-- 1. The five switch vectors of sodium.hs, restated with inputs: every event
--    of the text at [k] is sent at [k + 1], and the switch is built at [0].
vectors :: IO ()
vectors = do
  -- SwitchS
  let s1 = ev [(1, 'a'), (2, 'b'), (3, 'c'), (4, 'd')]
      s2 = ev [(1, 'W'), (2, 'X'), (3, 'Y'), (4, 'Z')]
      c = Hold s1 (ev [(2, s2)]) b0
  line "vector SwitchS" (occs (SwitchS c))
  -- SwitchC 1 to 4: the steps of the switch, and samples after each
  -- transaction.
  let c1 = Hold 'a' (ev [(1, 'b'), (2, 'c'), (3, 'd'), (4, 'e')]) b0
      vec name c2 = do
        let c3 = Hold c1 (ev [(2, c2)]) b0
            c4 = switchCell c3 b0
        line ("vector " ++ name) (steps c4)
        line ("vector " ++ name ++ " (text)") (steps (switchCellText c3 b0))
        line ("vector " ++ name ++ " samples") (samples c4 4)
  vec "SwitchC 1" (Hold 'V' (ev [(1, 'W'), (2, 'X'), (3, 'Y'), (4, 'Z')]) b0)
  vec "SwitchC 2" (Hold 'W' (ev [(2, 'X'), (3, 'Y'), (4, 'Z')]) b0)
  vec "SwitchC 3" (Hold 'X' (ev [(3, 'Y'), (4, 'Z')]) b0)
  let c2 = Hold 'V' (ev [(1, 'W'), (2, 'X'), (3, 'Y'), (4, 'Z')]) b0
      c3 = Hold '1' (ev [(1, '2'), (2, '3'), (3, '4'), (4, '5')]) b0
      c4 = Hold c1 (ev [(2, c2), (4, c3)]) b0
      c5 = switchCell c4 b0
  line "vector SwitchC 4" (steps c5)
  line "vector SwitchC 4 (text)" (steps (switchCellText c4 b0))
  line "vector SwitchC 4 samples" (samples c5 4)

-- 2. Claim 5 (probe 9): the outer switches from a constant to c2 at the
--    instant x makes c2 step; then c2 steps alone.
claim5 :: IO ()
claim5 = do
  let x = ev [(1, 7), (2, 8)]
      sel = ev [(1, ())]
      c2 = Hold 2 (MapS (* 10) x) b0
      outer = Hold (Constant 1) (MapS (const c2) sel) b0
      sc = switchCell outer b0
  line "claim5: steps sc" (steps sc :: C Int)
  line "claim5: samples" (samples sc 2)

-- 3. R3 to R5 and R8 (review-fidelity/hs/Review.hs), with F6.
review :: IO ()
review = do
  let x = ev [(1, 7 :: Int)]
      sel = ev [(1, ())]
      a = Constant 1
      c2 = Hold 2 (MapS (* 10) x) b0
      outer = Hold a (MapS (const c2) sel) b0
      -- R3: the loop's definition does not read the forward, so the
      -- forward is the definition.
      c = switchCell outer b0
      m = MapC (+ 1) c
  line "R3: steps (map_cell (+1) loop)" (steps m)
  let sa = ev [(1, ())]
      sb = ev [(1, ())]
      outerB = Hold (Constant 1) (MapS (const c2) sb) b0
      innerB = switchCell outerB b0
      outerA = Hold (Constant 0) (MapS (const innerB) sa) b0
      top = switchCell outerA b0
  line "R4: steps top" (steps top)
  line "R4: samples top" (samples top 1)
  let other = Hold 0 (MapS (subtract 4) x) b0
      sw = switchCell outer b0
      l = Apply (MapC (+) sw) other
  line "R5: steps lift" (steps l)
  let cc = Constant 2
      def8 = Hold a (MapS (const cc) sel) b0
      cur = switchCell def8 b0
  line "R8: samples cur" (samples cur 1)

-- 4. The switch_stream probes (review-fidelity/hs/SwitchS.hs): a selector
--    step while the old inner is quiet; and the loop through the selection,
--    here without construct: every event selects one of five streams built
--    beforehand, as a shared stream held directly, and as a linear stream
--    in a constant cell selected through a switch cell.
switchStreamProbes :: IO ()
switchStreamProbes = do
  let a = ev [(1, 'a'), (3, 'b'), (4, 'c')]
      z = ev [(1, 'X'), (3, 'Y'), (4, 'Z')]
      sel = ev [(2, z)]
  line "probe selector" (occs (SwitchS (Hold a sel b0)))
  let ticks = ev [(n, 1 :: Int) | n <- [1 .. 4]]
      inners v = MapS (+ v) ticks
      direct prev = SwitchS (Hold (inners 0) (MapS inners prev) b0)
      viaCells prev =
        SwitchS (switchCell (Hold (Constant (inners 0)) (MapS (Constant . inners) prev) b0) b0)
  line "probe loop, shared inners held directly" (solveS 50 direct)
  line "probe loop, linear inners through a switch cell" (solveS 50 viaCells)
  -- The same loop with construct, as review-fidelity/hs/SwitchS.hs has it.
  let mk v = Reactive (\_ -> MapS (+ v) ticks)
      built prev = SwitchS (Hold ticks (Execute (MapS mk prev)) b0)
  line "probe loop with construct (SwitchS.hs)" (solveS 50 built)

-- 5. A switch to a quiet inner steps, with the quiet inner's value; the old
--    inner's step at the switch instant is dropped; the deselected inner's
--    later steps are not the switch's.
quietInner :: IO ()
quietInner = do
  let x = ev [(1, 5), (2, 6), (3, 7), (4, 8)]
      y = ev [(3, 30)]
      sel = ev [(2, True), (4, False)]
      hx = Hold 1 x b0
      hy = Hold 2 y b0
      outer = Hold hx (MapS (\s -> if s then hy else hx) sel) b0
      sw = switchCell outer b0
  line "quiet: steps sw" (steps sw :: C Int)
  line "quiet: samples" (samples sw 4)
  line "quiet: steps (map_cell (*10) sw)" (steps (MapC (* 10) sw))

-- 6. Steps at creation: a switch built at [0] steps there, with its value;
--    steps_with_current of it fires once there; a loop forward over it
--    steps there too; a switch whose outer steps at [0] starts from the old
--    inner and steps to the new one.
creation :: IO ()
creation = do
  let x = ev [(2, 9)]
      hx = Hold 4 x b0
      outer = Hold hx (ev [(3, Constant 5)]) b0
      sw = switchCell outer b0
  line "creation: steps sw" (steps sw :: C Int)
  line "creation: updates sw" (occs (Updates sw))
  line "creation: value sw [0]" (occs (Value sw b0))
  line "creation: hold 99 (updates sw)" (samples (Hold 99 (Updates sw) b0) 3)
  -- The outer steps at [0]: a hold over the value of a constant.
  let k = Constant (6 :: Int)
      early = Hold hx (MapS (const k) (Value (Constant ()) b0)) b0
      sw0 = switchCell early b0
  line "creation: outer steps at [0]: steps" (steps sw0)
  line "creation: outer steps at [0]: samples" (samples sw0 3)
  line "creation: outer steps at [0]: text" (steps (switchCellText early b0))

-- 7. Nested switches: a switch cell of switch cells, and a switch of a
--    switch (Cell (Cell (Cell a)) switched twice), with the leaves, the
--    middles and the top moving in one instant and apart.
nested :: IO ()
nested = do
  let l1 = Hold 10 (ev [(1, 11), (3, 13), (5, 15), (6, 16)]) b0
      l2 = Hold 20 (ev [(2, 22), (3, 23), (6, 26)]) b0
      l3 = Hold 30 (ev [(4, 34), (6, 36)]) b0
      l4 = Hold 40 (ev [(5, 45), (6, 46)]) b0
      -- Two switch cells, each over two leaves.
      pickA = ev [(2, True), (5, False), (6, True)]
      pickB = ev [(3, True), (6, False)]
      oa = Hold l1 (MapS (\b -> if b then l2 else l1) pickA) b0
      ob = Hold l3 (MapS (\b -> if b then l4 else l3) pickB) b0
      sa = switchCell oa b0
      sb = switchCell ob b0
      -- A switch cell between the two switch cells.
      pickTop = ev [(3, True), (4, False), (6, True)]
      ot = Hold sa (MapS (\b -> if b then sb else sa) pickTop) b0
      top = switchCell ot b0
  line "nested: steps top" (steps top :: C Int)
  line "nested: samples top" (samples top 6)
  -- A switch of a switch: middles hold leaves, the outermost holds middles.
  let m1 = oa
      m2 = Hold l3 (MapS (\b -> if b then l4 else l3) pickB) b0
      pickOuter = ev [(3, True), (4, False), (6, True)]
      oo = Hold m1 (MapS (\b -> if b then m2 else m1) pickOuter) b0
      twice = switchCell (switchCell oo b0) b0
  line "nested: steps twice" (steps twice :: C Int)
  line "nested: samples twice" (samples twice 6)

-- 8. The forward-looking read through a memo: the new inner is a map cell
--    over a hold that steps in the switch instant.
throughMemo :: IO ()
throughMemo = do
  let x = ev [(1, 7), (2, 8), (3, 9)]
      sel = ev [(1, ())]
      h = Hold 0 x b0
      m = MapC (* 10) h
      outer = Hold (Constant 1) (MapS (const m) sel) b0
      sw = switchCell outer b0
  line "through memo: steps sw" (steps sw :: C Int)
  line "through memo: samples" (samples sw 3)

-- 9. A switch_stream whose new inner fires in the switch instant: the old
--    inner's event is forwarded there, the new inner's from the next
--    instant; a deselected inner is quiet for it.
newInnerFires :: IO ()
newInnerFires = do
  let a = ev [(1, 'a'), (2, 'b'), (3, 'c'), (4, 'd')]
      z = ev [(1, 'W'), (2, 'X'), (3, 'Y'), (5, 'Z')]
      sel = ev [(2, z), (4, a)]
      out = SwitchS (Hold a sel b0)
      viaCells = SwitchS (switchCell (Hold (Constant a) (MapS Constant sel) b0) b0)
  line "new inner fires: out" (occs out)
  line "new inner fires: linear, through a switch cell" (occs viaCells)

-- 10. A switch between two accumulators (Bough's accumulate_mut, whose
--     denotation is accumulate's): the State switch. Names are pushed; the
--     short list takes names under four letters.
stateSwitch :: IO ()
stateSwitch = do
  let names = ev [(1, "ada"), (2, "grace"), (3, "bo"), (4, "x"), (5, "eve")]
      short = Filter ((< 4) . length) names
      acc s = let c = Hold [] (Snapshot (\n l -> l ++ [n]) s c) b0 in c
      allNames = acc names
      shortNames = acc short
      pick = ev [(3, True), (5, False)]
      outer = Hold allNames (MapS (\p -> if p then shortNames else allNames) pick) b0
      current = switchCell outer b0
  line "state switch: steps current" (steps current)
  line "state switch: samples" (samples current 5)
  line "state switch: lengths" (map length (samples current 5))

-- 11. The outer steps to the inner the switch already follows: still a step
--     of the switch, with that inner's value; a map cell over it steps too.
sameInner :: IO ()
sameInner = do
  let x = ev [(1, 5), (3, 6)]
      sel = ev [(2, ()), (3, ())]
      hx = Hold 1 x b0
      outer = Hold hx (MapS (const hx) sel) b0
      sw = switchCell outer b0
  line "same inner: steps sw" (steps sw :: C Int)

-- 12. A switch_stream whose outer is a cell loop's forward, and one whose
--     outer is a map cell of a boolean: the selector probe again.
otherOuters :: IO ()
otherOuters = do
  let a = ev [(1, 'a'), (3, 'b'), (4, 'c'), (5, 'd')]
      z = ev [(1, 'X'), (3, 'Y'), (4, 'Z'), (5, 'W')]
      -- The loop's definition does not read the forward, so the forward is
      -- the definition.
      pick = Hold False (ev [(2, True), (4, False)]) b0
      byLoop = Hold a (MapS (\p -> if p then z else a) (ev [(2, True), (4, False)])) b0
      byMap = MapC (\p -> if p then z else a) pick
  line "other outers: through a loop forward" (occs (SwitchS byLoop))
  line "other outers: through a map cell" (occs (SwitchS byMap))

-- 13. A switch_stream whose outer steps at [0], its creation instant: at
--     [0] it follows the inner selected before [0], whose event there comes
--     through, and the new inner's does not; from [1] on, the new inner.
--     Both inners fire at [0] through steps_with_current of a constant.
streamCreation :: IO ()
streamCreation = do
  let a = Merge (Value (Constant 'p') b0) (ev [(1, 'a'), (2, 'b')]) const
      z = Merge (Value (Constant 'q') b0) (ev [(1, 'X'), (2, 'Y')]) const
      outer = Hold a (MapS (const z) (Value (Constant ()) b0)) b0
  line "stream creation: out" (occs (SwitchS outer))

-- 14. A switch_stream built before the stream it first follows, over a loop
--     forward closed with a constant holding that stream, which fires at
--     [0]: its event at [0] comes through. The loop's definition does not
--     read the forward, so the forward is the definition.
laterInner :: IO ()
laterInner = do
  let p = Merge (Value (Constant 'p') b0) (ev [(1, 'x'), (2, 'y')]) const
  line "later inner: out" (occs (SwitchS (Constant p)))

-- 15. Two switches reverse a dependency in one instant. Before [1], B
--     follows P = A + 10, so B depends on A; at [1], A moves to Y = B + 100
--     while B moves to the constant q, so A depends on B. A is a cell
--     loop's definition read through its forward, solved by fixed point.
reversal :: IO ()
reversal = do
  let sel = ev [(1, ())]
      x = Constant (1 :: Int)
      q = Constant 2
      def fa =
        let p = MapC (+ 10) fa
            b = switchCell (Hold p (MapS (const q) sel) b0) b0
            y = MapC (+ 100) b
            a = switchCell (Hold x (MapS (const y) sel) b0) b0
         in (a, b, p, y)
      go k prev =
        let (a, _, _, _) = def (concrete prev)
            next = steps a
         in if next == prev || k > (50 :: Int) then (prev, k) else go (k + 1) next
      (fixed, rounds) = go 1 (fst (steps (let (a, _, _, _) = def (Constant 0) in a)), [])
      (a, b, p, y) = def (concrete fixed)
  line "reversal: rounds" rounds
  line "reversal: steps a" (steps a)
  line "reversal: steps b" (steps b)
  line "reversal: samples a, b, p, y" (samples a 2, samples b 2, samples p 2, samples y 2)

-- 16. A switch cell whose outer is a map cell of a boolean input cell: at a
--     switch instant the value after it goes through the map cell's value
--     after the instant, and each new inner steps there.
mapOuter :: IO ()
mapOuter = do
  let pick = Hold False (ev [(2, True), (4, False)]) b0
      c1 = Hold 1 (ev [(1, 2), (2, 3), (3, 4), (4, 5)]) b0
      c2 = Hold 10 (ev [(2, 20), (3, 30)]) b0
      outer = MapC (\p -> if p then c2 else c1) pick
      sw = switchCell outer b0
  line "map outer: steps sw" (steps sw :: C Int)

-- 17. Two switch_streams trade linear streams in one instant: at [2], A
--     moves from s1 to s2 as B moves from s3 to s1, each through a switch
--     cell over constant cells holding the streams.
trade :: IO ()
trade = do
  let s k = ev [(t, t * 10 + k) | t <- [1 .. 3]]
      sel = ev [(2, ())]
      via from to = SwitchS (switchCell (Hold (Constant from) (MapS (const (Constant to)) sel) b0) b0)
  line "trade: a" (occs (via (s 1) (s 2)) :: S Int)
  line "trade: b" (occs (via (s 3) (s 1)) :: S Int)

-- 18. A switch cell whose outer steps in child instants: a split of a list
--     of cells, held. The switch moves at each child's commit, each child a
--     switch instant; a switch_stream whose outer steps in the children
--     forwards, in each child, the stream selected before it. F7's sorted
--     Split, as in stage4-ghc/Stage4.hs.
children :: IO ()
children = do
  let x1 = ev [(2, 5)]
      x2 = ev [(1, 20), (3, 21)]
      c1 = Hold 1 x1 b0
      c2 = Hold 2 x2 b0
      c3 = Constant 3
      lists = ev [(1, [c2, c3]), (2, [c1]), (3, [c2])]
      splitSorted s = MkStream (sortOn fst (occs (Split s)))
      outer = Hold c1 (splitSorted lists) b0
      sw = switchCell outer b0
  line "children: steps sw" (steps sw :: C Int)
  line "children: samples" (samples sw 3)
  -- Every split of an instant shares its children: child n carries
  -- element n of each, a selection and two events.
  let a = splitSorted (ev [(1, "ab"), (2, "c")])
      z = splitSorted (ev [(1, "yz"), (2, "x")])
      picks = splitSorted (ev [(1, [z, a]), (2, [z])])
      streamOuter = Hold a picks b0
  line "children: switch_stream" (occs (SwitchS streamOuter))

main :: IO ()
main = do
  vectors
  claim5
  review
  switchStreamProbes
  quietInner
  creation
  nested
  throughMemo
  newInnerFires
  stateSwitch
  sameInner
  otherOuters
  streamCreation
  laterInner
  reversal
  mapOuter
  trade
  children
