{-# LANGUAGE GADTs #-}
{-# LANGUAGE RecordWildCards #-}
-- The health-and-shield slice of research-loop-shapes/Shapes.hs (program
-- text copied unchanged, `slice`), solved by the same fixLoops over the
-- unchanged Denotational.hs, plus what stage 3's tests add:
--   * ReadsForward: every read of a looped cell outside its own loop goes
--     through the forward token (took snapshots shield's forward; both lifts
--     are over forwards). At the fixed point a forward replays its
--     definition's steps, so the values must equal Full's.
--   * edgeRun, Verify.hs's third schedule, whose values it checked but did
--     not print.
--   * TwoSinks: the verification's two-input reading of #52 (no level_up,
--     no max_health; NoMaxRead's update), with shield looped or a plain hold.
module Main (main) where

import Data.List (intercalate)
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

satSub :: Int -> Int -> Int
satSub a b = max 0 (a - b)

clampTo :: Int -> Int -> Int
clampTo m x = max 0 (min m x)

buildTime :: T
buildTime = [0]

data Inputs = Inputs {heal :: S Int, damage :: S Int, levelUp :: S Int}

sched :: [(Int, Maybe Int, Maybe Int, Maybe Int)] -> Inputs
sched rows =
  Inputs
    { heal = [([k], v) | (k, Just v, _, _) <- rows],
      damage = [([k], v) | (k, _, Just v, _) <- rows],
      levelUp = [([k], v) | (k, _, _, Just v) <- rows]
    }

record0003, longRun, edgeRun :: Inputs
record0003 = sched [(1, Just 100, Just 50, Just 100)]
longRun =
  sched
    [ (1, Nothing, Just 10, Nothing),
      (2, Just 100, Just 50, Just 100),
      (3, Nothing, Just 30, Nothing),
      (4, Just 500, Nothing, Just 50),
      (5, Just 100, Nothing, Nothing),
      (6, Nothing, Just 300, Nothing),
      (7, Just 40, Nothing, Nothing)
    ]
edgeRun =
  sched
    [ (1, Just 40, Nothing, Nothing),
      (2, Nothing, Just 30, Just 10),
      (3, Just 1000, Just 5, Just 0),
      (4, Nothing, Nothing, Just 5)
    ]

data Drive = Full | NoMaxRead | NoMerge | OnlyWhenChanged | ReadsForward deriving (Eq, Show)

data Slice = Slice
  { maxHealth :: Cell Int,
    shield :: Cell Int,
    health :: Cell Int,
    took :: Stream Int,
    delta :: Stream Int,
    fraction :: Cell Double,
    effective :: Cell Int
  }

slice :: Drive -> Inputs -> [Cell Int] -> Slice
slice drive Inputs {..} ~[maxFwd, shieldFwd, healthFwd] = Slice {..}
  where
    sHeal = MkStream heal
    sDamage = MkStream damage
    sLevelUp = MkStream levelUp
    maxHealth = Hold 100 (Snapshot (\e c -> c + e) sLevelUp maxFwd) buildTime
    shield = Hold 30 (Snapshot (\d s -> s `satSub` d) sDamage shieldFwd) buildTime
    healed = MapS id sHeal
    shieldRead = if drive == ReadsForward then shieldFwd else shield
    took = Snapshot (\d s -> negate (d `satSub` s)) sDamage shieldRead
    delta = case drive of
      NoMerge -> took
      _ -> Merge healed took (+)
    health = Hold 60 update buildTime
    clamped = Snapshot (\(d, cur) m -> clampTo m (cur + d)) (Snapshot (,) delta healthFwd) maxHealth
    update = case drive of
      NoMaxRead -> Snapshot (\d cur -> max 0 (cur + d)) delta healthFwd
      OnlyWhenChanged ->
        MapS fst $
          Filter (\(new, cur) -> new /= cur) $
            Snapshot (\(d, cur) m -> (clampTo m (cur + d), cur)) (Snapshot (,) delta healthFwd) maxHealth
      _ -> clamped
    (fm, fh, eh, es) =
      if drive == ReadsForward
        then (maxFwd, healthFwd, healthFwd, shieldFwd)
        else (maxHealth, health, health, shield)
    fraction = Apply (MapC (\m h -> fromIntegral h / fromIntegral m) fm) fh
    effective = Apply (MapC (+) eh) es

loopsOf :: Slice -> [Cell Int]
loopsOf s = [maxHealth s, shield s, health s]

solve :: Drive -> Inputs -> (Slice, Int)
solve drive inp = (slice drive inp (map replay sol), k)
  where
    (sol, k) = fixLoops 3 (loopsOf . slice drive inp)

showT :: T -> String
showT t = "[" ++ intercalate "," (map show t) ++ "]"

showS :: Show a => S a -> String
showS [] = "(none)"
showS sts = unwords [showT t ++ " " ++ show a | (t, a) <- sts]

showC :: Show a => C a -> String
showC (a, sts) = "initial " ++ show a ++ "; steps " ++ showS sts

report :: String -> Drive -> Inputs -> IO ()
report name drive inp = do
  let (s, k) = solve drive inp
  putStrLn $ "== " ++ name ++ " " ++ show drive ++ " (" ++ show k ++ " evaluations)"
  putStrLn $ "  max_health " ++ showC (steps (maxHealth s))
  putStrLn $ "  shield     " ++ showC (steps (shield s))
  putStrLn $ "  took       " ++ showS (occs (took s))
  putStrLn $ "  delta      " ++ showS (occs (delta s))
  putStrLn $ "  health     " ++ showC (steps (health s))
  putStrLn $ "  fraction   " ++ showC (steps (fraction s))
  putStrLn $ "  effective  " ++ showC (steps (effective s))

-- The two-input reading: heal and damage; health = hold 60 (snapshot delta
-- health_fwd (\d cur -> max 0 (cur + d))); effective = lift2 health shield.
twoSinks :: Bool -> Inputs -> IO ()
twoSinks shieldLooped Inputs {..} = do
  let sHeal = MkStream heal
      sDamage = MkStream damage
      def [shieldFwd, healthFwd] =
        let shield =
              if shieldLooped
                then Hold 30 (Snapshot (\d s -> s `satSub` d) sDamage shieldFwd) buildTime
                else Hold 30 (MapS (\d -> 30 `satSub` d) sDamage) buildTime
            took = Snapshot (\d s -> negate (d `satSub` s)) sDamage shield
            delta = Merge (MapS id sHeal) took (+)
            health = Hold 60 (Snapshot (\d cur -> max 0 (cur + d)) delta healthFwd) buildTime
         in [shield, health]
      def _ = error "two loops"
      ([sh, h], k) = fixLoops 2 def
      effective = Apply (MapC (+) (replay h)) (replay sh)
  putStrLn $ "== two sinks, shield looped " ++ show shieldLooped ++ " (" ++ show k ++ " evaluations)"
  putStrLn $ "  shield     " ++ showC sh
  putStrLn $ "  health     " ++ showC h
  putStrLn $ "  effective  " ++ showC (steps effective)

main :: IO ()
main = do
  report "record0003" Full record0003
  report "longRun" Full longRun
  report "longRun" ReadsForward longRun
  report "record0003" ReadsForward record0003
  report "edgeRun" Full edgeRun
  report "edgeRun" ReadsForward edgeRun
  report "longRun" NoMaxRead longRun
  report "longRun" NoMerge longRun
  report "longRun" OnlyWhenChanged longRun
  report "record0003" NoMaxRead record0003
  report "record0003" NoMerge record0003
  twoSinks True record0003
  twoSinks False record0003
  twoSinks True longRun
  twoSinks False longRun
