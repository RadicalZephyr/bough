{-# LANGUAGE GADTs #-}
-- The loop rule's boundary: a steps view inside a loop whose cycle also
-- passes through a snapshot. x = hold 0 (snapshot ticks y_fwd (\t y -> y + t));
-- y = hold 100 (map (*2) (updates x_fwd)). Solved by fixed point and by the
-- lazy knot over the unchanged Denotational.hs.
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

main :: IO ()
main = do
  let ticks = MkStream [([k], k) | k <- [1, 2, 3]]
      def [xF, yF] =
        [ Hold 0 (Snapshot (\t y -> y + t) ticks yF) [0],
          Hold 100 (MapS (* 2) (Updates xF)) [0]
        ]
      ([x, y], n) = fixLoops 2 def
  putStrLn $ "fixed point x " ++ show (x :: C Int) ++ " y " ++ show y ++ " evals " ++ show n
  let [xk, yk] = def [xk, yk]
  putStrLn $ "lazy knot   x " ++ show (steps xk) ++ " y " ++ show (steps yk)
