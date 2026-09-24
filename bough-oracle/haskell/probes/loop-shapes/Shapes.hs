{-# LANGUAGE GADTs #-}
{-# LANGUAGE RecordWildCards #-}
-- Loop shapes that RFD 1 and RFD 7 promise as fixed tests, written over the
-- constructors of Reactive.Sodium.Denotational (Sodium denotational semantics
-- 1.1, executable version), unchanged.
--
-- Every loop is computed twice:
--   * by explicit fixed-point iteration: each loop reference is a concrete
--     cell `Hold a (MkStream sts) []` replaying the previous iterate's steps,
--     starting from a cell that never steps; the program text is re-evaluated
--     unchanged until the loops' steps stop changing;
--   * by lazy knot-tying (Haskell recursion), where that terminates.
-- and the two are checked to agree.
--
-- Build:  ghc -O0 -outputdir build -i/home/user/sodiumfrp/sodium/denotational Shapes.hs -o shapes
-- Run:    ./shapes            (all shapes and checks)
--         ./shapes knot-3c    (lazy knot of shape 3c; expected not to return)
module Main (main) where

import Control.Monad (forM_, unless, when)
import Data.IORef
import Data.List (intercalate)
import Reactive.Sodium.Denotational
import System.Environment (getArgs)
import System.Exit (exitFailure)
import System.IO (hFlush, stdout)

------------------------------------------------------------------------------
-- The fixed-point machinery

-- | A loop reference: a concrete cell replaying the given steps. t0 = [] is the
-- least time, so every step is kept.
replay :: C a -> Cell a
replay (a, sts) = Hold a (MkStream sts) []

-- | Solve n same-typed loops. `def` maps the n loop references to the n loop
-- definitions (the program text with the forward tokens as parameters).
-- Returns the loops' steps at the fixed point and the number of times the
-- program text was evaluated, the last one being the evaluation that
-- reproduced its own input.
fixLoops :: Eq a => Int -> ([Cell a] -> [Cell a]) -> ([C a], Int)
fixLoops n def = go 1 start
  where
    unread = Hold (error "a loop's initial value read its own loop") Never []
    -- The first iterate: each loop's own initial value, never stepping.
    start = [(fst (steps c), []) | c <- def (replicate n unread)]
    go k cur =
      let next = map steps (def (map replay cur))
       in if next == cur then (cur, k) else go (k + 1) next

------------------------------------------------------------------------------
-- Helpers

satSub :: Int -> Int -> Int -- u32::saturating_sub
satSub a b = max 0 (a - b)

clampTo :: Int -> Int -> Int -- (x as i64).clamp(0, max) as u32
clampTo m x = max 0 (min m x)

buildTime :: T
buildTime = [0]

showT :: T -> String
showT t = "[" ++ intercalate "," (map show t) ++ "]"

showS :: Show a => S a -> String
showS [] = "(none)"
showS sts = unwords [showT t ++ " " ++ show a | (t, a) <- sts]

showC :: Show a => C a -> String
showC (a, sts) = "initial " ++ show a ++ "; steps " ++ showS sts

------------------------------------------------------------------------------
-- Inputs: one list of (time, value) per input. Build is at [0]; each later
-- top-level time [k] is one transaction.

data Inputs = Inputs {heal :: S Int, damage :: S Int, levelUp :: S Int}

-- | The instant of bevy-sodium record 0003 and of sodium-rust#52:
-- heal 100, level up +100, damage 50, all in one transaction.
record0003 :: Inputs
record0003 = Inputs {heal = [([1], 100)], damage = [([1], 50)], levelUp = [([1], 100)]}

-- | A longer schedule for the slice: full absorption, partial absorption,
-- heal and damage merged, heal clamped by the maximum from before the instant,
-- a step to an equal value, damage to zero, heal from zero.
longRun :: Inputs
longRun =
  Inputs
    { damage = [([1], 10), ([2], 50), ([3], 30), ([6], 300)],
      heal = [([2], 100), ([4], 500), ([5], 100), ([7], 40)],
      levelUp = [([2], 100), ([4], 50)]
    }

showInputs :: Inputs -> [String]
showInputs Inputs {..} =
  [ "  heal     " ++ showS heal,
    "  damage   " ++ showS damage,
    "  level_up " ++ showS levelUp
  ]

------------------------------------------------------------------------------
-- Shapes 1 and 2: the health-and-shield slice (bevy-sodium 0003 stage 2), and
-- the sodium-rust#52 reproduction, which is the same program.
--
-- `Drive` picks the form of health's update. `Full` is the program exactly as
-- 0003-lift2-loop-bug.rs and 0003-health-shield-frp.rs write it. The others
-- are the reductions sodium-rust#52 reports, reconstructed from its prose.

data Drive
  = Full -- merge healed took, snapshot3 reading health and max_health
  | FullPairCell -- the same, with snapshot3 as a snapshot of lift2 (,) health max_health
  | NoMaxRead -- #52: third cell read removed, still reproduced upstream
  | NoMerge -- #52: took drives health directly, did not reproduce upstream
  | OnlyWhenChanged -- shape 3c: health steps only when its value changes
  deriving (Eq, Show, Enum, Bounded)

data Slice = Slice
  { maxHealth :: Cell Int,
    shield :: Cell Int,
    health :: Cell Int,
    took :: Stream Int,
    delta :: Stream Int,
    fraction :: Cell Double, -- max_health.lift2(&health, ..)
    effective :: Cell Int -- health.lift2(&shield, ..)
  }

-- | The program text. The list holds the three forward tokens:
-- max_loop.cell(), shield_loop.cell(), health_loop.cell(). Every other read of
-- a looped cell reads the held cell itself, as the Rust does.
slice :: Drive -> Inputs -> [Cell Int] -> Slice
slice drive Inputs {..} ~[maxFwd, shieldFwd, healthFwd] = Slice {..}
  where
    sHeal = MkStream heal
    sDamage = MkStream damage
    sLevelUp = MkStream levelUp
    -- level_up.snapshot(&max_loop.cell(), |e, c| c + e).hold(100)
    maxHealth = Hold 100 (Snapshot (\e c -> c + e) sLevelUp maxFwd) buildTime
    -- damage.snapshot(&shield_loop.cell(), |d, s| s.saturating_sub(d)).hold(30)
    shield = Hold 30 (Snapshot (\d s -> s `satSub` d) sDamage shieldFwd) buildTime
    -- heal.map(|h| h as i64)
    healed = MapS id sHeal
    -- damage.snapshot(&shield, |d, s| -(d.saturating_sub(s) as i64))
    took = Snapshot (\d s -> negate (d `satSub` s)) sDamage shield
    -- healed.merge(&took, |a, b| a + b)
    delta = case drive of
      NoMerge -> took
      _ -> Merge healed took (+)
    -- delta.snapshot3(&health_loop.cell(), &max_health, clamp).hold(60)
    health = Hold 60 update buildTime
    clamped = Snapshot (\(d, cur) m -> clampTo m (cur + d)) (Snapshot (,) delta healthFwd) maxHealth
    update = case drive of
      Full -> clamped
      FullPairCell ->
        Snapshot (\d (cur, m) -> clampTo m (cur + d)) delta (Apply (MapC (,) healthFwd) maxHealth)
      NoMaxRead -> Snapshot (\d cur -> max 0 (cur + d)) delta healthFwd
      NoMerge -> clamped
      OnlyWhenChanged ->
        MapS fst $
          Filter (\(new, cur) -> new /= cur) $
            Snapshot (\(d, cur) m -> (clampTo m (cur + d), cur)) (Snapshot (,) delta healthFwd) maxHealth
    fraction = Apply (MapC (\m h -> fromIntegral h / fromIntegral m) maxHealth) health
    effective = Apply (MapC (+) health) shield

loopsOf :: Slice -> [Cell Int]
loopsOf s = [maxHealth s, shield s, health s]

sliceByFixpoint :: Drive -> Inputs -> (Slice, Int)
sliceByFixpoint drive inp = (slice drive inp (map replay sol), k)
  where
    (sol, k) = fixLoops 3 (loopsOf . slice drive inp)

sliceByKnot :: Drive -> Inputs -> Slice
sliceByKnot drive inp = s where s = slice drive inp (loopsOf s)

data Obs = Obs
  { oMax :: C Int,
    oShield :: C Int,
    oHealth :: C Int,
    oTook :: S Int,
    oDelta :: S Int,
    oFraction :: C Double,
    oEffective :: C Int
  }
  deriving (Eq, Show)

observe :: Slice -> Obs
observe Slice {..} =
  Obs
    { oMax = steps maxHealth,
      oShield = steps shield,
      oHealth = steps health,
      oTook = occs took,
      oDelta = occs delta,
      oFraction = steps fraction,
      oEffective = steps effective
    }

printObs :: Obs -> IO ()
printObs Obs {..} = do
  putStrLn $ "  max_health " ++ showC oMax
  putStrLn $ "  shield     " ++ showC oShield
  putStrLn $ "  took       " ++ showS oTook
  putStrLn $ "  delta      " ++ showS oDelta
  putStrLn $ "  health     " ++ showC oHealth
  putStrLn $ "  fraction   " ++ showC oFraction
  putStrLn $ "  effective  " ++ showC oEffective

-- | The final value of a cell after all its steps.
final :: C a -> a
final (a, sts) = last (a : map snd sts)

------------------------------------------------------------------------------
-- Shape 2, stage 1 of the slice: no shield, no damage (commit 20dfa33).

data Stage1 = Stage1 {s1Max :: Cell Int, s1Health :: Cell Int, s1Fraction :: Cell Double}

stage1 :: Inputs -> [Cell Int] -> Stage1
stage1 Inputs {..} ~[maxFwd, healthFwd] = Stage1 {..}
  where
    s1Max = Hold 100 (Snapshot (\e c -> c + e) (MkStream levelUp) maxFwd) buildTime
    -- heal.snapshot3(&health_loop.cell(), &max_health, |h, cur, max| (cur + h).min(max)).hold(60)
    s1Health =
      Hold 60 (Snapshot (\(h, cur) m -> min m (cur + h)) (Snapshot (,) (MkStream heal) healthFwd) s1Max) buildTime
    s1Fraction = Apply (MapC (\m h -> fromIntegral h / fromIntegral m) s1Max) s1Health

stage1Obs :: Stage1 -> (C Int, C Int, C Double)
stage1Obs Stage1 {..} = (steps s1Max, steps s1Health, steps s1Fraction)

------------------------------------------------------------------------------
-- Calibration: fact 1's capped counter, to confirm this fixLoops counts the
-- way the session's earlier fixed-point work did (11 iterations).

cappedCounter :: Cell Int -> Cell Int
cappedCounter c = Hold 0 (Filter (<= 10) (Snapshot (\_ n -> n + 1) ticks c)) [0]
  where
    ticks = MkStream [([k], ()) | k <- [1 .. 15]]

------------------------------------------------------------------------------
-- Checks

type Checker = String -> Bool -> IO ()

main :: IO ()
main = do
  args <- getArgs
  case args of
    ["knot-3c"] -> do
      putStrLn "shape 3c by lazy knot, health steps:"
      hFlush stdout
      putStrLn $ "  " ++ showC (steps (health (sliceByKnot OnlyWhenChanged longRun)))
    _ -> runAll

runAll :: IO ()
runAll = do
  failures <- newIORef (0 :: Int)
  let check :: Checker
      check name ok = do
        putStrLn $ (if ok then "  PASS " else "  FAIL ") ++ name
        unless ok $ modifyIORef failures (+ 1)

  putStrLn "== calibration: fact 1's capped counter"
  let ([cc], ccIters) = fixLoops 1 (map cappedCounter)
  putStrLn $ "  " ++ showC cc
  putStrLn $ "  program text evaluated " ++ show ccIters ++ " times"
  check "capped counter steps 1..10 at [1]..[10]" (cc == (0, [([k], k) | k <- [1 .. 10]]))

  --------------------------------------------------------------------------
  putStrLn ""
  putStrLn "== shape 1: sodium-rust#52 (bevy-sodium 0003-lift2-loop-bug.rs)"
  putStrLn "inputs:"
  mapM_ putStrLn (showInputs record0003)
  let (fx1, k1) = sliceByFixpoint Full record0003
      o1 = observe fx1
      kn1 = observe (sliceByKnot Full record0003)
  putStrLn $ "fixed point (program text evaluated " ++ show k1 ++ " times):"
  printObs o1
  check "lazy knot terminates and equals the fixed point" (kn1 == o1)
  let expected1 =
        Obs
          { oMax = (100, [([1], 200)]),
            oShield = (30, [([1], 0)]),
            oHealth = (60, [([1], 100)]),
            oTook = [([1], -20)],
            oDelta = [([1], 80)],
            oFraction = (0.6, [([1], 0.5)]),
            oEffective = (90, [([1], 100)])
          }
  check "matches the hand computation" (o1 == expected1)
  putStrLn "  the four rows of the reproduction, health after the instant:"
  forM_
    [ ("no lift2", Nothing),
      ("lift2(max_health, health)", Just "fraction"),
      ("lift2(health, shield)", Just "effective"),
      ("both", Just "both")
    ]
    $ \(label, lifted) -> do
      -- Each row is its own program; the lifts are separate definitions, so
      -- the oracle's health cannot differ between rows. Force the lifts anyway.
      let (s, _) = sliceByFixpoint Full record0003
          forced = case lifted of
            Nothing -> 0
            Just "fraction" -> length (snd (steps (fraction s)))
            Just "effective" -> length (snd (steps (effective s)))
            _ -> length (snd (steps (fraction s))) + length (snd (steps (effective s)))
          h = final (steps (health s))
      putStrLn $ "    " ++ label ++ replicate (28 - length label) ' ' ++ "health = " ++ show h ++ "  (lift steps forced: " ++ show forced ++ ")"
      check ("row " ++ show label ++ ": health = 100") (h == 100)
  let (fxPair, _) = sliceByFixpoint FullPairCell record0003
  check "snapshot3 as nested snapshots = snapshot of a lift2 pair cell" (observe fxPair == o1)
  check "fraction and effective each step once in the instant (5.14 no-glitch)" $
    length (snd (oFraction o1)) == 1 && length (snd (oEffective o1)) == 1

  --------------------------------------------------------------------------
  putStrLn ""
  putStrLn "== shape 2a: the health slice, stage 1 (no shield), record 0003's instant without damage"
  let st1Inputs = record0003 {damage = []}
      ([m1, h1], kS1) = fixLoops 2 (\refs -> let s = stage1 st1Inputs refs in [s1Max s, s1Health s])
      st1Fix = stage1Obs (stage1 st1Inputs (map replay [m1, h1]))
      st1Knot = let s = stage1 st1Inputs [s1Max s, s1Health s] in stage1Obs s
      (st1M, st1H, st1F) = st1Fix
  putStrLn $ "fixed point (program text evaluated " ++ show kS1 ++ " times):"
  putStrLn $ "  max_health " ++ showC st1M
  putStrLn $ "  health     " ++ showC st1H
  putStrLn $ "  fraction   " ++ showC st1F
  check "lazy knot terminates and equals the fixed point" (st1Knot == st1Fix)
  check "health 100 (clamped by the maximum from before the instant), max 200, fraction 0.5" $
    st1Fix == ((100, [([1], 200)]), (60, [([1], 100)]), (0.6, [([1], 0.5)]))

  --------------------------------------------------------------------------
  putStrLn ""
  putStrLn "== shape 2b: the health-and-shield slice (stage 2), long schedule"
  putStrLn "inputs:"
  mapM_ putStrLn (showInputs longRun)
  let (fx2, k2) = sliceByFixpoint Full longRun
      o2 = observe fx2
      kn2 = observe (sliceByKnot Full longRun)
  putStrLn $ "fixed point (program text evaluated " ++ show k2 ++ " times):"
  printObs o2
  check "lazy knot terminates and equals the fixed point" (kn2 == o2)
  let expected2 =
        Obs
          { oMax = (100, [([2], 200), ([4], 250)]),
            oShield = (30, [([1], 20), ([2], 0), ([3], 0), ([6], 0)]),
            oHealth = (60, [([1], 60), ([2], 100), ([3], 70), ([4], 200), ([5], 250), ([6], 0), ([7], 40)]),
            oTook = [([1], 0), ([2], -30), ([3], -30), ([6], -300)],
            oDelta = [([1], 0), ([2], 70), ([3], -30), ([4], 500), ([5], 100), ([6], -300), ([7], 40)],
            oFraction = (0.6, [([1], 0.6), ([2], 0.5), ([3], 0.35), ([4], 0.8), ([5], 1.0), ([6], 0.0), ([7], 0.16)]),
            oEffective = (90, [([1], 80), ([2], 100), ([3], 70), ([4], 200), ([5], 250), ([6], 0), ([7], 40)])
          }
  check "matches the hand computation" (o2 == expected2)
  let (fxPair2, _) = sliceByFixpoint FullPairCell longRun
  check "snapshot3 as nested snapshots = snapshot of a lift2 pair cell" (observe fxPair2 == o2)
  -- Apply's no-glitch rule: one step of each derived cell per instant in which
  -- any input steps.
  let stepTimes (_, sts) = map fst sts
      union a b = foldr (\t acc -> if t `elem` acc then acc else t : acc) b a
      sortT = foldr insertT []
      insertT t [] = [t]
      insertT t (u : us) = if t <= u then t : u : us else u : insertT t us
  check "fraction steps exactly at the union of max_health's and health's step times" $
    stepTimes (oFraction o2) == sortT (union (stepTimes (oMax o2)) (stepTimes (oHealth o2)))
  check "effective steps exactly at the union of health's and shield's step times" $
    stepTimes (oEffective o2) == sortT (union (stepTimes (oHealth o2)) (stepTimes (oShield o2)))

  --------------------------------------------------------------------------
  putStrLn ""
  putStrLn "== shape 3: the reductions sodium-rust#52 reports (reconstructed), record 0003's instant"
  forM_ [NoMaxRead, NoMerge] $ \drive -> do
    let (fx, k) = sliceByFixpoint drive record0003
        o = observe fx
        kn = observe (sliceByKnot drive record0003)
    putStrLn $ show drive ++ " (program text evaluated " ++ show k ++ " times):"
    printObs o
    check (show drive ++ ": lazy knot terminates and equals the fixed point") (kn == o)
    check
      (show drive ++ ": health after the instant = " ++ show (if drive == NoMaxRead then 140 else 40 :: Int))
      (final (oHealth o) == (if drive == NoMaxRead then 140 else 40))
  putStrLn "  (NoMaxRead and NoMerge on the long schedule:)"
  forM_ [NoMaxRead, NoMerge] $ \drive -> do
    let (fx, k) = sliceByFixpoint drive longRun
        o = observe fx
        kn = observe (sliceByKnot drive longRun)
    putStrLn $ "  " ++ show drive ++ " health " ++ showC (oHealth o) ++ "  [" ++ show k ++ " evaluations]"
    check (show drive ++ " long: lazy knot equals the fixed point") (kn == o)

  --------------------------------------------------------------------------
  putStrLn ""
  putStrLn "== shape 3c: health steps only when its value changes (not in the records), long schedule"
  let (fx3, k3) = sliceByFixpoint OnlyWhenChanged longRun
      o3 = observe fx3
  putStrLn $ "fixed point (program text evaluated " ++ show k3 ++ " times):"
  printObs o3
  check "health: no step at [1] (delta 0), otherwise as shape 2b" $
    oHealth o3 == (60, [([2], 100), ([3], 70), ([4], 200), ([5], 250), ([6], 0), ([7], 40)])
  check "everything but health, fraction and effective as shape 2b" $
    (oMax o3, oShield o3, oTook o3, oDelta o3) == (oMax o2, oShield o2, oTook o2, oDelta o2)
  putStrLn "  lazy knot: run `./shapes knot-3c` under a timeout; it is expected not to return."

  n <- readIORef failures
  putStrLn ""
  if n == 0 then putStrLn "all checks passed" else putStrLn (show n ++ " check(s) FAILED")
  when (n > 0) exitFailure
