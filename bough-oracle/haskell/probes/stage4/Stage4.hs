{-# LANGUAGE GADTs #-}
-- Expected values for bough's stage 4 child-transaction tests, computed over
-- an unchanged copy of Reactive.Sodium.Denotational (Sodium 1.1; SHA-256
-- d6772fb8...c822b, the file vendored in bough-oracle/haskell) with the
-- oracle's patch F7: Split's output sorted stably by time, exactly
-- bough-oracle's Oracle/Derived.hs `split`. `defer` is Split of a singleton
-- (Operational.java:46), as there.
--
-- Loops are solved by explicit fixed-point iteration (fact 2), the method of
-- stage3-ghc/Stage3.hs: a stream loop replays the previous iterate's events
-- from `MkStream []`; a cell loop replays the previous iterate's steps from a
-- cell that never steps.
--
-- Build is instant [0]; transaction k is [k]; the children of t are t ++ [n].
-- A sample from I/O after transaction k reads `at (steps c) [k + 1]`.
--
-- Build: ghc -i. -outputdir build -o stage4 Stage4.hs && ./stage4
module Main (main) where

import Data.List (sortOn)
import Reactive.Sodium.Denotational

-- | F7: the children sorted stably by time (Oracle/Derived.hs).
split :: Stream [a] -> Stream a
split s = MkStream (sortOn fst (occs (Split s)))

-- | The text's Split, for the one place its order differs.
splitText :: Stream [a] -> Stream a
splitText = Split

defer :: Stream a -> Stream a
defer s = split (MapS (: []) s)

replay :: C a -> Cell a
replay (a, sts) = Hold a (MkStream sts) []

fixLoops :: Eq a => Int -> ([Cell a] -> [Cell a]) -> ([C a], Int)
fixLoops n def = go 1 start
  where
    unread = Hold (error "a loop's initial value read its own loop") Never []
    start = [(fst (steps c), []) | c <- def (replicate n unread)]
    go k cur =
      let next = map steps (def (map replay cur))
       in if next == cur then (cur, k) else go (k + 1) next

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

line :: Show a => String -> a -> IO ()
line name v = putStrLn (name ++ ": " ++ show v)

-- 1. Two splits in one instant share child indices: child n takes item n of
--    each, and a merge combines the two items. f is not commutative.
twoSplits :: IO ()
twoSplits = do
  let l = ev [(1, [1, 2]), (3, []), (4, [5])]
      r = ev [(1, [30, 40, 50]), (2, [7, 8]), (3, [9]), (4, [])]
      merged = Merge (split l) (split r) (\a b -> a * 100 + b)
  line "two splits: merged" (occs merged :: S Int)
  line "two splits: hold of merged" (steps (Hold 0 merged b0))

-- 2. A split and a defer share child index 0; two defers in one instant are
--    simultaneous.
splitAndDefer :: IO ()
splitAndDefer = do
  let lists = ev [(1, [1, 2, 3]), (3, [6]), (4, [])]
      numbers = ev [(1, 40), (2, 5), (3, 7), (4, 8)]
      others = ev [(1, 9), (2, 4)]
      merged = Merge (split lists) (defer numbers) (\a b -> a * 100 + b)
      defers = Merge (defer numbers) (defer others) (\a b -> a * 10 + b)
  line "split and defer: merged" (occs merged :: S Int)
  line "split and defer: two defers" (occs defers :: S Int)

-- 3. Depth first: a split fed by its own children (the brief's program), and
--    beside it a split outside the loop fed in the same transaction, which
--    shares child indices with the loop's split at the top level only.
depthFirst :: IO ()
depthFirst = do
  let lists = ev [(1, [1, 2]), (2, [3])]
      others = ev [(1, [7, 8, 9])]
      def f = Merge lists (MapS (\n -> [n * 10, n * 10 + 1]) (Filter (< 10) (split f))) (\a _ -> a)
  case solveS 50 def of
    Nothing -> putStrLn "depth first: no fixed point"
    Just (fwd, k) -> do
      line "depth first: forward" (fwd :: S [Int])
      line "depth first: rounds" k
      line "depth first: items (F7 sorted)" (occs (split (MkStream fwd)))
      line "depth first: items (the text's Split)" (occs (splitText (MkStream fwd)))
      let both = Merge (split (MkStream fwd)) (split others) (\a b -> a * 1000 + b)
      line "depth first: merged with a split outside the loop" (occs both)

-- 4. R7: a defer-style loop through split, each item below 3 feeding back n + 1.
r7 :: IO ()
r7 = do
  let lists = ev [(1, [0])]
      def f = Merge lists (MapS (\n -> [n + 1]) (Filter (< 3) (split f))) (\a _ -> a)
  case solveS 50 def of
    Nothing -> putStrLn "r7: no fixed point"
    Just (fwd, k) -> do
      line "r7: items" (occs (split (MkStream fwd)) :: S Int)
      line "r7: rounds" k

-- 5. Nested splits, no loop: the inner split's children run inside the
--    outer's, before the outer's next child. Lengths are tagged 100 + n.
nested :: IO ()
nested = do
  let lists = ev [(1, [[1, 2], [3]]), (2, [[], [4]])]
      rows = split lists
      merged = Merge (MapS (\r -> 100 + length r) rows) (split rows) (\a b -> a * 1000 + b)
  line "nested: merged" (occs merged :: S Int)

-- 6. A hold stepping in one child is read by a snapshot in the next; its
--    steps, a map_cell's steps and an accumulator in the children.
holdInChildren :: IO ()
holdInChildren = do
  let lists = ev [(1, [5, 6, 7]), (2, [8])]
      items = split lists
      h = Hold 0 items b0
      seen = Snapshot (\i p -> i * 10 + p) items h
      doubled = MapC (* 2) h
      total = Hold 0 (Snapshot (+) items total) b0
  line "hold in children: seen" (occs seen :: S Int)
  line "hold in children: steps h" (occs (Updates h))
  line "hold in children: steps (map_cell h (*2))" (occs (Updates doubled))
  line "hold in children: total" (steps total)
  line "hold in children: sample h after [1], after [2]" (at (steps h) [2], at (steps h) [3])
  line "hold in children: sample total after [1], after [2]" (at (steps total) [2], at (steps total) [3])

-- 7. listen_steps on a cell stepping in two children: its steps from [1] on.
twoSteps :: IO ()
twoSteps = do
  let lists = ev [(1, [5, 6]), (2, [])]
      h = Hold 0 (split lists) b0
  line "two steps: steps h" (occs (Updates h) :: S Int)
  line "two steps: sample after [1], after [2]" (at (steps h) [2], at (steps h) [3])

-- 8. Transaction zero has children (F11): a defer of a steps_with_current
--    built in the build fires at [0, 0]; a split of one fires at [0, n].
txZero :: IO ()
txZero = do
  let level = Hold 3 (ev [(1, 4)]) b0
      current = Value level b0
      direct = Hold 0 current b0
      deferred = defer (MapS (* 10) current)
      held = Hold 0 deferred b0
      seen = Hold 0 (Snapshot (+) deferred direct) b0
      rows = Hold [1, 2, 3] (ev [(1, [4, 5])]) b0
      items = split (Value rows b0)
      sums = Hold 0 (Snapshot (+) items sums) b0
      lastItem = Hold 0 items b0
  line "tx zero: current" (occs current :: S Int)
  line "tx zero: deferred" (occs deferred)
  line "tx zero: held, direct, seen at [1]" (at (steps held) [1], at (steps direct) [1], at (steps seen) [1])
  line "tx zero: held, direct, seen after [1]" (at (steps held) [2], at (steps direct) [2], at (steps seen) [2])
  line "tx zero: split items" (occs items :: S Int)
  line "tx zero: sum, last at [1]" (at (steps sums) [1], at (steps lastItem) [1])
  line "tx zero: sum, last after [1]" (at (steps sums) [2], at (steps lastItem) [2])

-- 9. A countdown loop through defer: s = merge input (defer (filter (> 0) (map (- 1) s))).
countdown :: IO ()
countdown = do
  let input = ev [(1, 3), (2, 1), (3, 2)]
      def f = Merge input (defer (Filter (> 0) (MapS (subtract 1) f))) (\a _ -> a)
  case solveS 50 def of
    Nothing -> putStrLn "countdown: no fixed point"
    Just (s, k) -> do
      line "countdown: s" (s :: S Int)
      line "countdown: rounds" k
      line "countdown: hold after [1], [2], [3]" (let c = steps (Hold 0 (MkStream s) b0) in (at c [2], at c [3], at c [4]))
  -- F22: with no filter there is no fixed point.
  let unbounded f = Merge input (defer (MapS (subtract 1) f)) (\a _ -> a)
  line "countdown with no filter, 30 rounds" (fmap snd (solveS 30 unbounded :: Maybe (S Int, Int)))

-- 10. A cell loop through defer of the forward's steps: legal, where the same
--     loop without the defer is F3's same-instant cycle.
cellLoopThroughDefer :: IO ()
cellLoopThroughDefer = do
  let ticks = ev [(1, 1), (2, 0), (3, 2)]
      def [f] = [Hold 0 (Merge ticks (defer (MapS (+ 1) (Filter (< 3) (Updates f)))) const) b0]
      ([c], k) = fixLoops 1 def
  line "cell loop through defer: count" (c :: C Int)
  line "cell loop through defer: rounds" k

-- 11. A split of an empty list emits nothing.
emptyList :: IO ()
emptyList = do
  let lists = ev [(1, []), (2, [4])]
  line "empty list: items" (occs (split lists) :: S Int)

main :: IO ()
main = do
  twoSplits
  splitAndDefer
  depthFirst
  r7
  nested
  holdInChildren
  twoSteps
  txZero
  countdown
  cellLoopThroughDefer
  emptyList
