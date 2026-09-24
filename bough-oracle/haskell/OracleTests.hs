{-# LANGUAGE GADTs #-}
-- | The oracle's own tests, run by GHC.
--
-- * The research's tests of every Bough operation (research-derived-ops,
--   DerivedTests.hs) and of its verification (research-derived-ops-verify,
--   VerifyTests.hs), unchanged but for their imports, now over
--   Oracle.Derived.
-- * The two patches: F6 and F7 each show the text's answer and the patched
--   one on the case the patch fixes, and the twenty vectors of sodium.hs
--   give the same answer under the text and under the patches.
-- * Programs through the interpreter: the twenty vectors and the five
--   common-tests vectors restated as programs, the derived operations away
--   from [0], loops by fixed point (and, where the lazy knot terminates,
--   equal to it), the checks a malformed program fails, and the answers'
--   format.
--
-- Prints one line per test case, "ok" or "FAIL" with the message, then the
-- counts; exits nonzero on any failure.
module Main (main) where

import Test.HUnit
import System.Exit (exitFailure, exitSuccess)
import System.Timeout (timeout)
import Control.Monad (forM_)
import Data.Function (on)
import Data.List (groupBy, isInfixOf, isPrefixOf, nub, sort)
import Reactive.Sodium.Denotational (Stream(..), Cell(..), Reactive(..), T, S, C)
import qualified Reactive.Sodium.Denotational as D
import qualified Oracle.Derived as B
import Oracle.Derived (build)
import Oracle.Interpret
import Oracle.Program

main :: IO ()
main = do
    let onStart _ us = return us
        onProblem kind _ msg st us = do
            putStrLn (kind ++ " " ++ showPath (path st) ++ "\n    " ++ msg)
            return (path st : us)
    (counts, failed) <- performTest onStart (onProblem "ERROR") (onProblem "FAIL") [] tests
    mapM_ (\p -> if p `elem` failed then return () else putStrLn ("ok   " ++ showPath p))
          (testCasePaths tests)
    print counts
    if errors counts + failures counts == 0 then exitSuccess else exitFailure

tests :: Test
tests = TestList
    [ TestLabel "sodium.hs vectors through Derived" sodiumVectors
    , TestLabel "common-tests SemanticTests.hs, Denotational.hs times" commonVectors
    , TestLabel "Bough operations" boughTests
    , TestLabel "loops" loopTests
    , TestLabel "text versus Derived" textTests
    , TestLabel "verification: creation time" creationTests
    , TestLabel "verification: simultaneous events" simultaneousTests
    , TestLabel "verification: child-transaction times" childTests
    , TestLabel "verification: loops" verifyLoopTests
    , TestLabel "verification: text claims" verifyTextTests
    , TestLabel "patches" patchTests
    , TestLabel "interpreter: sodium.hs vectors" interpreterVectors
    , TestLabel "interpreter: common-tests vectors" interpreterCommonVectors
    , TestLabel "interpreter: derived operations" interpreterOperations
    , TestLabel "interpreter: loops" interpreterLoops
    , TestLabel "interpreter: checks" interpreterChecks
    , TestLabel "interpreter: answers" interpreterAnswers
    ]

------------------------------------------------------------------------------
-- The 20 vectors of denotational/sodium.hs, each through its Bough name.

sodiumVectors :: Test
sodiumVectors = test [
    "Never" ~: do
        assertEqual "e" [] (D.occs (B.never :: Stream ())),
    "MapS" ~: do
        let s1 = B.input [([0], 5), ([1], 10), ([2], 12)]
        assertEqual "s2" [([0],6),([1],11),([2],13)] (D.occs (B.map s1 (1+))),
    "Snapshot" ~: do
        let c = build (B.hold (B.input [([1], 4), ([5], 7)]) 3)
        let s1 = B.input [([0], 'a'), ([3], 'b'), ([5], 'c')]
        let s2 = B.snapshot s1 c (flip const)
        assertEqual "s2" [([0],3),([3],4),([5],4)] (D.occs s2)
        assertEqual "s3" (D.occs s2) (D.occs (B.snapshotViaExecute (flip const) s1 c)),
    "Merge" ~: do
        let s1 = B.input [([0], 0), ([2], 2)]
        let s2 = B.input [([1], 10), ([2], 20), ([3], 30)]
        assertEqual "s3" [([0],0),([1],10),([2],22),([3],30)] (D.occs (B.merge s1 s2 (+))),
    "Filter" ~: do
        let s1 = B.input [([0], 5), ([1], 6), ([2], 7)]
        assertEqual "s2" [([0],5),([2],7)] (D.occs (B.filter s1 odd)),
    "SwitchS" ~: do
        let s1 = B.input [([0], 'a'), ([1], 'b'), ([2], 'c'), ([3], 'd')]
        let s2 = B.input [([0], 'W'), ([1], 'X'), ([2], 'Y'), ([3], 'Z')]
        let c = build (B.hold (B.input [([1], s2)]) s1)
        assertEqual "s3" [([0],'a'),([1],'b'),([2],'Y'),([3],'Z')] (D.occs (B.switchStream c)),
    "Execute" ~: do
        let s2 = B.construct (B.input [([0], ())]) (\_ -> return 'a')
        assertEqual "s2" [([0],'a')] (D.occs s2),
    "Updates" ~: do
        let c = build (B.hold (B.input [([1], 'b'), ([3], 'c')]) 'a')
        assertEqual "s" [([1],'b'),([3],'c')] (D.occs (B.steps c)),
    "Value 1" ~: do
        let c = build (B.hold (B.input [([1], 'b'), ([3], 'c')]) 'a')
        assertEqual "s" [([0],'a'),([1],'b'),([3],'c')] (D.occs (build (B.stepsWithCurrent c))),
    "Value 2" ~: do
        let c = build (B.hold (B.input [([0], 'b'), ([1], 'c'), ([3], 'd')]) 'a')
        assertEqual "s" [([0],'b'),([1],'c'),([3],'d')] (D.occs (build (B.stepsWithCurrent c))),
    "Split" ~: do
        let s1 = B.input [([0], ['a', 'b']), ([1],['c'])]
        assertEqual "s2" [([0,0],'a'),([0,1],'b'),([1,0],'c')] (D.occs (B.split s1)),
    "Constant" ~: do
        assertEqual "c" ('a',[]) (D.steps (B.constant 'a')),
    "Hold" ~: do
        let c = build (B.hold (B.input [([1], 'b'), ([3], 'c')]) 'a')
        assertEqual "b" ('a',[([1],'b'),([3],'c')]) (D.steps c),
    "MapC" ~: do
        let c1 = build (B.hold (B.input [([2], 3), ([3], 5)]) 0)
        assertEqual "c2" (1,[([2],4),([3],6)]) (D.steps (B.mapCell c1 (1+))),
    "Apply" ~: do
        let cf = build (B.hold (B.input [([1], (5+)), ([3], (6+))]) (0+))
        let ca = build (B.hold (B.input [([1], 200), ([2], 300), ([4], 400)]) (100 :: Int))
        let expected = (100,[([1],205),([2],305),([3],306),([4],406)])
        assertEqual "cb lift" expected (D.steps (B.lift2 (cf, ca) (\f a -> f a)))
        assertEqual "cb apply" expected (D.steps (B.apply cf ca)),
    "SwitchC 1" ~: do
        let c1 = build (B.hold (B.input [([0], 'b'), ([1], 'c'), ([2], 'd'), ([3], 'e')]) 'a')
        let c2 = build (B.hold (B.input [([0], 'W'), ([1], 'X'), ([2], 'Y'), ([3], 'Z')]) 'V')
        let c3 = build (B.hold (B.input [([1], c2)]) c1)
        assertEqual "c4" ('a',[([0],'b'),([1],'X'),([2],'Y'),([3],'Z')]) (D.steps (build (B.switchCell c3))),
    "SwitchC 2" ~: do
        let c1 = build (B.hold (B.input [([0], 'b'), ([1], 'c'), ([2], 'd'), ([3], 'e')]) 'a')
        let c2 = build (B.hold (B.input [([1], 'X'), ([2], 'Y'), ([3], 'Z')]) 'W')
        let c3 = build (B.hold (B.input [([1], c2)]) c1)
        assertEqual "c4" ('a',[([0],'b'),([1],'X'),([2],'Y'),([3],'Z')]) (D.steps (build (B.switchCell c3))),
    "SwitchC 3" ~: do
        let c1 = build (B.hold (B.input [([0], 'b'), ([1], 'c'), ([2], 'd'), ([3], 'e')]) 'a')
        let c2 = build (B.hold (B.input [([2], 'Y'), ([3], 'Z')]) 'X')
        let c3 = build (B.hold (B.input [([1], c2)]) c1)
        assertEqual "c4" ('a',[([0],'b'),([1],'X'),([2],'Y'),([3],'Z')]) (D.steps (build (B.switchCell c3))),
    "SwitchC 4" ~: do
        let c1 = build (B.hold (B.input [([0], 'b'), ([1], 'c'), ([2], 'd'), ([3], 'e')]) 'a')
        let c2 = build (B.hold (B.input [([0], 'W'), ([1], 'X'), ([2], 'Y'), ([3], 'Z')]) 'V')
        let c3 = build (B.hold (B.input [([0], '2'), ([1], '3'), ([2], '4'), ([3], '5')]) '1')
        let c4 = build (B.hold (B.input [([1], c2), ([3], c3)]) c1)
        assertEqual "c5" ('a',[([0],'b'),([1],'X'),([2],'Y'),([3],'5')]) (D.steps (build (B.switchCell c4))),
    "Sample" ~: do
        let c = build (B.hold (B.input [([1], 'b')]) 'a')
        assertEqual "a" 'a' (run (B.sample c) [1])
        assertEqual "a" 'b' (run (B.sample c) [2])
  ]

------------------------------------------------------------------------------
-- The five vectors of common-tests/SemanticTests.hs. Times are the ones
-- Denotational.hs gives. Each runs verbatim (shift 0) and moved one
-- transaction later (shift 1), where no input fires in transaction zero, as
-- in Bough.

shiftT :: Int -> T -> T
shiftT k (h : rest) = (h + k) : rest
shiftT _ [] = []

sh :: Int -> S a -> S a
sh k = map (\(t, a) -> (shiftT k t, a))

-- Values grouped by top-level instant: all the common tests compare.
perInstant :: S a -> [(Int, [a])]
perInstant s = [ (head (fst (head g)), map snd g) | g <- groupBy ((==) `on` (head . fst)) s ]

commonVectors :: Test
commonVectors = TestList [ TestLabel ("shift " ++ show k) (commonAt k) | k <- [0, 1] ]

commonAt :: Int -> Test
commonAt k = test [
    "split" ~: do
        let a = B.input (sh k [([0], ["a", "b"])])
        let got = D.occs (B.split a)
        assertEqual "Denotational.hs times" (sh k [([0,0], "a"), ([0,1], "b")]) got
        -- The common test's own vector, [0,0] twice, agrees per instant.
        assertEqual "per instant" (perInstant (sh k [([0,0], "a"), ([0,0], "b")])) (perInstant got),
    "defer1" ~: do
        let a = B.input (sh k [([0],"a"),([1],"b")])
        assertEqual "b" (sh k [([0,0], "a"), ([1,0], "b")]) (D.occs (B.defer a)),
    "defer2" ~: do
        let a = B.input (sh k [([0],"a"),([1],"b")])
        let b = B.input (sh k [([1],"B")])
        assertEqual "c" (sh k [([0,0], "a"), ([1], "B"), ([1,0], "b")]) (D.occs (B.orElse (B.defer a) b)),
    "orElse1" ~: do
        let a = B.input (sh k [([0], 0 :: Int), ([2], 2)])
        let b = B.input (sh k [([1], 10 :: Int), ([2], 20), ([3], 30)])
        assertEqual "c" (sh k [([0],0),([1],10),([2],2),([3],30)]) (D.occs (B.orElse a b)),
    "deferSimultaneous" ~: do
        let a = B.input (sh k [([1],"b")])
        let b = B.input (sh k [([0],"A"),([1],"B")])
        assertEqual "c" (sh k [([0,0], "A"), ([1,0], "b")]) (D.occs (B.orElse (B.defer a) (B.defer b)))
  ]

------------------------------------------------------------------------------
-- The Bough operations the sodium.hs vectors do not reach.

ints :: S Int
ints = [([1], 1), ([2], 2), ([3], 3)]

boughTests :: Test
boughTests = test [
    "input: Bough inputs fire at external times only" ~: do
        assertBool "ints" (B.boughInput ints)
        assertBool "transaction zero" (not (B.boughInput [([0], 'a')]))
        assertBool "child time" (not (B.boughInput [([1,0], 'a')])),
    "input_coalescing: left fold, first send on the left" ~: do
        let sends = [([1],1),([1],2),([1],3),([2],10),([4],7),([4],1)] :: [(T, Int)]
        let expected = [([1],(1-2)-3),([2],10),([4],6)]
        assertEqual "coalesce" expected (D.occs (B.inputCoalescing (-) sends))
        assertEqual "by merge" expected (D.occs (B.inputCoalescingByMerge (-) sends))
        assertBool "well formed" (B.wellFormed (D.occs (B.inputCoalescing (-) sends))),
    "input_cell" ~: do
        assertEqual "c" (0,[([1],5),([3],6)]) (D.steps (build (B.inputCell (0 :: Int) [([1],5),([3],6)]))),
    "input_cell_coalescing folds before the hold" ~: do
        let sends = [([1],1),([1],2),([2],5)] :: [(T, Int)]
        assertEqual "coalescing" (0,[([1],3),([2],5)]) (D.steps (build (B.inputCellCoalescing 0 (+) sends)))
        -- Hold's own coalesce is last-wins: a different cell.
        assertEqual "raw hold" (0,[([1],2),([2],5)]) (D.steps (build (B.hold (MkStream sends) 0))),
    "constant is Hold of Never at any t0" ~: do
        assertEqual "[0]" (D.steps (B.constant 'a')) (D.steps (Hold 'a' Never [0]))
        assertEqual "[5]" (D.steps (B.constant 'a')) (D.steps (Hold 'a' Never [5])),
    "filter_map" ~: do
        let s = B.input [([1],1),([2],2),([3],3),([4],4 :: Int)]
        let f n = if even n then Just (n * 10) else Nothing
        assertEqual "evens" [([2],20),([4],40)] (D.occs (B.filterMap s f))
        let o = B.input [([1], Just 'x'), ([2], Nothing), ([3], Just 'z')]
        assertEqual "filterOptional" [([1],'x'),([3],'z')] (D.occs (B.filterMap o id)),
    "map_to" ~: do
        assertEqual "x" [([1],'x'),([2],'x'),([3],'x')] (D.occs (B.mapTo (B.input ints) 'x')),
    "gate reads the cell before the instant" ~: do
        let c = build (B.inputCell True [([2], False), ([3], True)])
        assertEqual "g" [([1],1),([2],2)] (D.occs (B.gate (B.input ints) c)),
    "once from the build" ~: do
        let s = B.input [([1],'a'),([2],'b'),([3],'c')]
        assertEqual "direct" [([1],'a')] (D.occs (build (B.once s)))
        assertEqual "loop" [([1],'a')] (D.occs (build (B.onceLoop s))),
    "once created at [2] sees the event at [2]" ~: do
        let s = B.input [([1],'a'),([2],'b'),([3],'c')]
        assertEqual "direct" [([2],'b')] (D.occs (run (B.once s) [2]))
        let loopOccs = D.occs (run (B.onceLoop s) [2])
        assertEqual "loop from t0" [([2],'b')] (filter ((>= [2]) . fst) loopOccs)
        assertEqual "loop before t0 repeats the input" [([1],'a'),([2],'b')] loopOccs,
    "once built by construct and switched in loses its event" ~: do
        let s = B.input [([1],'a'),([2],'b'),([3],'c')]
        let onces = B.construct (B.input [([2], ())]) (\_ -> B.once s)
        let sel = build (B.hold onces B.never)
        assertEqual "switch_stream uses the old stream at [2]" [] (D.occs (B.switchStream sel)),
    "accumulate" ~: do
        let c = build (B.accumulate (B.input ints) 0 (+))
        assertEqual "c" (0,[([1],1),([2],3),([3],6)]) (D.steps c)
        assertEqual "snapshot sees the old state" [([1],0),([2],1),([3],3)]
            (D.occs (B.snapshot (B.input ints) c (\_ n -> n))),
    "accumulate created at [2]" ~: do
        assertEqual "c" (0,[([2],2),([3],5)]) (D.steps (run (B.accumulate (B.input ints) 0 (+)) [2])),
    "accumulate_mut equals accumulate" ~: do
        let push a xs = xs ++ [a]
        assertEqual "c" (D.steps (build (B.accumulate (B.input ints) [] push)))
                        (D.steps (build (B.accumulateMut (B.input ints) [] push)))
        assertEqual "vec" ([],[([1],[1]),([2],[1,2]),([3],[1,2,3])])
                          (D.steps (build (B.accumulateMut (B.input ints) [] push))),
    "scan (Sodium collect)" ~: do
        let s = B.input [([1],'a'),([2],'b'),([3],'c')]
        let out = build (B.scan s (0 :: Int) (\a n -> ((a, n), n + 1)))
        assertEqual "o" [([1],('a',0)),([2],('b',1)),([3],('c',2))] (D.occs out),
    "share and node are identities" ~: do
        assertEqual "share" ints (D.occs (B.share (B.input ints)))
        assertEqual "node" ints (D.occs (B.node (B.input ints))),
    "merge calls f left right" ~: do
        let l = B.input [([1], "L")]
        let r = B.input [([1], "R")]
        assertEqual "m" [([1], "LR")] (D.occs (B.merge l r (++)))
        assertEqual "orElse" [([1], "L")] (D.occs (B.orElse l r)),
    "split children run depth first and share indices" ~: do
        let x = B.input [([1], ["a", "b"])]
        let y = B.input [([1], ["a", "b"])]
        let got = D.occs (B.merge (B.split x) (B.defer (B.split y)) (++))
        assertEqual "o" [([1,0],"a"),([1,0,0],"a"),([1,1],"b"),([1,1,0],"b")] got
        assertBool "well formed" (B.wellFormed got),
    "split of an empty list fires nothing" ~: do
        assertEqual "o" [([2,0],'z')] (D.occs (B.split (B.input [([1], []), ([2], "z")]))),
    "construct: sample reads the value before the instant" ~: do
        let c = build (B.inputCell (0 :: Int) [([2], 5)])
        let out = B.construct (B.input [([2], ()), ([3], ())]) (\_ -> B.sample c)
        assertEqual "o" [([2],0),([3],5)] (D.occs out),
    "construct: a hold built at [2] takes the event at [2]" ~: do
        let s2 = B.input [([2],'x'),([3],'y')]
        let cells = B.construct (B.input [([2], ())]) (\_ -> B.hold s2 '?')
        let holder = build (B.hold cells (B.constant '-'))
        let sw = build (B.switchCell holder)
        assertEqual "c" ('-',[([0],'-'),([2],'x'),([3],'y')]) (D.steps sw),
    "switch_cell steps at creation, so steps fires at [0]" ~: do
        let holder = build (B.hold B.never (B.constant 'q'))
        assertEqual "s" [([0],'q')] (D.occs (B.steps (build (B.switchCell holder)))),
    "lift3: two inputs stepping at one instant are one step" ~: do
        let a = build (B.inputCell 1 [([1],2),([2],3)])
        let b = build (B.inputCell 10 [([2],20)])
        let c = build (B.inputCell (100 :: Int) [([3],300)])
        assertEqual "l" ((1,10,100),[([1],(2,10,100)),([2],(3,20,100)),([3],(3,20,300))])
            (D.steps (B.lift3 (a, b, c) (\x y z -> (x, y, z)))),
    "lift4, lift5, lift6" ~: do
        let k n = build (B.inputCell n [([1], n * 2)]) :: Cell Int
        assertEqual "4" ([1,2,3,4],[([1],[2,4,6,8])])
            (D.steps (B.lift4 (k 1, k 2, k 3, k 4) (\a b c d -> [a, b, c, d])))
        assertEqual "5" ([1,2,3,4,5],[([1],[2,4,6,8,10])])
            (D.steps (B.lift5 (k 1, k 2, k 3, k 4, k 5) (\a b c d e -> [a, b, c, d, e])))
        assertEqual "6" ([1,2,3,4,5,6],[([1],[2,4,6,8,10,12])])
            (D.steps (B.lift6 (k 1, k 2, k 3, k 4, k 5, k 6) (\a b c d e g -> [a, b, c, d, e, g]))),
    "steps_with_current built in transaction zero fires at [0]" ~: do
        let v = build (B.stepsWithCurrent (B.constant (7 :: Int)))
        assertEqual "v" [([0],7)] (D.occs v)
        let h = build (B.stepsWithCurrent (B.constant (7 :: Int)) >>= \s -> B.hold s 0)
        assertEqual "held" (0,[([0],7)]) (D.steps h),
    "transaction zero has child transactions" ~: do
        let d = build (B.defer <$> B.stepsWithCurrent (B.constant 'k'))
        assertEqual "d" [([0,0],'k')] (D.occs d),
    "steps fires on a step to an equal value" ~: do
        assertEqual "s" [([1],0)] (D.occs (B.steps (build (B.inputCell (0 :: Int) [([1],0)])))),
    "sample in the build and from I/O" ~: do
        let c = build (B.inputCell (0 :: Int) [([1],5)])
        assertEqual "build" 0 (build (B.sample c))
        assertEqual "after 0" 0 (B.ioSample c 0)
        assertEqual "after 1" 5 (B.ioSample c 1)
        let h = build (B.hold (B.split (B.input [([1],[1,2,3 :: Int])])) 0)
        assertEqual "children run before send returns" 3 (B.ioSample h 1)
  ]

------------------------------------------------------------------------------
-- Loops, computed by fixed-point iteration (and by the lazy knot where it
-- terminates).

ticks :: Int -> Stream ()
ticks n = B.input [ ([k], ()) | k <- [1 .. n] ]

loopTests :: Test
loopTests = test [
    "capped counter: filter inside a cell loop" ~: do
        let body c = B.hold (B.filter (B.snapshot (ticks 15) c (\_ n -> n + 1)) (<= 10)) (0 :: Int)
        let (c, n) = build (B.cellLoopCount body)
        assertEqual "steps" (0, [ ([k], k) | k <- [1 .. 10] ]) (D.steps c)
        assertEqual "iterations" 11 n,
    "uncapped counter: fixed point equals the lazy knot" ~: do
        let body c = B.hold (B.snapshot (ticks 8) c (\_ n -> n + 1)) (0 :: Int)
        let fixed = build (B.cellLoop body)
        let knot = build (B.cellLoopKnot body)
        assertEqual "fixed" (0, [ ([k], k) | k <- [1 .. 8] ]) (D.steps fixed)
        assertEqual "knot" (D.steps fixed) (D.steps knot),
    "accumulate as a cell loop equals the knot" ~: do
        let viaLoop = build (B.cellLoop (\fwd -> B.hold (B.snapshot (B.input ints) fwd (+)) 0))
        assertEqual "c" (D.steps (build (B.accumulate (B.input ints) 0 (+)))) (D.steps viaLoop),
    "scan as a stream loop equals the knot" ~: do
        let s = B.input [([1],'a'),([2],'b'),([3],'c')]
        let f a n = ((a, n), n + 1 :: Int)
        let viaLoop = build (B.streamLoop (\states -> do
                st <- B.hold states 0
                return (B.map (B.snapshot s st f) snd)))
        let outputs = B.map (B.snapshot s (build (B.hold viaLoop 0)) f) fst
        assertEqual "o" (D.occs (build (B.scan s 0 f))) (D.occs outputs),
    "stream loop through defer with a filter inside" ~: do
        let ins = B.input [([1], 0), ([2], 5 :: Int)]
        let body fwd = return (B.orElse ins (B.defer (B.filter (B.map fwd (+1)) (< 3))))
        let (s, n) = build (B.streamLoopCount body)
        assertEqual "o" [([1],0),([1,0],1),([1,0,0],2),([2],5)] (D.occs s)
        assertEqual "iterations" 3 n
        assertBool "well formed" (B.wellFormed (D.occs s)),
    "once as a cell loop: gate inside the loop" ~: do
        let (active, n) = build (B.cellLoopCount (\fwd -> B.hold (B.mapTo (B.gate (B.input ints) fwd) False) True))
        assertEqual "active" (True,[([1],False)]) (D.steps active)
        assertEqual "iterations" 2 n
  ]

------------------------------------------------------------------------------
-- Where the text's own equations say more, or less, than Derived.

textTests :: Test
textTests = test [
    "switch_cell created after its outer stepped (the text's SwitchC)" ~: do
        let c1 = build (B.inputCell 'a' [])
        let c2 = build (B.inputCell 'x' [])
        let outer = build (B.hold (B.input [([1], c2)]) c1)
        let text = run (B.switchCellText outer) [3]
        let chopped = run (B.switchCell outer) [3]
        assertEqual "text" ('x',[([3],'a'),([1],'x')]) (D.steps text)
        assertBool "text is not well formed" (not (B.wellFormed (snd (D.steps text))))
        assertEqual "text value" [([3],'a')] (D.occs (run (B.stepsWithCurrent text) [3]))
        assertEqual "chopped" ('x',[([3],'x')]) (D.steps chopped)
        assertEqual "chopped value" [([3],'x')] (D.occs (run (B.stepsWithCurrent chopped) [3]))
        assertEqual "sample agrees" (map (D.at (D.steps chopped)) [[3],[4]]) (map (D.at (D.steps text)) [[3],[4]]),
    "switch_cell equals the text when the outer has no earlier step" ~: do
        let c1 = build (B.inputCell 'a' [([2],'b')])
        let c2 = build (B.inputCell 'x' [([3],'y')])
        let outer = build (B.hold (B.input [([2], c2)]) c1)
        assertEqual "c" (D.steps (build (B.switchCellText outer))) (D.steps (build (B.switchCell outer))),
    "Updates of a Hold is the held stream from t0 (section 5.8)" ~: do
        let s = B.input [([1],'a'),([3],'b')]
        let c = run (B.hold s '-') [2]
        assertEqual "s" [([3],'b')] (D.occs (B.steps c))
        assertEqual "coalesce" (D.occs (B.steps c))
            (D.occs (B.coalesceStream (flip const) (MkStream (filter ((>= [2]) . fst) (D.occs s))))),
    "Coalesce is the identity on a 1.1 stream" ~: do
        assertEqual "s" ints (D.occs (B.coalesceStream (+) (B.input ints))),
    "switcher (section 4) is hold then switch_cell" ~: do
        let c1 = build (B.inputCell 'a' [([1],'b')])
        let c2 = build (B.inputCell 'x' [([3],'y')])
        let s = B.input [([2], c2)]
        assertEqual "c" (D.steps (build (B.hold s c1 >>= B.switchCell))) (D.steps (B.switcher c1 s))
  ]
-- Helpers ------------------------------------------------------------------

-- Run a Reactive at a given time: the scope of a construct closure at t.
at :: T -> Reactive a -> a
at t r = run r t

-- Keep the events from t0 on: what anything created at t0 can observe.
from :: T -> S a -> S a
from t0 = filter ((>= t0) . fst)

-- A fixed-point iteration of a cell loop from an arbitrary first iterate.
fixCellFrom :: Eq a => C a -> (Cell a -> Cell a) -> Maybe (C a)
fixCellFrom start body = go (50 :: Int) (B.concreteCell start)
  where
    go 0 _ = Nothing
    go n prev =
        let next = D.steps (body prev)
        in if next == D.steps prev then Just next else go (n - 1) (B.concreteCell next)

-- The first n iterates of a stream loop from the stream that never fires.
streamIterates :: Int -> (Stream a -> Stream a) -> [S a]
streamIterates n body = take n (map D.occs (iterate (MkStream . D.occs . body) (MkStream [])))

-- The outer cell of the late-switch cases: c1 until [1], then c2.
lateOuter :: Cell Char -> Cell Char -> [(T, Cell Char)] -> Cell (Cell Char)
lateOuter c1 _ switches = build (B.hold (B.input switches) c1)

-- Creation time --------------------------------------------------------------

creationTests :: Test
creationTests = test [
    "hold created at a child time keeps that child's event and drops its parent's" ~: do
        let s = MkStream [([1], 'a'), ([2], 'b'), ([2,0], 'c'), ([3], 'd')]
        assertEqual "h" ('-', [([2,0],'c'),([3],'d')]) (D.steps (at [2,0] (B.hold s '-'))),
    "steps_with_current created at [2] after a step at [1]: fires [2] with the current value" ~: do
        let c = build (B.inputCell 'a' [([1],'b'),([4],'c')])
        assertEqual "v" [([2],'b'),([4],'c')] (D.occs (at [2] (B.stepsWithCurrent c))),
    "steps_with_current created at [2] when the cell steps at [2]: one event, the new value" ~: do
        let c = build (B.inputCell 'a' [([2],'b'),([4],'c')])
        assertEqual "v" [([2],'b'),([4],'c')] (D.occs (at [2] (B.stepsWithCurrent c))),
    "steps_with_current created at a child time [1,0]" ~: do
        let c = build (B.inputCell 'a' [([1],'b'),([1,1],'c')])
        assertEqual "v" [([1,0],'b'),([1,1],'c')] (D.occs (at [1,0] (B.stepsWithCurrent c))),
    "switch_cell created at [3], outer switched at [1] and switches again at [5]" ~: do
        let c1 = build (B.inputCell 'a' [])
        let c2 = build (B.inputCell 'x' [([4],'y')])
        let c3 = build (B.inputCell 'p' [])
        let outer = lateOuter c1 c2 [([1], c2), ([5], c3)]
        let chopped = at [3] (B.switchCell outer)
        let text = at [3] (B.switchCellText outer)
        assertEqual "chopped" ('x', [([3],'x'),([4],'y'),([5],'p')]) (D.steps chopped)
        assertEqual "text" ('x', [([3],'a'),([1],'x'),([4],'y'),([5],'p')]) (D.steps text),
    "switch_cell created at [3] when the outer switches at [3]: text and fix agree" ~: do
        let c1 = build (B.inputCell 'a' [([3],'b')])
        let c2 = build (B.inputCell 'x' [([3],'y')])
        let outer = lateOuter c1 c2 [([3], c2)]
        let expected = ('a', [([3],'y')])
        assertEqual "chopped" expected (D.steps (at [3] (B.switchCell outer)))
        assertEqual "text" expected (D.steps (at [3] (B.switchCellText outer))),
    "switch_cell created at [3] after a switch at [1], new inner steps at [3]" ~: do
        let c1 = build (B.inputCell 'a' [])
        let c2 = build (B.inputCell 'x' [([3],'y')])
        let outer = lateOuter c1 c2 [([1], c2)]
        assertEqual "chopped" ('x', [([3],'y')]) (D.steps (at [3] (B.switchCell outer)))
        -- The text: normalize is bound to the creation time [3], so it strips
        -- c2's step at [3] off the segment that starts at [1] and moves 'y' to [1].
        let text = at [3] (B.switchCellText outer)
        assertEqual "text" ('x', [([3],'a'),([1],'y')]) (D.steps text)
        -- So even sample is wrong: before [3] the cell holds 'x', the text says 'y'.
        assertEqual "fix sample at [3]" 'x' (at [3] (B.sample (at [3] (B.switchCell outer))))
        assertEqual "text sample at [3]" 'y' (at [3] (B.sample text)),
    "the text's late switch cell gives a hold the wrong value, not only a bad list" ~: do
        let c1 = build (B.inputCell 'a' [])
        let c2 = build (B.inputCell 'x' [])
        let outer = lateOuter c1 c2 [([1], c2)]
        -- In a construct at [3]: switch, then hold its steps.
        let viaText = at [3] (B.switchCellText outer >>= \sw -> B.hold (B.steps sw) '?')
        let viaFix  = at [3] (B.switchCell outer >>= \sw -> B.hold (B.steps sw) '?')
        assertEqual "text hold after [3]" 'a' (B.ioSample viaText 3)
        assertEqual "fix hold after [3]" 'x' (B.ioSample viaFix 3)
        assertEqual "the switch cell itself" 'x' (B.ioSample (at [3] (B.switchCell outer)) 3),
    "scan created at [2]: state starts at [2]" ~: do
        let s = B.input [([1],'a'),([2],'b'),([3],'c')]
        let out = at [2] (B.scan s (0 :: Int) (\a n -> ((a, n), n + 1)))
        assertEqual "o" [([2],('b',0)),([3],('c',1))] (from [2] (D.occs out)),
    "scan's output has no creation time: an event before t0 is in the denotation" ~: do
        let s = B.input [([1],'a'),([2],'b')]
        let out = at [2] (B.scan s (0 :: Int) (\a n -> (n, n + 1)))
        assertEqual "o" [([1],0),([2],0)] (D.occs out),
    "input_cell created in a construct at [2] never sees a send before [2]" ~: do
        -- Bough sends only to existing inputs; the Hold's t0 is then vacuous.
        let c = at [2] (B.inputCell 'a' [([3],'b')])
        assertEqual "c" ('a',[([3],'b')]) (D.steps c)
  ]

-- Simultaneous events --------------------------------------------------------

simultaneousTests :: Test
simultaneousTests = test [
    "two inputs in one transaction: merge calls f left right, hold takes one step" ~: do
        let l = B.input [([1], "L1"), ([2], "L2")]
        let r = B.input [([2], "R2"), ([3], "R3")]
        let m = B.merge l r (++)
        assertEqual "m" [([1],"L1"),([2],"L2R2"),([3],"R3")] (D.occs m)
        assertEqual "h" ("-",[([1],"L1"),([2],"L2R2"),([3],"R3")]) (D.steps (build (B.hold m "-"))),
    "two splits in one instant share child indices, so merge combines them" ~: do
        let x = B.input [([1], ["a", "b"])]
        let y = B.input [([1], ["A"])]
        assertEqual "m" [([1,0],"aA"),([1,1],"b")] (D.occs (B.merge (B.split x) (B.split y) (++))),
    "a defer and a split in one instant share child index 0" ~: do
        let x = B.input [([1], ["a", "b"])]
        let y = B.input [([1], "D")]
        assertEqual "m" [([1,0],"aD"),([1,1],"b")] (D.occs (B.merge (B.split x) (B.defer y) (++))),
    "gate and the cell it reads step in one transaction: the gate reads the old value" ~: do
        let open = build (B.inputCell False [([2], True), ([3], False)])
        let s = B.input [([1],'a'),([2],'b'),([3],'c'),([4],'d')]
        assertEqual "g" [([3],'c')] (D.occs (B.gate s open)),
    "switch_cell at a later switch instant: old inner's value and new inner's step collapse" ~: do
        let c1 = build (B.inputCell 'a' [([2],'b')])
        let c2 = build (B.inputCell 'x' [([2],'y')])
        let outer = build (B.hold (B.input [([2], c2)]) c1)
        assertEqual "one step at [2], the new inner's post-instant value"
            ('a', [([0],'a'),([2],'y')]) (D.steps (build (B.switchCell outer))),
    "switch_cell switching at [2] to a quiet inner still steps at [2]" ~: do
        let c1 = build (B.inputCell 'a' [])
        let c2 = build (B.inputCell 'x' [])
        let outer = build (B.hold (B.input [([2], c2)]) c1)
        assertEqual "s" [([0],'a'),([2],'x')] (D.occs (B.steps (build (B.switchCell outer)))),
    "lift: three inputs stepping in one child instant are one step" ~: do
        let a = build (B.hold (B.split (B.input [([1],[1,2])])) 0)
        let b = build (B.hold (B.split (B.input [([1],[10,20])])) 0)
        let c = build (B.hold (B.split (B.input [([1],[100 :: Int])])) 0)
        assertEqual "l" (0, [([1,0],111),([1,1],122)]) (D.steps (B.lift3 (a, b, c) (\x y z -> x + y + z)))
  ]

-- Child-transaction times ----------------------------------------------------

childTests :: Test
childTests = test [
    "nested split: grandchildren are t ++ [n, m]" ~: do
        let s = B.input [([1], [["a","b"],["c"]])]
        assertEqual "o" [([1,0,0],"a"),([1,0,1],"b"),([1,1,0],"c")] (D.occs (B.split (B.split s))),
    "child times order depth first and before the next external instant" ~: do
        let ts = [[2],[1,1],[1,0,0],[1],[1,0]] :: [T]
        assertEqual "sorted" [[1],[1,0],[1,0,0],[1,1],[2]] (sort ts),
    "construct over a defer runs its closure at the child time" ~: do
        let ticks = B.defer (B.input [([1], ()), ([2], ())])
        let src = B.input [([1],'p'),([1,0],'q'),([2],'r')]
        -- src is an input only for the test; its [1,0] event stands for a child event.
        let holds = B.construct ticks (\_ -> B.hold src '-')
        let occ = D.occs holds
        assertEqual "times" [[1,0],[2,0]] (map fst occ)
        assertEqual "hold built at [1,0] keeps [1,0], drops [1]"
            ('-', [([1,0],'q'),([2],'r')]) (D.steps (snd (head occ))),
    "sample in a closure at [1,0] sees the step at [1]" ~: do
        let c = build (B.inputCell (0 :: Int) [([1], 5)])
        let out = B.construct (B.defer (B.input [([1], ())])) (\_ -> B.sample c)
        assertEqual "o" [([1,0],5)] (D.occs out),
    "switch_stream at a child instant uses the old stream at that instant" ~: do
        let s1 = B.input [([1,0],'a'),([1,1],'b')]
        let s2 = B.input [([1,0],'X'),([1,1],'Y')]
        let sel = build (B.hold (B.split (B.input [([1], [s2])])) s1)
        assertEqual "o" [([1,0],'a'),([1,1],'Y')] (D.occs (B.switchStream sel)),
    "steps_with_current built in a construct at [2], then deferred, fires at [2,0]" ~: do
        let c = build (B.inputCell 'a' [([1],'b')])
        let out = B.construct (B.input [([2], ())]) (\_ -> B.defer <$> B.stepsWithCurrent c)
        assertEqual "outer" [[2]] (map fst (D.occs out))
        assertEqual "inner" [([2,0],'b')] (D.occs (snd (head (D.occs out)))),
    "Graph::sample after k sees grandchildren of k" ~: do
        let h = build (B.hold (B.split (B.split (B.input [([1],[[1],[2,3 :: Int]])]))) 0)
        assertEqual "after 1" 3 (B.ioSample h 1)
        assertEqual "after 0" 0 (B.ioSample h 0)
  ]

-- Loops ------------------------------------------------------------------------

ticksN :: Int -> Stream ()
ticksN n = B.input [ ([k], ()) | k <- [1 .. n] ]

verifyLoopTests :: Test
verifyLoopTests = test [
    "cell loop declared in a construct at [2]: the counter starts at [2]" ~: do
        let body c = B.hold (B.snapshot (ticksN 4) c (\_ n -> n + 1)) (0 :: Int)
        assertEqual "c" (0, [([2],1),([3],2),([4],3)]) (D.steps (at [2] (B.cellLoop body))),
    "stream loop declared in a construct at [2]: scan by a loop starts at [2]" ~: do
        let s = B.input [([1],'a'),([2],'b'),([3],'c')]
        let f a n = ((a, n), n + 1 :: Int)
        let states = at [2] (B.streamLoop (\st -> do
                h <- B.hold st 0
                return (B.map (B.snapshot s h f) snd)))
        assertEqual "o" [([2],1),([3],2)] (from [2] (D.occs states)),
    "the capped counter's fixed point does not depend on the first iterate" ~: do
        let body c = run (B.hold (B.filter (B.snapshot (ticksN 15) c (\_ n -> n + 1)) (<= 10)) (0 :: Int)) [0]
        let expected = Just (0, [ ([k], k) | k <- [1 .. 10] ])
        assertEqual "from never" expected (fixCellFrom (0, []) body)
        assertEqual "from junk" expected (fixCellFrom (0, [ ([k], 99) | k <- [1 .. 15] ]) body),
    "a stream loop through defer and merge converges" ~: do
        let ins = B.input [([1], 1 :: Int)]
        let body fwd = return (B.merge ins (B.defer (B.filter (B.map fwd (* 2)) (< 10))) (+))
        let (s, _) = build (B.streamLoopCount body)
        assertEqual "o" [([1],1),([1,0],2),([1,0,0],4),([1,0,0,0],8)] (D.occs s),
    "a stream loop through hold then steps passes the cycle rule and has no fixed point" ~: do
        -- s = ins merged (+) with (s held, then its steps, plus one): at [1], x = 0 + (x + 1).
        let ins = B.input [([1], 0 :: Int)]
        let body fwd = B.merge ins (B.map (B.steps (build (B.hold fwd 0))) (+ 1)) (+)
        let its = streamIterates 12 body
        assertEqual "no two iterates equal" (length its) (length (nub its))
        assertEqual "value at [1] grows" [0,1,2,3,4] (take 5 (map (snd . head) (drop 1 its))),
    "a cell loop hold(steps fwd) passes the cycle rule and has many fixed points" ~: do
        let body fwd = build (B.hold (B.steps fwd) (0 :: Int))
        assertEqual "never steps" (Just (0, [])) (fixCellFrom (0, []) body)
        assertEqual "steps at [1] to 5" (Just (0, [([1],5)])) (fixCellFrom (0, [([1],5)]) body),
    "once by a loop created at [2] after an event at [1]" ~: do
        let s = B.input [([1],'a'),([3],'c'),([4],'d')]
        assertEqual "direct" [([3],'c')] (D.occs (at [2] (B.once s)))
        assertEqual "loop" [([3],'c')] (from [2] (D.occs (at [2] (B.onceLoop s))))
  ]

-- The text's own claims --------------------------------------------------------

verifyTextTests :: Test
verifyTextTests = test [
    "5.15's prose says Value keeps simultaneous events; Value coalesces them" ~: do
        -- Figure E.10's vector: a step at the creation instant [0].
        let c = Hold 'a' (MkStream [([0], 'b'), ([1], 'c'), ([3], 'd')]) [0]
        let s = D.occs (Value c [0])
        assertEqual "one event at [0]" 1 (length (filter ((== [0]) . fst) s))
        assertEqual "it carries the step" ([0],'b') (head s),
    "accumulate_mut's value reaches steps through map_cell during the instant" ~: do
        -- RFD 4 refuses steps on the accumulator itself; the same post-instant
        -- state is carried by steps of any read-through cell over it.
        let acc = build (B.accumulateMut (B.input [([1],1),([2],2 :: Int)]) 0 (+))
        assertEqual "o" [([1],1),([2],3)] (D.occs (B.steps (B.mapCell acc id))),
    "Java's CellSink/StreamSink fold: left, as input_coalescing" ~: do
        -- CoalesceHandler.java:16-17: accum = f.apply(accum, a).
        let sends = [([1],"a"),([1],"b"),([1],"c")] :: [(T, String)]
        let javaFold = [([1], foldl1 (\acc a -> "(" ++ acc ++ a ++ ")") (map snd sends))]
        assertEqual "o" javaFold (D.occs (B.inputCoalescing (\acc a -> "(" ++ acc ++ a ++ ")") sends))
  ]

------------------------------------------------------------------------------
-- The two patches.

-- | The fidelity review's program (review-fidelity/hs/SplitSort.hs): a split
-- fed by its own children, as a stream loop over the lists, then merged
-- with an event at [0,0,0]. Returns the items and the merge.
splitFedByItsChildren :: (Stream [Int] -> Stream Int) -> (S Int, S Int)
splitFedByItsChildren splitWith = (D.occs items, D.occs (Merge items other (+)))
  where
    lists = MkStream [([0], [1, 2])]
    body fwd = Merge lists (MapS (\n -> [n * 10, n * 10 + 1]) (Filter (< 10) (splitWith fwd))) (\l _ -> l)
    (loop, _) = B.fixStream body
    items = MkStream (D.occs (splitWith loop))
    other = MkStream [([0, 0, 0], 100)]

-- | The outer cells of sodium.hs's four SwitchC vectors, and their answers.
switchVectors :: [(String, Cell (Cell Char), C Char)]
switchVectors =
    [ ("SwitchC 1", Hold c1 (MkStream [([1], c2)]) [0], ('a',[([0],'b'),([1],'X'),([2],'Y'),([3],'Z')]))
    , ("SwitchC 2", Hold c1 (MkStream [([1], c2')]) [0], ('a',[([0],'b'),([1],'X'),([2],'Y'),([3],'Z')]))
    , ("SwitchC 3", Hold c1 (MkStream [([1], c2'')]) [0], ('a',[([0],'b'),([1],'X'),([2],'Y'),([3],'Z')]))
    , ("SwitchC 4", Hold c1 (MkStream [([1], c2), ([3], c3)]) [0], ('a',[([0],'b'),([1],'X'),([2],'Y'),([3],'5')]))
    ]
  where
    c1 = Hold 'a' (MkStream [([0], 'b'), ([1], 'c'), ([2], 'd'), ([3], 'e')]) [0]
    c2 = Hold 'V' (MkStream [([0], 'W'), ([1], 'X'), ([2], 'Y'), ([3], 'Z')]) [0]
    c2' = Hold 'W' (MkStream [([1], 'X'), ([2], 'Y'), ([3], 'Z')]) [0]
    c2'' = Hold 'X' (MkStream [([2], 'Y'), ([3], 'Z')]) [0]
    c3 = Hold '1' (MkStream [([0], '2'), ([1], '3'), ([2], '4'), ([3], '5')]) [0]

patchTests :: Test
patchTests = test [
    "F6: switch_cell created at [3] after a switch at [1], new inner steps at [3]" ~: do
        let c1 = build (B.inputCell 'a' [])
        let c2 = build (B.inputCell 'x' [([3],'y')])
        let outer = build (B.hold (B.input [([1], c2)]) c1)
        -- The text starts from the deselected inner c1, and normalize moves
        -- c2's step at [3] back to [1].
        assertEqual "the text" ('x', [([3],'a'),([1],'y')]) (D.steps (run (B.switchCellText outer) [3]))
        assertEqual "the patch" ('x', [([3],'y')]) (D.steps (run (B.switchCell outer) [3])),
    "F6: the text's late switch cell steps to the deselected inner, the patch does not" ~: do
        let c1 = build (B.inputCell 'a' [])
        let c2 = build (B.inputCell 'x' [])
        let outer = build (B.hold (B.input [([1], c2)]) c1)
        assertEqual "the text's steps_with_current" [([3],'a')] (D.occs (run (B.switchCellText outer >>= B.stepsWithCurrent) [3]))
        assertEqual "the patch's steps_with_current" [([3],'x')] (D.occs (run (B.switchCell outer >>= B.stepsWithCurrent) [3])),
    "F7: a split fed by its own children, then merged" ~: do
        let (textItems, textMerged) = splitFedByItsChildren B.splitText
        let (items, merged) = splitFedByItsChildren B.split
        assertEqual "the text's items, out of time order"
            [([0,0],1),([0,1],2),([0,0,0],10),([0,0,1],11),([0,1,0],20),([0,1,1],21)] textItems
        assertEqual "the text's merge: two events at [0,0,0]"
            [([0,0],1),([0,0,0],100),([0,1],2),([0,0,0],10),([0,0,1],11),([0,1,0],20),([0,1,1],21)] textMerged
        assertEqual "the patch's items"
            [([0,0],1),([0,0,0],10),([0,0,1],11),([0,1],2),([0,1,0],20),([0,1,1],21)] items
        assertEqual "the patch's merge: one event at [0,0,0]"
            [([0,0],1),([0,0,0],110),([0,0,1],11),([0,1],2),([0,1,0],20),([0,1,1],21)] merged
        assertBool "the patch's merge is well formed" (B.wellFormed merged),
    "the twenty vectors are unchanged under the patches" ~: do
        -- Fifteen of the twenty use neither Split nor SwitchC. The other five
        -- give sodium.hs's answer through the text and through the patch.
        let s1 = MkStream [([0], ['a', 'b']), ([1],['c'])]
        assertEqual "Split, the text" [([0,0],'a'),([0,1],'b'),([1,0],'c')] (D.occs (Split s1))
        assertEqual "Split, the patch" [([0,0],'a'),([0,1],'b'),([1,0],'c')] (D.occs (B.split s1))
        forM_ switchVectors $ \(label, outer, expected) -> do
            assertEqual (label ++ ", the text") expected (D.steps (SwitchC outer [0]))
            assertEqual (label ++ ", the patch") expected (D.steps (build (B.switchCell outer))),
    "the patches agree with the text wherever the text is well formed" ~: do
        -- A split whose parents' children come in time order, and a switch
        -- cell created before its outer's first step.
        let parents = MkStream [([1], [1, 2 :: Int]), ([2], [3]), ([2,1], [4]), ([3], [5, 6])]
        assertEqual "split" (D.occs (B.splitText parents)) (D.occs (B.split parents))
        let c1 = build (B.inputCell 'a' [([2],'b')])
        let c2 = build (B.inputCell 'x' [([3],'y')])
        let outer = build (B.hold (B.input [([2], c2)]) c1)
        assertEqual "switch_cell at [1]" (D.steps (run (B.switchCellText outer) [1])) (D.steps (run (B.switchCell outer) [1]))
  ]

------------------------------------------------------------------------------
-- Programs through the interpreter.

-- | A vector's program: no inputs, no sends, everything answered.
vectorProgram :: [Def] -> [Int] -> Program
vectorProgram defs observed = Program Everything [] defs observed []

-- | A program over integer inputs, answered from the first transaction.
driven :: Int -> [Def] -> [Int] -> [[(Int, Int)]] -> Program
driven inputCount defs observed schedule =
    Program FromFirstTransaction (replicate inputCount (Input TInt Nothing)) defs observed
        (map (map (\(i, n) -> (i, I n))) schedule)

numbers :: [(T, Int)] -> Def
numbers events = SLit TInt [ (t, I n) | (t, n) <- events ]

letters :: [(T, Char)] -> Def
letters events = SLit TInt [ (t, I (fromEnum c)) | (t, c) <- events ]

streamOf :: [(T, Int)] -> Observation
streamOf events = Events [ (t, VInt n) | (t, n) <- events ]

cellOf :: Int -> [(T, Int)] -> Observation
cellOf initial sts = Steps (VInt initial) [ (t, VInt n) | (t, n) <- sts ]

letterStream :: [(T, Char)] -> Observation
letterStream events = streamOf [ (t, fromEnum c) | (t, c) <- events ]

letterCell :: Char -> [(T, Char)] -> Observation
letterCell initial sts = cellOf (fromEnum initial) [ (t, fromEnum c) | (t, c) <- sts ]

answers :: Program -> [Observation] -> Assertion
answers program expected = assertEqual "observations" (Right expected) (interpret FixedPoint program)

-- | sodium.hs's SwitchC vectors as a program: each inner a hold of letters,
-- a selector stream picking inner k, a hold of cells, and the switch.
switchProgram :: [(Char, [(T, Char)])] -> [(T, Int)] -> Program
switchProgram inners switches = vectorProgram defs [length defs - 1]
  where
    innerDefs = concat [ [letters events, CHold (Lit (fromEnum initial)) (N (2 * k))] | (k, (initial, events)) <- zip [0 ..] inners ]
    n = length innerDefs
    defs = innerDefs ++
        [ numbers switches
        , SPickCell Arg [ N (2 * k + 1) | k <- [0 .. length inners - 1] ] (N n)
        , CHoldCell (N 1) (N (n + 1))
        , CSwitch (N (n + 2))
        ]

interpreterVectors :: Test
interpreterVectors = test [
    "Never" ~: answers (vectorProgram [SNever TInt] [0]) [Events []],
    "MapS" ~: answers
        (vectorProgram [numbers [([0],5),([1],10),([2],12)], SMap (Add (Lit 1) Arg) (N 0)] [1])
        [streamOf [([0],6),([1],11),([2],13)]],
    "Snapshot" ~: answers
        (vectorProgram
            [ numbers [([1],4),([5],7)], CHold (Lit 3) (N 0)
            , letters [([0],'a'),([3],'b'),([5],'c')]
            , SSnapshot Arg2 (N 2) (N 1)
            , SConstruct (Body [] (RValue (Sample (N 1)))) (N 2) ] [3, 4])
        [streamOf [([0],3),([3],4),([5],4)], streamOf [([0],3),([3],4),([5],4)]],
    "Merge" ~: answers
        (vectorProgram [numbers [([0],0),([2],2)], numbers [([1],10),([2],20),([3],30)], SMerge (Add Arg Arg2) (N 0) (N 1)] [2])
        [streamOf [([0],0),([1],10),([2],22),([3],30)]],
    "Filter" ~: answers
        (vectorProgram [numbers [([0],5),([1],6),([2],7)], SFilter (Mod Arg 2) (N 0)] [1])
        [streamOf [([0],5),([2],7)]],
    "SwitchS" ~: answers
        (vectorProgram
            [ letters [([0],'a'),([1],'b'),([2],'c'),([3],'d')]
            , letters [([0],'W'),([1],'X'),([2],'Y'),([3],'Z')]
            , numbers [([1],1)]
            , SPickStream Arg [N 0, N 1] (N 2)
            , CHoldStream (N 0) (N 3)
            , SSwitch (N 4) ] [5])
        [letterStream [([0],'a'),([1],'b'),([2],'Y'),([3],'Z')]],
    "Execute" ~: answers
        (vectorProgram [numbers [([0],0)], SConstruct (Body [] (RValue (Lit (fromEnum 'a')))) (N 0)] [1])
        [letterStream [([0],'a')]],
    "Updates" ~: answers
        (vectorProgram [letters [([1],'b'),([3],'c')], CHold (Lit (fromEnum 'a')) (N 0), SSteps (N 1)] [2])
        [letterStream [([1],'b'),([3],'c')]],
    "Value 1" ~: answers
        (vectorProgram [letters [([1],'b'),([3],'c')], CHold (Lit (fromEnum 'a')) (N 0), SStepsWithCurrent (N 1)] [2])
        [letterStream [([0],'a'),([1],'b'),([3],'c')]],
    "Value 2" ~: answers
        (vectorProgram [letters [([0],'b'),([1],'c'),([3],'d')], CHold (Lit (fromEnum 'a')) (N 0), SStepsWithCurrent (N 1)] [2])
        [letterStream [([0],'b'),([1],'c'),([3],'d')]],
    "Split" ~: answers
        (vectorProgram [SLit TList [([0], L [97, 98]), ([1], L [99])], SSplit (N 0)] [1])
        [letterStream [([0,0],'a'),([0,1],'b'),([1,0],'c')]],
    "Constant" ~: answers (vectorProgram [CConstant (Lit (fromEnum 'a'))] [0]) [letterCell 'a' []],
    "Hold" ~: answers
        (vectorProgram [letters [([1],'b'),([3],'c')], CHold (Lit (fromEnum 'a')) (N 0)] [1])
        [letterCell 'a' [([1],'b'),([3],'c')]],
    "MapC" ~: answers
        (vectorProgram [numbers [([2],3),([3],5)], CHold (Lit 0) (N 0), CMapCell (Add (Lit 1) Arg) (N 1)] [2])
        [cellOf 1 [([2],4),([3],6)]],
    "Apply" ~: answers
        (vectorProgram
            [ numbers [([1],5),([3],6)], CHold (Lit 0) (N 0)
            , numbers [([1],200),([2],300),([4],400)], CHold (Lit 100) (N 2)
            , CLift (Add (ArgN 0) (ArgN 1)) [N 1, N 3] ] [4])
        [cellOf 100 [([1],205),([2],305),([3],306),([4],406)]],
    "SwitchC 1" ~: answers
        (switchProgram [('a', [([0],'b'),([1],'c'),([2],'d'),([3],'e')]), ('V', [([0],'W'),([1],'X'),([2],'Y'),([3],'Z')])] [([1],1)])
        [letterCell 'a' [([0],'b'),([1],'X'),([2],'Y'),([3],'Z')]],
    "SwitchC 2" ~: answers
        (switchProgram [('a', [([0],'b'),([1],'c'),([2],'d'),([3],'e')]), ('W', [([1],'X'),([2],'Y'),([3],'Z')])] [([1],1)])
        [letterCell 'a' [([0],'b'),([1],'X'),([2],'Y'),([3],'Z')]],
    "SwitchC 3" ~: answers
        (switchProgram [('a', [([0],'b'),([1],'c'),([2],'d'),([3],'e')]), ('X', [([2],'Y'),([3],'Z')])] [([1],1)])
        [letterCell 'a' [([0],'b'),([1],'X'),([2],'Y'),([3],'Z')]],
    "SwitchC 4" ~: answers
        (switchProgram
            [ ('a', [([0],'b'),([1],'c'),([2],'d'),([3],'e')])
            , ('V', [([0],'W'),([1],'X'),([2],'Y'),([3],'Z')])
            , ('1', [([0],'2'),([1],'3'),([2],'4'),([3],'5')]) ]
            [([1],1),([3],2)])
        [letterCell 'a' [([0],'b'),([1],'X'),([2],'Y'),([3],'5')]],
    "Sample" ~: answers
        (vectorProgram
            [ letters [([1],'b')], CHold (Lit (fromEnum 'a')) (N 0)
            , numbers [([1],0),([2],0)]
            , SConstruct (Body [] (RValue (Sample (N 1)))) (N 2) ] [3])
        [letterStream [([1],'a'),([2],'b')]]
  ]

interpreterCommonVectors :: Test
interpreterCommonVectors = test [
    "split" ~: answers
        (vectorProgram [SLit TList [([0], L [97, 98])], SSplit (N 0)] [1])
        [letterStream [([0,0],'a'),([0,1],'b')]],
    "defer1" ~: answers
        (vectorProgram [letters [([0],'a'),([1],'b')], SDefer (N 0)] [1])
        [letterStream [([0,0],'a'),([1,0],'b')]],
    "defer2" ~: answers
        (vectorProgram [letters [([0],'a'),([1],'b')], letters [([1],'B')], SDefer (N 0), SOrElse (N 2) (N 1)] [3])
        [letterStream [([0,0],'a'),([1],'B'),([1,0],'b')]],
    "orElse1" ~: answers
        (vectorProgram [numbers [([0],0),([2],2)], numbers [([1],10),([2],20),([3],30)], SOrElse (N 0) (N 1)] [2])
        [streamOf [([0],0),([1],10),([2],2),([3],30)]],
    "deferSimultaneous" ~: answers
        (vectorProgram [letters [([1],'b')], letters [([0],'A'),([1],'B')], SDefer (N 0), SDefer (N 1), SOrElse (N 2) (N 3)] [4])
        [letterStream [([0,0],'A'),([1,0],'b')]]
  ]

-- | A construct at [2] whose body emits a cell, held into a switch cell
-- created in the build, so the answer shows the body's cell from [2] on.
-- Input 0 carries 97, 98, 99 at [1], [2], [3]; input 1 fires once, at [2].
constructedAt2 :: [Def] -> Ref -> Program
constructedAt2 body result = driven 2
    [ SInput 0, SInput 1
    , SConstruct (Body body (RNode result)) (N 1)
    , CConstant (Lit (-2))
    , CHoldCell (N 3) (N 2)
    , CSwitch (N 4) ]
    [5]
    [[(0, 97)], [(0, 98), (1, 0)], [(0, 99)]]

interpreterOperations :: Test
interpreterOperations = test [
    "scan created at [2] by a construct: its state starts at [2]" ~: do
        answers (constructedAt2 [SScan (Lit 0) (Add (Mul Arg (Lit 100)) Arg2) (Add Arg2 (Lit 1)) (N 0), CHold (Lit (-1)) (Local 0)] (Local 1))
            [cellOf (-2) [([2],9800),([3],9901)]]
        -- The same through Oracle.Derived.
        let s = B.input [([1],97),([2],98),([3],99 :: Int)]
        assertEqual "Derived" [([2],9800),([3],9901)]
            (filter ((>= [2]) . fst) (D.occs (run (B.scan s 0 (\a n -> (a * 100 + n, n + 1))) [2]))),
    "once created at [2] by a construct sees the event at [2]" ~:
        answers (constructedAt2 [SOnce (N 0), CHold (Lit (-1)) (Local 0)] (Local 1)) [cellOf (-2) [([2],98)]],
    "accumulate created at [2] by a construct" ~:
        answers (constructedAt2 [CAccum (Lit 0) (Add Arg Arg2) (N 0)] (Local 0)) [cellOf (-2) [([2],98),([3],197)]],
    "a cell loop declared in a construct at [2]: the counter starts at [2]" ~:
        answers (constructedAt2
            [CLoop TInt, SSnapshot (Add Arg2 (Lit 1)) (N 0) (Local 0), CHold (Lit 0) (Local 1), Close 0 (Local 2)] (Local 2))
            [cellOf (-2) [([2],1),([3],2)]],
    "a stream loop declared in a construct at [2]: scan by a loop starts at [2]" ~:
        answers (constructedAt2
            [ SLoop TInt, CHold (Lit 0) (Local 0), SSnapshot (Add Arg2 (Lit 1)) (N 0) (Local 1), Close 0 (Local 2)
            , CHold (Lit (-1)) (Local 2) ] (Local 4))
            [cellOf (-2) [([2],1),([3],2)]],
    "gate reads the cell before the instant" ~:
        answers (driven 2 [SInput 0, CInput 1 (Lit 1), CToBool (N 1), SGate (N 0) (N 2)] [3]
            [[(0, 1)], [(0, 2), (1, 0)], [(0, 3), (1, 1)], [(0, 4)]])
            [streamOf [([1],1),([2],2),([4],4)]],
    "a coalescing input folds left, first send on the left" ~:
        answers (Program FromFirstTransaction [Input TInt (Just (Sub Arg Arg2))] [SInput 0] [0]
            [[(0, I 1), (0, I 2), (0, I 3)], [(0, I 10)], [], [(0, I 7), (0, I 1)]])
            [streamOf [([1],(1 - 2) - 3),([2],10),([4],6)]],
    "switch_cell created at [3] after its outer switched: the patch (F6)" ~:
        -- The outer switches from a constant 'a' to a constant 'x' at [1]; a
        -- construct at [3] holds the steps of a switch cell it creates. The
        -- text would hold 'a'.
        answers (driven 1
            [ SInput 0, CConstant (Lit 97), CConstant (Lit 120)
            , SPickCell Arg [N 1, N 2] (N 0)
            , CHoldCell (N 1) (N 3)
            , SLit TInt [([3], I 0)]
            , SConstruct (Body [CSwitch (N 4), SSteps (Local 0), CHold (Lit 0) (Local 1)] (RNode (Local 2))) (N 5)
            , CConstant (Lit (-2))
            , CHoldCell (N 7) (N 6)
            , CSwitch (N 8) ]
            [9] [[(0, 1)], [], []])
            [cellOf (-2) [([3],120)]],
    "a split fed by its own children answers in time order (F7)" ~:
        answers (Program Everything []
            [ SLit TList [([0], L [1, 2])]
            , SLoop TList
            , SSplit (N 1)
            , SFilter (Lt Arg (Lit 10)) (N 2)
            , SMapList (Lit 2) (Add (Mul Arg (Lit 10)) Arg2) (N 3)
            , SOrElse (N 0) (N 4)
            , Close 1 (N 5)
            , SSplit (N 5)
            , SLit TInt [([0,0,0], I 100)]
            , SMerge (Add Arg Arg2) (N 7) (N 8) ]
            [7, 9] [])
            [ streamOf [([0,0],1),([0,0,0],10),([0,0,1],11),([0,1],2),([0,1,0],20),([0,1,1],21)]
            , streamOf [([0,0],1),([0,0,0],110),([0,0,1],11),([0,1],2),([0,1,0],20),([0,1,1],21)] ]
  ]

-- | The health-and-shield slice of research-loop-shapes, shape 1 and 2b,
-- with three cell loops. Inputs: heal, damage, level_up. snapshot3 is two
-- snapshots, the first passing on the sum of the delta and the current
-- health. The encoded fraction is health * 1000 + max_health.
slice :: [[(Int, Int)]] -> Program
slice = driven 3
    [ SInput 0, SInput 1, SInput 2, SShare (N 1)
    , CLoop TInt, SSnapshot (Add Arg2 Arg) (N 2) (N 4), CHold (Lit 100) (N 5), Close 4 (N 6)
    , CLoop TInt, SSnapshot (Max (Sub Arg2 Arg) (Lit 0)) (N 3) (N 8), CHold (Lit 30) (N 9), Close 8 (N 10)
    , SMap Arg (N 0)
    , SSnapshot (Sub (Lit 0) (Max (Sub Arg Arg2) (Lit 0))) (N 3) (N 10)
    , SMerge (Add Arg Arg2) (N 12) (N 13)
    , CLoop TInt, SSnapshot (Add Arg Arg2) (N 14) (N 15), SSnapshot (Max (Lit 0) (Min Arg2 Arg)) (N 16) (N 6)
    , CHold (Lit 60) (N 17), Close 15 (N 18)
    , CLift (Add (Mul (ArgN 1) (Lit 1000)) (ArgN 0)) [N 6, N 18]
    , CLift (Add (ArgN 0) (ArgN 1)) [N 18, N 10] ]
    [6, 10, 13, 14, 18, 20, 21]

-- | The wave and the ring (the oracle review's second round): a hold of 1
-- over n ticks, transaction k sending k, that never steps. The snapshot
-- passes 11 when it reads 0 at [1], and k + 10 when it reads k + 9 at [k];
-- anything else is -999, which the filter drops. So from the start, where
-- the loop holds 0, every iterate is one step, and round k's is at [k].
-- With one loop the snapshot reads the hold's own loop (the wave). With
-- more, each further loop is a map_cell of the one before and the snapshot
-- reads the last (the ring), so the step takes a round per loop at each
-- instant.
ring :: Int -> Int -> Program
ring loops n = driven 1 defs [loops + 3] [ [(0, k)] | k <- [1 .. n] ]
  where
    passed = If (Eq Arg2 (Lit 0))
        (If (Eq Arg (Lit 1)) (Lit 11) (Lit (-999)))
        (If (Eq Arg2 (Add Arg (Lit 9))) (Add Arg (Lit 10)) (Lit (-999)))
    defs = [SInput 0] ++ replicate loops (CLoop TInt)
        ++ [ SSnapshot passed (N 0) (N loops), SFilter (Not (Eq Arg (Lit (-999)))) (N (loops + 1))
           , CHold (Lit 1) (N (loops + 2)), Close 1 (N (loops + 3)) ]
        ++ concat [ [CMapCell Arg (N (j - 1)), Close j (N (loops + 1 + 2 * j))] | j <- [2 .. loops] ]

interpreterLoops :: Test
interpreterLoops = test [
    "capped counter: filter inside a cell loop, by fixed point" ~:
        answers (driven 1
            [SInput 0, CLoop TInt, SSnapshot (Add Arg2 (Lit 1)) (N 0) (N 1), SFilter (Lt Arg (Lit 11)) (N 2), CHold (Lit 0) (N 3), Close 1 (N 4)]
            [4] (replicate 15 [(0, 0)]))
            [cellOf 0 [ ([k], k) | k <- [1 .. 10] ]],
    "uncapped counter: the fixed point equals the lazy knot" ~: do
        let program = driven 1 [SInput 0, CLoop TInt, SSnapshot (Add Arg2 (Lit 1)) (N 0) (N 1), CHold (Lit 0) (N 2), Close 1 (N 3)]
                [3] (replicate 8 [(0, 0)])
        answers program [cellOf 0 [ ([k], k) | k <- [1 .. 8] ]]
        assertEqual "knot" (interpret FixedPoint program) (interpret Knot program),
    "sodium-rust#52: three loops together, health 100 (record 0003)" ~: do
        let program = slice [[(0, 100), (2, 100), (1, 50)]]
        answers program
            [ cellOf 100 [([1],200)], cellOf 30 [([1],0)], streamOf [([1],-20)], streamOf [([1],80)]
            , cellOf 60 [([1],100)], cellOf 60100 [([1],100200)], cellOf 90 [([1],100)] ]
        assertEqual "knot" (interpret FixedPoint program) (interpret Knot program),
    "the health-and-shield slice on the long schedule (2b)" ~: do
        let program = slice
                [ [(1, 10)], [(0, 100), (1, 50), (2, 100)], [(1, 30)], [(0, 500), (2, 50)], [(0, 100)], [(1, 300)], [(0, 40)] ]
        answers program
            [ cellOf 100 [([2],200),([4],250)]
            , cellOf 30 [([1],20),([2],0),([3],0),([6],0)]
            , streamOf [([1],0),([2],-30),([3],-30),([6],-300)]
            , streamOf [([1],0),([2],70),([3],-30),([4],500),([5],100),([6],-300),([7],40)]
            , cellOf 60 [([1],60),([2],100),([3],70),([4],200),([5],250),([6],0),([7],40)]
            , cellOf 60100 [([1],60100),([2],100200),([3],70200),([4],200250),([5],250250),([6],250),([7],40250)]
            , cellOf 90 [([1],80),([2],100),([3],70),([4],200),([5],250),([6],0),([7],40)] ]
        assertEqual "knot" (interpret FixedPoint program) (interpret Knot program),
    "a stream loop through defer with a filter inside (the countdown)" ~:
        answers (Program Everything []
            [ numbers [([1],3),([2],1)], SLoop TInt, SDefer (N 1), SMap (Sub Arg (Lit 1)) (N 2)
            , SFilter (Lt (Lit 0) Arg) (N 3), SOrElse (N 0) (N 4), Close 1 (N 5) ] [5] [])
            [streamOf [([1],3),([1,0],2),([1,0,0],1),([2],1)]],
    "accumulate equals a cell loop through a snapshot, and the knot" ~: do
        let program = driven 1
                [ SInput 0, CAccum (Lit 0) (Add Arg Arg2) (N 0)
                , CLoop TInt, SSnapshot (Add Arg Arg2) (N 0) (N 2), CHold (Lit 0) (N 3), Close 2 (N 4) ]
                [1, 4] [[(0, 1)], [(0, 2)], [(0, 3)]]
        answers program [cellOf 0 [([1],1),([2],3),([3],6)], cellOf 0 [([1],1),([2],3),([3],6)]]
        assertEqual "knot" (interpret FixedPoint program) (interpret Knot program),
    "a loop whose answer chains through 250 instants converges, and equals accumulate" ~: do
        -- One round per instant: 251 rounds, past the 200 that once were all
        -- a loop had. Accumulate's knot needs no rounds at all.
        let program = driven 1
                [ SInput 0, CAccum (Lit 0) (Add Arg Arg2) (N 0)
                , CLoop TInt, SSnapshot (Add Arg Arg2) (N 0) (N 2), CHold (Lit 0) (N 3), Close 2 (N 4) ]
                [1, 4] [ [(0, k)] | k <- [1 .. 250] ]
            sums = cellOf 0 [ ([k], k * (k + 1) `div` 2) | k <- [1 .. 250] ]
        answers program [sums, sums]
        assertEqual "knot" (interpret FixedPoint program) (interpret Knot program),
    "two loops, one reading the other within the instant, settle an instant every two rounds" ~: do
        -- a = hold 0 (snapshot ticks b (+1)) and b = map_cell a: where the
        -- rounds' iterates first differ stays at an instant for two rounds,
        -- so a test that it moves on every round would refuse this loop.
        let program = driven 1
                [ SInput 0, CLoop TInt, CLoop TInt, SSnapshot (Add Arg2 (Lit 1)) (N 0) (N 2)
                , CHold (Lit 0) (N 3), Close 1 (N 4), CMapCell Arg (N 1), Close 2 (N 6) ]
                [4, 6] (replicate 3 [(0, 0)])
            counted = cellOf 0 [([1],1),([2],2),([3],3)]
        answers program [counted, counted]
        assertEqual "knot" (interpret FixedPoint program) (interpret Knot program),
    "a loop whose one step moves an instant later every round converges (the wave)" ~:
        -- 252 rounds over 250 ticks, while each state holds two loop times.
        -- Rounds allowed by the size of the largest state ran out at 204;
        -- every loop time the states have held counts.
        answers (ring 1 250) [cellOf 1 []],
    "ten loops that pass the step along within each instant converge (the ring)" ~:
        -- A round per loop at each instant: 261 rounds over 25 ticks, where
        -- the largest state allowed 240.
        answers (ring 10 25) [cellOf 1 []],
    "a body's loop that chains through child instants before the body's instant converges" ~:
        -- The body runs at [2], and its stream loop counts up from each event
        -- of input 0 in child instants, to 29: from 0 at [1], before the body
        -- existed, which the switch never shows, and from 25 at [3]. The
        -- count at [1] takes 30 rounds. A test that the earliest change moves
        -- later, taking a body run's changes at its instant at the earliest,
        -- would see those rounds stand still at [2] and answer ERR.
        answers (driven 2
            [ SInput 0, SInput 1
            , SConstruct (Body
                [ SLoop TInt, SDefer (Local 0), SMap (Add Arg (Lit 1)) (Local 1), SFilter (Lt Arg (Lit 30)) (Local 2)
                , SOrElse (N 0) (Local 3), Close 0 (Local 4) ]
                (RNode (Local 4))) (N 1)
            , SNever TInt, CHoldStream (N 3) (N 2), SSwitch (N 4) ]
            [5] [[(0, 0)], [(1, 0)], [(0, 25)]])
            [streamOf [([3],25),([3,0],26),([3,0,0],27),([3,0,0,0],28),([3,0,0,0,0],29)]],
    "a running sum over split's children chains through 201 child instants" ~:
        answers (Program FromFirstTransaction [Input TList Nothing]
            [SInput 0, SSplit (N 0), CLoop TInt, SSnapshot (Add Arg2 Arg) (N 1) (N 2), CHold (Lit 0) (N 3), Close 2 (N 4)]
            [4] (replicate 67 [(0, L [1, 1, 1])]))
            [cellOf 0 [ ([k, j], 3 * (k - 1) + j + 1) | k <- [1 .. 67], j <- [0 .. 2] ]],
    "a body's loop that never stops at the loops' start converges with them" ~: do
        -- The body counts 0, 1, 2, ... in child instants of [1] and stops
        -- where the count meets the outer loop's value before [1], which is
        -- 5; it emits 5. At the start the outer loop holds 0, where the
        -- count never stops. The body's loop iterates in the same rounds as
        -- the outer one, so it never has to settle on the start's value
        -- alone (the oracle review's e.txt).
        let body limit result = Body
                [ SLoop TInt, SDefer (Local 0), SMap (Add Arg (Lit 1)) (Local 1), SFilter limit (Local 2)
                , SOnce (N 0), SMapTo (I 0) (Local 4), SOrElse (Local 5) (Local 3), Close 0 (Local 6)
                , CHold (Lit 0) (Local 6) ]
                result
            emitsFive limit = driven 1
                [ SInput 0, CLoop TInt, SConstruct (body limit (RValue (Add (Sample (Local 8)) (Lit 5)))) (N 0)
                , CHold (Lit 5) (N 2), Close 1 (N 3) ]
                [3] [[(0, 0)]]
        answers (emitsFive (Not (Eq Arg (Sample (N 1))))) [cellOf 5 [([1],5)]]
        answers (emitsFive (Lt Arg (Sample (N 1)))) [cellOf 5 [([1],5)]]
        -- The count itself, with the limit a constant: a switch shows the
        -- body's loop after [1].
        answers (driven 1
            [ SInput 0, CConstant (Lit 5), SConstruct (body (Not (Eq Arg (Sample (N 1)))) (RNode (Local 6))) (N 0)
            , SNever TInt, CHoldStream (N 3) (N 2), SSwitch (N 4) ]
            [5] [[(0, 0)], []])
            [streamOf [([1,0],1),([1,0,0],2),([1,0,0,0],3),([1,0,0,0,0],4)]],
    "a same-instant cycle that counts up at one instant answers ERR" ~: do
        -- s = the input merged (+) with (s held, its steps, plus one): at
        -- [1], x = 0 + (x + 1). The text diverges on it, and the engine must
        -- refuse it (finding F3). Every state holds the one loop time, its
        -- event at [1], so the rounds run out.
        line <- answer (show (driven 1
            [SInput 0, SLoop TInt, CHold (Lit 0) (N 1), SSteps (N 2), SMap (Add Arg (Lit 1)) (N 3), SMerge (Add Arg Arg2) (N 0) (N 4), Close 1 (N 5)]
            [5] [[(0, 0)]]))
        assertEqual "answer"
            ("ERR the loops did not converge in " ++ show (roundsAllowed 1) ++ " rounds; still changing: node 1 at [1]")
            line,
    "a same-instant cycle in a body answers ERR, naming the body run" ~: do
        -- The same cycle in a body run at [1], whose result nothing reads:
        -- its loop iterates with the rest all the same.
        line <- answer (show (driven 1
            [ SInput 0
            , SConstruct (Body
                [SLoop TInt, CHold (Lit 0) (Local 0), SSteps (Local 1), SMap (Add Arg (Lit 1)) (Local 2), SMerge (Add Arg Arg2) (N 0) (Local 3), Close 0 (Local 4)]
                (RValue (Lit 0))) (N 0) ]
            [1] [[(0, 0)]]))
        assertEqual "answer"
            ("ERR the loops did not converge in " ++ show (roundsAllowed 1) ++ " rounds; still changing: node 1, body at [1], node 0 at [1]")
            line,
    "a loop through defer with no filter grows without end, until the time limit" ~: do
        -- Each round adds a child instant, [1,0], [1,0,0], and so on, a loop
        -- time no state held before, and the rounds allowed grow with it
        -- (finding F22): only the process's time limit ends it.
        result <- timeout 1000000 (answer (show (driven 1
            [SInput 0, SLoop TInt, SDefer (N 1), SMap (Add Arg (Lit 1)) (N 2), SOrElse (N 0) (N 3), Close 1 (N 4)]
            [4] [[(0, 0)]])))
        assertEqual "answer" Nothing result
  ]

-- | The first line of an answer's message, for checks.
rejects :: Program -> String -> Assertion
rejects program expected = case interpret FixedPoint program of
    Left message -> assertBool ("message: " ++ message) (expected `isInfixOf` message)
    Right _ -> assertFailure "the program was accepted"

interpreterChecks :: Test
interpreterChecks = test [
    "an adapter over the wrong type" ~:
        rejects (vectorProgram [SLit TList [([0], L [1])], SMap Arg (N 0)] [1]) "node 1 (SMap): N 0 carries TList",
    "a reference to a later node" ~:
        rejects (vectorProgram [SMap Arg (N 1), numbers []] [0]) "node 0 (SMap): N 1 does not name a node defined before this one",
    "Local at the top level" ~:
        rejects (vectorProgram [SMap Arg (Local 0)] [0]) "names a body's node",
    "a loop never closed" ~:
        rejects (vectorProgram [CLoop TInt] [0]) "node 0: the loop is never closed",
    "a loop closed twice" ~:
        rejects (vectorProgram [CLoop TInt, CConstant (Lit 1), Close 0 (N 1), Close 0 (N 1)] [1]) "closed more than once",
    "a loop closed with the wrong type" ~:
        rejects (vectorProgram [CLoop TInt, numbers [], Close 0 (N 1)] [1]) "the loop has type TCellOf TInt",
    "an unbound argument" ~:
        rejects (vectorProgram [numbers [], SMap Arg2 (N 0)] [1]) "Arg2 is not bound",
    "CArg outside a body" ~:
        rejects (vectorProgram [numbers [], SMap CArg (N 0)] [1]) "CArg is used outside a construct body",
    "a divisor that is not positive" ~:
        rejects (vectorProgram [numbers [], SMap (Mod Arg 0) (N 0)] [1]) "Mod needs a positive divisor",
    "a literal stream whose times do not increase" ~:
        rejects (vectorProgram [numbers [([1],1),([1],2)]] [0]) "must increase",
    "two sends to an input that does not coalesce" ~:
        rejects (driven 1 [SInput 0] [0] [[(0, 1), (0, 2)]]) "transaction 1: input 0 is sent more than once",
    "a send of the wrong type" ~:
        rejects (Program FromFirstTransaction [Input TInt Nothing] [SInput 0] [0] [[(0, B True)]]) "input 0 carries TInt",
    "observing a stream of streams" ~:
        rejects (vectorProgram [SNever (TStreamOf TInt)] [0]) "only first-order values can be answered",
    "observing a Close" ~:
        rejects (vectorProgram [CLoop TInt, CConstant (Lit 1), Close 0 (N 1)] [2]) "is a Close",
    "a body's error names the body" ~:
        rejects (vectorProgram [numbers [], SConstruct (Body [SMap Arg (Local 3)] (RValue (Lit 0))) (N 0)] [1])
            "node 1 (SConstruct): body of node 0 (SMap): Local 3 does not name"
  ]

interpreterAnswers :: Test
interpreterAnswers = test [
    "a stream and a cell, as JSON" ~: do
        line <- answer (show (vectorProgram [numbers [([0],5),([1],10)], SMap (Add (Lit 1) Arg) (N 0), CHold (Lit (-3)) (N 1)] [1, 2]))
        assertEqual "answer" "OK [[0,[[[0],6],[[1],11]]],[1,-3,[[[0],6],[[1],11]]]]" line,
    "booleans answer 0 and 1, lists answer arrays" ~: do
        line <- answer (show (vectorProgram [numbers [([0],5),([1],10)], SMapTo (B True) (N 0), SMapTo (L [1, -2]) (N 0)] [1, 2]))
        assertEqual "answer" "OK [[0,[[[0],1],[[1],1]]],[0,[[[0],[1,-2]],[[1],[1,-2]]]]]" line,
    "the window from the first transaction" ~: do
        let program = Program FromFirstTransaction [Input TInt Nothing]
                [SInput 0, CHold (Lit 7) (N 0), SStepsWithCurrent (N 1), CHold (Lit 0) (N 2)] [1, 2, 3] [[(0, I 8)]]
        line <- answer (show program)
        -- The steps_with_current event at [0] is before the window; its hold
        -- has already taken it by [1].
        assertEqual "answer" "OK [[1,7,[[[1],8]]],[0,[[[1],8]]],[1,7,[[[1],8]]]]" line,
    "extreme integers" ~: do
        line <- answer "Program Everything [] [CConstant (Lit (-9223372036854775808)),CConstant (Lit 9223372036854775807)] [0,1] []"
        assertEqual "answer" "OK [[1,-9223372036854775808,[]],[1,9223372036854775807,[]]]" line,
    "arithmetic wraps at 64 bits" ~: do
        line <- answer (show (vectorProgram [CConstant (Add (Lit maxBound) (Lit 1)), CConstant (Mul (Lit minBound) (Lit (-1))), CConstant (Mod (Lit (-7)) 3)] [0, 1, 2]))
        assertEqual "answer" "OK [[1,-9223372036854775808,[]],[1,-9223372036854775808,[]],[1,2,[]]]" line,
    "a line that is not a program" ~: do
        line <- answer "Program Everything"
        assertBool line ("ERR parse:" `isPrefixOf` line),
    "an error message stays on one line" ~: do
        line <- answer (show (vectorProgram [SMap Arg (N 5)] [0]))
        assertBool line ("ERR node 0 (SMap)" `isPrefixOf` line && '\n' `notElem` line)
  ]
