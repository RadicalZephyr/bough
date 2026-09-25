{-# LANGUAGE GADTs #-}
-- | Every Bough operation, defined over the constructors of the executable
-- semantics, Reactive.Sodium.Denotational (Denotational Semantics of Sodium,
-- revision 1.1). That module is imported unchanged.
--
-- Ported from the spike's research (research-derived-ops, Derived.hs) with
-- its verification's corrections, and with two patches to defects in the
-- text. The patches live here, in the derived layer; the vendored file stays
-- untouched.
--
-- * F6, 'switchCell': the text's SwitchC scans the outer cell from its
--   initial value, so a switch cell created after its outer stepped starts
--   from a deselected inner and has steps out of time order; when the new
--   inner steps at the creation instant, even sample reads a wrong value.
--   The patch chops the outer cell at the creation time, as Java's
--   Cell.switchC does. 'switchCellText' is the text.
-- * F7, 'split': the text's Split concatenates each parent's children in
--   the order of the parents' events, so a split fed by its own children
--   emits out of time order, and a merge downstream sees two events at one
--   instant. The patch sorts the children stably by time. 'splitText' is the
--   text.
--
-- Conventions.
--
-- * Argument order follows Bough: the receiver first, then the Rust
--   arguments in order. @s.map(f)@ is @map s f@; @s.hold(b, init)@ is
--   @hold s init@.
-- * The build context is the 'Reactive' monad. An operation lives in
--   'Reactive' only when its denotation reads the time, which is its
--   creation time t0. Every other operation is a plain function, even where
--   Bough makes it a materializer that takes @b@.
-- * Time: the build runs as transaction zero at @[0]@; external
--   transactions are @[1]@, @[2]@, ...; the child transactions of @t@ are
--   @t ++ [n]@. 'build' runs a build closure at @[0]@.
-- * The PDF of the text prints no section numbers, only "Appendix E" and
--   the figure numbers E.1 to E.20. The numbers here are that appendix's
--   numbering with the E dropped, as bevy-sodium's rendering of the text
--   prints them: 5.1 Never, 5.2 MapS, 5.3 Snapshot, 5.4 Merge, 5.5 Filter,
--   5.6 SwitchS, 5.7 Execute, 5.8 Updates, 5.9 Value, 5.10 Split,
--   5.11 Constant, 5.12 Hold, 5.13 MapC, 5.14 Apply, 5.15 SwitchC,
--   5.16 Sample.
module Oracle.Derived
  ( -- * Time
    buildTime, externalTime, build, ioSample, wellFormed, boughInput
    -- * Build: things that start from nothing
  , input, inputCoalescing, inputCoalescingByMerge
  , inputCell, inputCellCoalescing, constant, never
    -- * Adapters
  , map, filter, filterMap, mapTo, snapshot, gate, once, onceLoop
    -- * Stream materializers
  , hold, accumulate, accumulateMut, scan, share, node, merge, orElse
  , split, splitText, defer, construct
    -- * Cell operations
  , switchStream, switchCell, switchCellText, mapCell
  , lift2, lift3, lift4, lift5, lift6
  , steps, stepsWithCurrent, sample
    -- * Loops
  , cellLoop, cellLoopCount, streamLoop, streamLoopCount
  , cellLoopKnot, streamLoopKnot, fixCell, fixStream
    -- * Sodium operations Bough does not name
  , switcher, apply, coalesceStream, snapshotViaExecute
    -- * Helpers
  , concrete, concreteCell, chopCell
  ) where

import Prelude hiding (map, filter)
import qualified Prelude as P
import Data.Function (on)
import Data.List (groupBy, sortOn)
import Data.Maybe (fromJust, isJust)
import Reactive.Sodium.Denotational (Stream(..), Cell(..), Reactive(..), T, S, C)
import qualified Reactive.Sodium.Denotational as D

------------------------------------------------------------------------------
-- Time

-- | The time of transaction zero, in which the build closure runs.
buildTime :: T
buildTime = [0]

-- | The time of the k-th external transaction, k >= 1.
externalTime :: Int -> T
externalTime k = [k]

-- | Run a build closure: the build context is 'Reactive' run at @[0]@.
build :: Reactive a -> a
build r = run r buildTime

-- | Graph::sample from I/O code after external transaction k and all of its
-- child transactions: every step before @[k+1]@ has happened.
ioSample :: Cell a -> Int -> a
ioSample c k = D.at (D.steps c) [k + 1]

-- | The 1.1 invariant (section 2): times strictly increase.
wellFormed :: S a -> Bool
wellFormed s = and (zipWith (<) ts (drop 1 ts)) where ts = P.map fst s

-- | What I/O code can put into a Bough input: external times only.
boughInput :: S a -> Bool
boughInput s = wellFormed s && all (\(t, _) -> length t == 1 && head t >= 1) s

------------------------------------------------------------------------------
-- Build: things that start from nothing

-- | @b.input()@. The sends are the stream; at most one per transaction
-- (a second is DoubleSend), which is exactly the S a invariant. MkStream,
-- section 5 preamble.
input :: S a -> Stream a
input = MkStream

-- | @b.input_coalescing(f)@. @sends@ lists every send in send order, times
-- nondecreasing; the sends of one transaction fold left, first send on the
-- left: @f (f a1 a2) a3@. This is the executable's own 'D.coalesce', the
-- function Merge applies (section 5.4). Java's StreamSink(f) and
-- CellSink(init, f) fold the same way (CoalesceHandler.java:16-17).
inputCoalescing :: (a -> a -> a) -> [(T, a)] -> Stream a
inputCoalescing f sends = MkStream (D.coalesce f sends)

-- | The same stream with constructors only: the first sends of every
-- transaction, the second sends, and so on, merged left to right with f.
inputCoalescingByMerge :: (a -> a -> a) -> [(T, a)] -> Stream a
inputCoalescingByMerge f sends =
    foldl1 (\x y -> Merge x y f) (P.map MkStream ranks)
  where
    groups = groupBy ((==) `on` fst) sends
    width = maximum (1 : P.map length groups)
    ranks = [ [ g !! r | g <- groups, length g > r ] | r <- [0 .. width - 1] ]

-- | @b.input_cell(initial)@: a hold over an input. Hold, section 5.12.
inputCell :: a -> S a -> Reactive (Cell a)
inputCell initial sends = hold (input sends) initial

-- | @b.input_cell_coalescing(initial, f)@. The input coalesces with f
-- before the hold sees it. Under revision 1.1 a hold never sees two events
-- in one instant, so the coalescing belongs to the input.
inputCellCoalescing :: a -> (a -> a -> a) -> [(T, a)] -> Reactive (Cell a)
inputCellCoalescing initial f sends = hold (inputCoalescing f sends) initial

-- | @b.constant(a)@. Constant, section 5.11; equal to @Hold a Never t0@ for
-- any t0.
constant :: a -> Cell a
constant = Constant

-- | @b.never()@. Never, section 5.1.
never :: Stream a
never = Never

------------------------------------------------------------------------------
-- Adapters

-- | MapS, section 5.2.
map :: Stream a -> (a -> b) -> Stream b
map s f = MapS f s

-- | Filter, section 5.5.
filter :: Stream a -> (a -> Bool) -> Stream a
filter s p = Filter p s

-- | Sodium's filterOptional / filterJust after a map. Not Split of
-- maybeToList: Split would move the event to a child time.
filterMap :: Stream a -> (a -> Maybe b) -> Stream b
filterMap s f = MapS fromJust (Filter isJust (MapS f s))

-- | MapS of const.
mapTo :: Stream a -> b -> Stream b
mapTo s b = MapS (const b) s

-- | Snapshot, section 5.3. The cell is read before the instant: 'D.at'
-- keeps only the steps strictly before it.
snapshot :: Stream a -> Cell b -> (a -> b -> c) -> Stream c
snapshot s c f = Snapshot f s c

-- | Sodium's gate: filterJust of a snapshot, so the cell is read before
-- the instant (haskell/src/FRP/Sodium/Context.hs:153, Stream.java:462).
gate :: Stream a -> Cell Bool -> Stream a
gate s c = MapS fromJust (Filter isJust (Snapshot (\a b -> if b then Just a else Nothing) s c))

-- | @once@: the first event at or after the creation time t0
-- (Stream.java:537, "starting from the transaction in which once() was
-- invoked"). The semantics has no constructor for it; this reference form
-- uses the semantic function occs. t0 is the time of the materializer that
-- fuses the chain.
once :: Stream a -> Reactive (Stream a)
once s = Reactive $ \t0 -> MkStream (take 1 (P.filter ((>= t0) . fst) (D.occs s)))

-- | @once@ with constructors only: a gate on a hold of its own output, a
-- cell loop with a filter inside, computed as a fixed point. Its occs agree
-- with 'once' from t0 on; before t0 it repeats the input, which nothing
-- created at t0 can observe.
onceLoop :: Stream a -> Reactive (Stream a)
onceLoop s = do
    active <- cellLoop (\fwd -> hold (mapTo (gate s fwd) False) True)
    return (gate s active)

------------------------------------------------------------------------------
-- Stream materializers

-- | Hold, section 5.12, created at t0.
hold :: Stream a -> a -> Reactive (Cell a)
hold s initial = D.hold initial s

-- | Sodium's accum: the hold snapshots itself. The event spine comes from
-- the input alone, so the lazy knot terminates.
accumulate :: Stream a -> s -> (a -> s -> s) -> Reactive (Cell s)
accumulate s initial f = Reactive $ \t0 ->
    let c = Hold initial (Snapshot f s c) t0 in c

-- | @accumulate_mut@: f gives the state after the in-place mutation. It runs
-- at commit, after every reader saw the old state, so it denotes the same
-- cell as 'accumulate'.
accumulateMut :: Stream a -> s -> (a -> s -> s) -> Reactive (Cell s)
accumulateMut = accumulate

-- | Sodium's collect (collectE): one output per event, the state held. The
-- state is created at t0; the output stream has no creation time, so an
-- event before t0 is in its denotation, where nothing created at t0 can
-- observe it.
scan :: Stream a -> s -> (a -> s -> (b, s)) -> Reactive (Stream b)
scan s initial f = Reactive $ \t0 ->
    let state = Hold initial (MapS snd pairs) t0
        pairs = Snapshot f s state
    in MapS fst pairs

-- | Explicit fan-out: an identity on the semantics.
share :: Stream a -> Stream a
share = id

-- | A chain materialized with an identity of its own: an identity on the
-- semantics.
node :: Stream a -> Stream a
node = id

-- | Merge, section 5.4: f is called as f left right.
merge :: Stream a -> Stream a -> (a -> a -> a) -> Stream a
merge s other f = Merge s other f

-- | Sodium's orElse: merge keeping the left event (Stream.java:330,
-- merge(s, (left, right) -> left)).
orElse :: Stream a -> Stream a -> Stream a
orElse s other = Merge s other const

-- | Split, section 5.10, with patch F7: the children sorted stably by time.
-- The text orders the children by their parents' events, which is time
-- order unless the split is fed by its own children; there a parent at
-- @[0,0]@ comes after a parent at @[0]@, and its children @[0,0,0]@ come
-- after @[0,1]@. The sort changes nothing on a stream the text already
-- orders, which covers every vector.
split :: Stream [a] -> Stream a
split s = MkStream (sortOn fst (D.occs (Split s)))

-- | Split, section 5.10, exactly as the text writes it.
splitText :: Stream [a] -> Stream a
splitText = Split

-- | Sodium's defer: split of a singleton (Operational.java:46). A defer and
-- a split in one instant share child index 0.
defer :: Stream a -> Stream a
defer s = split (MapS (: []) s)

-- | Execute, section 5.7: the closure runs at each event's time with a
-- fresh build context.
construct :: Stream a -> (a -> Reactive b) -> Stream b
construct s f = Execute (MapS f s)

------------------------------------------------------------------------------
-- Cell operations

-- | SwitchS, section 5.6. No creation time.
switchStream :: Cell (Stream a) -> Stream a
switchStream = SwitchS

-- | SwitchC, section 5.15, exactly as the text writes it.
switchCellText :: Cell (Cell a) -> Reactive (Cell a)
switchCellText c = Reactive (SwitchC c)

-- | SwitchC, section 5.15, with patch F6: the outer cell chopped at t0,
-- @SwitchC (concrete (chopFront (steps c) t0)) t0@. Equal to
-- 'switchCellText' whenever the outer has no step before t0, which covers
-- every vector in the text. Different when a switch cell is created after
-- its outer stepped: there the text starts from the deselected inner, emits
-- a step to it at t0, and emits steps before t0; normalize, bound to t0,
-- can move the new inner's step at t0 back to the outer's switch, so that
-- sample at t0 reads the value after it.
switchCell :: Cell (Cell a) -> Reactive (Cell a)
switchCell c = Reactive $ \t0 -> SwitchC (concrete (D.chopFront (D.steps c) t0)) t0

-- | MapC, section 5.13.
mapCell :: Cell a -> (a -> b) -> Cell b
mapCell c f = MapC f c

-- | lift over a tuple of cells: MapC then Apply, section 5.14, as
-- Cell.java:161 does it.
lift2 :: (Cell a, Cell b) -> (a -> b -> r) -> Cell r
lift2 (a, b) f = Apply (MapC f a) b

lift3 :: (Cell a, Cell b, Cell c) -> (a -> b -> c -> r) -> Cell r
lift3 (a, b, c) f = Apply (Apply (MapC f a) b) c

lift4 :: (Cell a, Cell b, Cell c, Cell d) -> (a -> b -> c -> d -> r) -> Cell r
lift4 (a, b, c, d) f = Apply (Apply (Apply (MapC f a) b) c) d

lift5 :: (Cell a, Cell b, Cell c, Cell d, Cell e)
      -> (a -> b -> c -> d -> e -> r) -> Cell r
lift5 (a, b, c, d, e) f = Apply (Apply (Apply (Apply (MapC f a) b) c) d) e

lift6 :: (Cell a, Cell b, Cell c, Cell d, Cell e, Cell g)
      -> (a -> b -> c -> d -> e -> g -> r) -> Cell r
lift6 (a, b, c, d, e, g) f =
    Apply (Apply (Apply (Apply (Apply (MapC f a) b) c) d) e) g

-- | Sodium's updates: Updates, section 5.8. No creation time.
steps :: Cell a -> Stream a
steps = Updates

-- | Sodium's value: Value, section 5.9, created at t0.
stepsWithCurrent :: Cell a -> Reactive (Stream a)
stepsWithCurrent = D.value

-- | The Reactive sample, section 5.16: the value before the instant.
sample :: Cell a -> Reactive a
sample = D.sample

------------------------------------------------------------------------------
-- Loops

-- | A cell with the given (initial value, steps) and no creation filter.
-- Its creation time is @[]@, which is before every time, so it keeps every
-- step.
concrete :: C a -> Cell a
concrete (a, sts) = Hold a (MkStream sts) []

-- | The name the research gave 'concrete'.
concreteCell :: C a -> Cell a
concreteCell = concrete

-- | The cell as it is from t0 on: equal to c at every t >= t0.
chopCell :: T -> Cell a -> Cell a
chopCell t0 c = concrete (D.chopFront (D.steps c) t0)

-- | A cell loop as an explicit fixed point. The forward cell is a concrete
-- cell holding the previous iterate's steps; the first never steps and
-- has an initial value that is an error to read, as sampling an open loop
-- is in Bough. Returns the fixed point and the number of iterations that
-- reached it (body evaluations, not counting the one that confirms it).
fixCell :: Eq a => (Cell a -> Cell a) -> (Cell a, Int)
fixCell body = go 1 start
  where
    open = concrete (error "cell loop sampled before close", [])
    start = concrete (fst (D.steps (body open)), [])
    go n prev =
        let next = concrete (D.steps (body prev))
        in if D.steps next == D.steps prev then (prev, n - 1) else go (n + 1) next

-- | A stream loop as an explicit fixed point, starting from a stream that
-- never fires.
fixStream :: Eq a => (Stream a -> Stream a) -> (Stream a, Int)
fixStream body = go 1 (MkStream [])
  where
    go n prev =
        let next = MkStream (D.occs (body prev))
        in if D.occs next == D.occs prev then (prev, n - 1) else go (n + 1) next

-- | @b.cell_loop()@ and @close@: the body maps the forward cell to the
-- definition, all built at the time of the declaring scope.
cellLoop :: Eq a => (Cell a -> Reactive (Cell a)) -> Reactive (Cell a)
cellLoop body = fst <$> cellLoopCount body

cellLoopCount :: Eq a => (Cell a -> Reactive (Cell a)) -> Reactive (Cell a, Int)
cellLoopCount body = Reactive $ \t -> fixCell (\fwd -> run (body fwd) t)

-- | @b.stream_loop()@ and @close@.
streamLoop :: Eq a => (Stream a -> Reactive (Stream a)) -> Reactive (Stream a)
streamLoop body = fst <$> streamLoopCount body

streamLoopCount :: Eq a => (Stream a -> Reactive (Stream a)) -> Reactive (Stream a, Int)
streamLoopCount body = Reactive $ \t -> fixStream (\fwd -> run (body fwd) t)

-- | The lazy knot. Terminates when the event spine does not depend on the
-- loop's values (accumulate, scan, the uncapped counter); diverges when it
-- does (a filter or gate inside the loop, a merge in a stream loop).
cellLoopKnot :: (Cell a -> Reactive (Cell a)) -> Reactive (Cell a)
cellLoopKnot body = Reactive $ \t -> let c = run (body c) t in c

streamLoopKnot :: (Stream a -> Reactive (Stream a)) -> Reactive (Stream a)
streamLoopKnot body = Reactive $ \t -> let s = run (body s) t in s

------------------------------------------------------------------------------
-- Sodium operations Bough does not name

-- | Conal's switcher, section 4: hold then switch_cell, both at [0].
switcher :: Cell a -> Stream (Cell a) -> Cell a
switcher c s = SwitchC (Hold c s [0]) [0]

-- | Apply, section 5.14: Bough spells it @(cf, ca).lift(b, |f, a| f(a))@.
apply :: Cell (a -> b) -> Cell a -> Cell b
apply = Apply

-- | The "Coalesce" of section 5.8. Not a constructor; on a 1.1 stream it is
-- the identity, since no two events share a time.
coalesceStream :: (a -> a -> a) -> Stream a -> Stream a
coalesceStream f s = MkStream (D.coalesce f (D.occs s))

-- | snapshot2, section 5.3: Snapshot from MapS, sample and Execute.
snapshotViaExecute :: (a -> b -> c) -> Stream a -> Cell b -> Stream c
snapshotViaExecute = D.snapshot2
