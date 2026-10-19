{-# LANGUAGE GADTs #-}
-- Expected values for bough's stage 3 loop tests, computed over the
-- unchanged Reactive.Sodium.Denotational (Sodium 1.1) by explicit
-- fixed-point iteration (fact 2; the same method as
-- research-loop-shapes/Shapes.hs): each forward token is a concrete cell
-- replaying the previous iterate's steps, starting from one that never
-- steps, and the program text is re-evaluated until it reproduces itself.
-- A stream loop replays the previous iterate's events (Verify.hs's solveS).
--
-- Build is instant [0]; transaction k is [k].
module Main (main) where

import Reactive.Sodium.Denotational

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

at' :: Int -> T
at' k = [k]

ev :: [(Int, a)] -> Stream a
ev xs = MkStream [(at' k, a) | (k, a) <- xs]

-- 1. counter: count = hold 0 (snapshot ticks count_fwd (+1)); five ticks.
counter :: IO ()
counter = do
  let ticks = ev [(k, ()) | k <- [1 .. 5]]
      ([c], n) = fixLoops 1 (\[f] -> [Hold 0 (Snapshot (\_ v -> v + 1) ticks f) b0])
  putStrLn $ "counter: " ++ show (c :: C Int) ++ " evals " ++ show n

-- 2. fact 1's capped counter: hold 0 (filter (<= 10) (snapshot ticks c (+1))); twelve ticks.
capped :: IO ()
capped = do
  let ticks = ev [(k, ()) | k <- [1 .. 12]]
      ([c], n) = fixLoops 1 (\[f] -> [Hold 0 (Filter (<= 10) (Snapshot (\_ v -> v + 1) ticks f)) b0])
  putStrLn $ "capped: " ++ show (c :: C Int) ++ " evals " ++ show n

-- 3. a loop whose definition is an accumulate: two accumulators reading each other.
--    b_acc = accumulate (snapshot ticks a_fwd (+)) 0 (+)
--    a_acc = accumulate (snapshot ticks b_acc (*)) 1 (+); a_loop.close(a_acc)
--    accumulate f s0 = hold s0 (snapshot f stream self), the knot.
accumulatePair :: IO ()
accumulatePair = do
  let ticks = ev [(1, 1), (2, 2), (3, 3), (4, 4)]
      def [aF, bF] =
        [ Hold 1 (Snapshot (\x s -> s + x) (Snapshot (\t b -> t * b) ticks bF) aF) b0,
          Hold 0 (Snapshot (\x s -> s + x) (Snapshot (\t a -> t + a) ticks aF) bF) b0
        ]
      ([a, b], n) = fixLoops 2 def
  putStrLn $ "accumulate pair: a " ++ show (a :: C Int) ++ " b " ++ show b ++ " evals " ++ show n

-- 4. an accumulator reads itself through a read-through loop:
--    doubled = map_cell fwd (*2); acc = accumulate (snapshot ticks doubled (,)) 1 (\(t, d) s -> s + t + d)
accThroughRead :: IO ()
accThroughRead = do
  let ticks = ev [(1, 5), (2, 1), (3, 2)]
      def [f] = [Hold 1 (Snapshot (\(t, d) s -> s + t + d) (Snapshot (,) ticks (MapC (* 2) f)) f) b0]
      ([c], n) = fixLoops 1 def
  putStrLn $ "acc through read: " ++ show (c :: C Int) ++ " evals " ++ show n

-- 5. a loop whose definition steps in transaction zero:
--    start = steps_with_current (constant 10) at [0];
--    count = hold 0 (or_else start (snapshot ticks count_fwd (+1)))
txZero :: IO ()
txZero = do
  let ticks = ev [(1, ()), (2, ())]
      def [f] = [Hold 0 (Merge (Value (Constant 10) b0) (Snapshot (\_ v -> v + 1) ticks f) const) b0]
      ([c], n) = fixLoops 1 def
  putStrLn $ "tx zero: " ++ show (c :: C Int) ++ " evals " ++ show n

-- 6. diamonds through loops: x and y read each other through snapshots of
--    their forwards and both read level; lifted with level, and with a hold
--    of the shared ticks.
diamond :: IO ()
diamond = do
  let ticks = ev [(1, 1), (3, 2), (4, 5), (6, 0), (7, 3)]
      level = Hold 1 (ev [(2, 2), (3, 3), (5, 3), (6, 1)]) b0
      def [xF, yF] =
        [ Hold 1 (Snapshot (\(t, y) l -> (y + t * l) `mod` 1000) (Snapshot (,) ticks yF) level) b0,
          Hold 2 (Snapshot (\(t, x) l -> (x * 2 + t + l) `mod` 1000) (Snapshot (,) ticks xF) level) b0
        ]
      ([xs, ys], n) = fixLoops 2 def
      x = replay xs
      y = replay ys
      lastT = Hold 0 ticks b0
      both = Apply (Apply (MapC (\l x' y' -> l * 1000000 + x' * 1000 + y') level) x) y
      total = Apply (Apply (MapC (\x' y' t -> x' + y' + t) x) y) lastT
  putStrLn $ "diamond x: " ++ show xs ++ " evals " ++ show n
  putStrLn $ "diamond y: " ++ show ys
  putStrLn $ "diamond level: " ++ show (steps level)
  putStrLn $ "diamond both: " ++ show (steps both)
  putStrLn $ "diamond total: " ++ show (steps total)

-- 7. a stream loop through a hold read by snapshot:
--    fwd -> last = hold 0 fwd; fwd = snapshot ticks last (+)
streamThroughHold :: IO ()
streamThroughHold = do
  let ticks = ev [(1, 1), (2, 2), (3, 3), (4, 10)]
      def f = Snapshot (+) ticks (Hold 0 f b0)
  putStrLn $ "stream through hold: " ++ show (solveS 20 def :: Maybe (S Int, Int))
  case solveS 20 def of
    Just (s, _) -> putStrLn $ "  last: " ++ show (steps (Hold 0 (MkStream s) b0))
    Nothing -> putStrLn "  no fixed point"

-- 8. a state loop: log = accumulate_mut (snapshot ticks log_fwd (\t l -> (t, length l))) [] push
stateLoop :: IO ()
stateLoop = do
  let ticks = ev [(1, 1), (2, 2), (3, 3)]
      def [f] = [Hold [] (Snapshot (\(t, n) l -> l ++ [t * 10 + n]) (Snapshot (\t l -> (t, length l)) ticks f) f) b0]
      ([c], n) = fixLoops 1 def
  putStrLn $ "state loop: " ++ show (c :: C [Int]) ++ " evals " ++ show n

-- 9. R9: steps of the forward, the definition a hold over s; and a map_cell
--    over the forward with its steps.
r9 :: IO ()
r9 = do
  let s = ev [(1, 5), (2, 7), (3, 7)]
      ([c], _) = fixLoops 1 (\[_] -> [Hold 0 s b0])
      fwd = replay c
  putStrLn $ "r9 steps fwd: " ++ show (occs (Updates fwd) :: S Int)
  putStrLn $ "r9 steps (map_cell fwd (*3)): " ++ show (occs (Updates (MapC (* 3) fwd)))
  putStrLn $ "r9 value fwd at [0]: " ++ show (occs (Value fwd b0))

main :: IO ()
main = do
  counter
  capped
  accumulatePair
  accThroughRead
  txZero
  diamond
  streamThroughHold
  stateLoop
  r9
