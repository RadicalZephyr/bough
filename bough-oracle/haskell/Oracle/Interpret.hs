{-# LANGUAGE GADTs #-}
-- | A 'Program' to terms of Reactive.Sodium.Denotational, and the answer.
--
-- A program is checked first, so a malformed one answers @ERR@ with the
-- node at fault. Then each scope is interpreted: the top level at @[0]@,
-- and each construct body at its event's instant. Every node's term is
-- replaced by a concrete term with the same @occs@ or @steps@, which the
-- text's own observers cannot tell apart, so that a node read by many
-- others is computed once.
--
-- Loops are computed by explicit fixed-point iteration, never by lazy
-- knot-tying, because the text does not terminate on a loop whose spine
-- depends on its values (a filter inside a loop through a hold). Each loop
-- node is bound to a concrete term, a cell loop to @Hold v0 (MkStream sts)
-- []@ and a stream loop to @MkStream occs@, starting from a cell that never
-- steps (initial value the type's zero) and a stream that never fires.
-- Each round interprets the whole program again and takes the steps or
-- events of each loop's definition as its next iterate. Every loop iterates
-- in the same rounds: the top level's, and those of each construct body's
-- run, one set of iterates per run. A body's loops are never solved on
-- their own inside a round, where the loops around them may still be far
-- from their answer. The answer is the round whose iterates reproduce
-- themselves.
--
-- The fixed point from a never-stepping start is the semantics for
-- well-founded loops: those whose back edge is a read before the instant
-- (snapshot, gate, sample, the selection of switch_stream) or a child
-- instant (split, defer). A back edge through a steps view is a same-instant
-- cycle, which the text diverges on and the iteration may still settle;
-- the generator must not produce one.
--
-- A round settles at least one more instant of a well-founded loop, or one
-- more loop where loops read one another within an instant, so the rounds
-- a loop needs grow with the instants its answer chains through: the
-- counter over n transactions needs n + 1. The rounds allowed grow with the
-- loops too ('roundsAllowed'), with room for every loop that settles; past
-- them the answer is @ERR@. A loop whose iterates keep growing, such as a
-- stream loop through defer with no filter to stop it (finding F22), never
-- reaches them, and the caller's time limit ends it instead.
module Oracle.Interpret
  ( -- * The protocol
    respond
  , answer
    -- * Checking and interpreting
  , check
  , interpret
  , LoopMode (..)
  , Observation (..)
  , Value (..)
  , OracleError (..)
  , roundsAllowed
  , render
  ) where

import Control.DeepSeq (NFData (..), deepseq, force)
import Control.Exception
  ( AsyncException (..), Exception, SomeAsyncException (..), SomeException
  , catch, displayException, evaluate, fromException, throw, throwIO )
import Control.Monad (foldM, forM_, unless, when, zipWithM_)
import Data.IntMap.Lazy (IntMap)
import qualified Data.IntMap.Lazy as IntMap
import Data.List (intercalate)
import Data.Map.Lazy (Map)
import qualified Data.Map.Lazy as Map
import Data.Maybe (isJust)
import Text.Read (readMaybe)
import Reactive.Sodium.Denotational (Stream (..), Cell (..), Reactive (..), T, S, C)
import qualified Reactive.Sodium.Denotational as D
import qualified Oracle.Derived as B
import Oracle.Program

------------------------------------------------------------------------------
-- The protocol

-- | Answers one line: @OK@ and the observations as JSON, or @ERR@ and why.
-- Errors found while evaluating (a loop that does not converge) are thrown
-- when the string is forced; 'answer' catches them.
respond :: String -> String
respond line = case readMaybe line of
    Nothing -> "ERR parse: the line is not a Program in the syntax of Oracle.Program"
    Just program -> case interpret FixedPoint program of
        Left message -> "ERR " ++ message
        Right observations -> "OK " ++ render observations

-- | 'respond', forced, with every failure turned into an @ERR@ line. The
-- caller adds the time limit. A heap overflow (the process runs with
-- @-M1g@ unless told otherwise) answers @ERR heap@; other asynchronous
-- exceptions, the time limit's among them, pass through.
answer :: String -> IO String
answer line = evaluate (force (respond line)) `catch` failure
  where
    failure :: SomeException -> IO String
    failure e
        | Just HeapOverflow <- fromException e =
            return "ERR heap: the program needed more heap than the limit allows"
        | Just StackOverflow <- fromException e =
            return "ERR stack: the program overflowed the stack"
        | Just (SomeAsyncException _) <- fromException e = throwIO e
        | otherwise = return ("ERR " ++ oneLine (message e))
    message e = case fromException e of
        Just (OracleError text) -> text
        Nothing -> displayException e
    oneLine = map (\c -> if c == '\n' || c == '\r' then ' ' else c)

-- | The JSON the answer carries: one element per observed node, a stream as
-- @[0, [[t, v], ...]]@ and a cell as @[1, v0, [[t, v], ...]]@.
render :: [Observation] -> String
render = list . map observation
  where
    observation (Events events) = list ["0", occurrences events]
    observation (Steps initial sts) = list ["1", value initial, occurrences sts]
    occurrences events = list [ list [list (map show t), value v] | (t, v) <- events ]
    value (VInt n) = show n
    value (VBool b) = if b then "1" else "0"
    value (VList ns) = list (map show ns)
    value other = throw (OracleError ("cannot answer " ++ show other))
    list elements = "[" ++ intercalate "," elements ++ "]"

------------------------------------------------------------------------------
-- Values

-- | A value at run time. Tokens are values, for the higher-order operations.
data Value
  = VInt Int
  | VBool Bool
  | VList [Int]
  | VStream (Stream Value)
  | VCell (Cell Value)

-- | First-order values compare. Tokens never need to: loops are first-order,
-- and only loop iterates and test expectations are compared.
instance Eq Value where
    VInt a == VInt b = a == b
    VBool a == VBool b = a == b
    VList a == VList b = a == b
    a == b = throw (OracleError ("compared " ++ show a ++ " with " ++ show b))

instance Show Value where
    showsPrec d (VInt n) = showParen (d > 10) (showString "VInt " . showsPrec 11 n)
    showsPrec d (VBool b) = showParen (d > 10) (showString "VBool " . showsPrec 11 b)
    showsPrec d (VList ns) = showParen (d > 10) (showString "VList " . showsPrec 11 ns)
    showsPrec _ (VStream _) = showString "<stream>"
    showsPrec _ (VCell _) = showString "<cell>"

-- | Forces a first-order value whole. A token is left as it is: loops are
-- first-order, and only their iterates are forced.
instance NFData Value where
    rnf (VInt n) = rnf n
    rnf (VBool b) = rnf b
    rnf (VList ns) = rnf ns
    rnf (VStream _) = ()
    rnf (VCell _) = ()

-- | What the answer carries for one observed node, cut to the window.
data Observation
  = -- | A stream's events.
    Events (S Value)
  | -- | A cell's value at the start of the window, and its steps in it.
    Steps Value (S Value)
  deriving (Eq, Show)

-- | An error in a program, found while checking or evaluating it.
newtype OracleError = OracleError String

instance Show OracleError where
    show (OracleError text) = text

instance Exception OracleError

------------------------------------------------------------------------------
-- Checking

-- | What a definition makes: a stream or a cell of the given type
-- ('TStreamOf' or 'TCellOf'), or nothing, for 'Close'.
type NodeType = Maybe Ty

-- | What a name means where a definition is checked.
data TypeScope = TypeScope
  { typeInputs :: [Ty]
  , typeTop :: IntMap NodeType
    -- ^ The top-level nodes in scope.
  , typeLocal :: Maybe (IntMap NodeType)
    -- ^ The body's nodes in scope; 'Nothing' at the top level.
  , typeEvent :: Maybe Ty
    -- ^ The construct's event, in a body.
  , typePath :: String
    -- ^ Where this scope is, for messages: "" at the top level, "body of "
    -- in a construct body.
  }

-- | Checks a program. Returns the type of each top-level node, or the first
-- thing wrong with it.
check :: Program -> Either String [NodeType]
check (Program _ inputs defs observed schedule) = do
    inputTypes <- mapM checkInput (zip [0 ..] inputs)
    types <- checkScope (TypeScope inputTypes IntMap.empty Nothing Nothing "") defs
    forM_ observed (checkObserved types)
    zipWithM_ (checkTransaction inputs) [1 :: Int ..] schedule
    return types

checkInput :: (Int, Input) -> Either String Ty
checkInput (k, Input ty coalescing) = do
    unless (firstOrder ty) (Left (at ++ "an input carries numbers, booleans or lists, not " ++ show ty))
    case coalescing of
        Nothing -> Right ()
        Just e -> do
            unless (scalar ty) (Left (at ++ "an input of lists cannot coalesce: an expression gives a number"))
            prefixed at (checkE (TypeScope [] IntMap.empty Nothing Nothing "") 2 e)
    return ty
  where
    at = "input " ++ show k ++ ": "

checkObserved :: [NodeType] -> Int -> Either String ()
checkObserved types i = case drop i types of
    _ | i < 0 -> Left ("observe: there is no node " ++ show i)
    [] -> Left ("observe: there is no node " ++ show i)
    Nothing : _ -> Left ("observe: node " ++ show i ++ " is a Close, which defines no node")
    Just ty : _
        | Just inner <- contents ty, firstOrder inner -> Right ()
        | otherwise -> Left ("observe: node " ++ show i ++ " has type " ++ show ty ++ "; only first-order values can be answered")

checkTransaction :: [Input] -> Int -> [(Int, V)] -> Either String ()
checkTransaction inputs k sends = do
    forM_ sends $ \(i, v) -> case drop i inputs of
        _ | i < 0 -> Left (at ++ "there is no input " ++ show i)
        [] -> Left (at ++ "there is no input " ++ show i)
        Input ty _ : _ ->
            unless (valueType v == ty) (Left (at ++ "input " ++ show i ++ " carries " ++ show ty ++ ", and the send is " ++ show v))
    forM_ (zip [0 :: Int ..] inputs) $ \(i, Input _ coalescing) ->
        when (coalescing == Nothing && length (filter ((== i) . fst) sends) > 1) $
            Left (at ++ "input " ++ show i ++ " is sent more than once and does not coalesce")
  where
    at = "transaction " ++ show k ++ ": "

-- | Checks the definitions of one scope in order, then that every loop is
-- closed exactly once.
checkScope :: TypeScope -> [Def] -> Either String [NodeType]
checkScope scope defs = do
    types <- foldM step IntMap.empty (zip [0 ..] defs)
    forM_ [ i | (i, d) <- zip [0 ..] defs, Just _ <- [loopDeclaration d] ] $ \i ->
        case length [ () | Close j _ <- defs, j == i ] of
            1 -> Right ()
            0 -> Left (typePath scope ++ "node " ++ show i ++ ": the loop is never closed")
            _ -> Left (typePath scope ++ "node " ++ show i ++ ": the loop is closed more than once")
    return (IntMap.elems types)
  where
    step types (i, def) = do
        let here = case typeLocal scope of
                Nothing -> scope { typeTop = types }
                Just _ -> scope { typeLocal = Just types }
            at = typePath scope ++ "node " ++ show i ++ " (" ++ name def ++ "): "
        t <- prefixed at (checkDef here def)
        case def of
            Close j r -> prefixed at (checkClose here i j r)
            _ -> Right ()
        return (IntMap.insert i t types)
    checkClose here i j r = do
        when (j < 0 || j >= i) (Left ("node " ++ show j ++ " is not a loop declared before this Close"))
        declared <- case loopDeclaration (defs !! j) of
            Just (isCell, ty) -> Right (if isCell then TCellOf ty else TStreamOf ty)
            Nothing -> Left ("node " ++ show j ++ " is not a loop")
        actual <- resolveType here r
        unless (actual == declared)
            (Left ("the loop has type " ++ show declared ++ ", and " ++ show r ++ " has type " ++ show actual))

-- | The type of the node a definition makes, given the nodes before it.
checkDef :: TypeScope -> Def -> Either String NodeType
checkDef scope def = case def of
    SInput k -> input k >>= stream
    CInput k e -> do
        ty <- input k
        unless (scalar ty) (Left "an input cell holds numbers or booleans")
        expression 0 e
        cell ty
    SNever ty -> stream ty
    CConstant e -> expression 0 e >> cell TInt
    SLit ty events -> do
        unless (firstOrder ty) (Left ("a literal stream carries numbers, booleans or lists, not " ++ show ty))
        forM_ events $ \(t, v) ->
            unless (valueType v == ty) (Left ("the event at " ++ show t ++ " is " ++ show v ++ ", not " ++ show ty))
        let times = map fst events
        unless (and (zipWith (<) times (drop 1 times))) (Left "the times of a literal stream must increase")
        stream ty
    SMap e r -> scalarStream r >> expression 1 e >> stream TInt
    SFilter e r -> do ty <- scalarStream r; expression 1 e; stream ty
    SFilterMap e1 e2 r -> scalarStream r >> expression 1 e1 >> expression 1 e2 >> stream TInt
    SMapTo v r -> anyStream r >> stream (valueType v)
    SSnapshot e s c -> scalarStream s >> scalarCell c >> expression 2 e >> stream TInt
    SGate s c -> do
        ty <- anyStream s
        inner <- anyCell c
        unless (inner == TBool) (Left (show c ++ " is a cell of " ++ show inner ++ "; a gate reads a cell of booleans"))
        stream ty
    SOnce r -> anyStream r >>= stream
    SMapList e1 e2 r -> scalarStream r >> expression 1 e1 >> expression 2 e2 >> stream TList
    SPickStream e rs r -> do
        _ <- scalarStream r
        expression 1 e
        ty <- same "streams" =<< mapM anyStream rs
        stream (TStreamOf ty)
    SPickCell e rs r -> do
        _ <- scalarStream r
        expression 1 e
        ty <- same "cells" =<< mapM anyCell rs
        stream (TCellOf ty)
    SNode r -> anyStream r >>= stream
    SShare r -> anyStream r >>= stream
    SMerge e l r -> do
        ty <- scalarStream l
        ty' <- scalarStream r
        unless (ty == ty') (Left ("a merge needs two streams of one type, and they carry " ++ show ty ++ " and " ++ show ty'))
        expression 2 e
        stream ty
    SOrElse l r -> do
        ty <- anyStream l
        ty' <- anyStream r
        unless (ty == ty') (Left ("or_else needs two streams of one type, and they carry " ++ show ty ++ " and " ++ show ty'))
        stream ty
    SSplit r -> do
        ty <- anyStream r
        unless (ty == TList) (Left (show r ++ " carries " ++ show ty ++ "; split needs lists"))
        stream TInt
    SDefer r -> anyStream r >>= stream
    SScan e0 eo es r -> do
        _ <- scalarStream r
        expression 0 e0
        expression 2 eo
        expression 2 es
        stream TInt
    SSteps r -> anyCell r >>= stream
    SStepsWithCurrent r -> anyCell r >>= stream
    SSwitch r -> do
        ty <- anyCell r
        case ty of
            TStreamOf inner -> stream inner
            _ -> Left (show r ++ " is a cell of " ++ show ty ++ "; switch_stream needs a cell of streams")
    SConstruct body r -> do
        event <- anyStream r
        result <- checkBody scope event body
        stream result
    CHold e r -> do
        ty <- anyStream r
        unless (scalar ty) (Left (show r ++ " carries " ++ show ty ++ "; hold's initial expression gives a number or a boolean"))
        expression 0 e
        cell ty
    CHoldStream r0 r -> do
        ty <- anyStream r0
        outer <- anyStream r
        unless (outer == TStreamOf ty) (Left (show r ++ " carries " ++ show outer ++ ", not " ++ show (TStreamOf ty)))
        cell (TStreamOf ty)
    CHoldCell r0 r -> do
        ty <- anyCell r0
        outer <- anyStream r
        unless (outer == TCellOf ty) (Left (show r ++ " carries " ++ show outer ++ ", not " ++ show (TCellOf ty)))
        cell (TCellOf ty)
    CConstantStream r -> anyStream r >>= cell . TStreamOf
    CConstantCell r -> anyCell r >>= cell . TCellOf
    CAccum e0 e r -> scalarStream r >> expression 0 e0 >> expression 2 e >> cell TInt
    CAccumMut e0 e r -> scalarStream r >> expression 0 e0 >> expression 2 e >> cell TInt
    CMapCell e r -> scalarCell r >> expression 1 e >> cell TInt
    CToBool r -> scalarCell r >> cell TBool
    CMapPickCell e rs r -> do
        _ <- scalarCell r
        expression 1 e
        ty <- same "cells" =<< mapM anyCell rs
        cell (TCellOf ty)
    CLift e rs -> do
        unless (length rs >= 2 && length rs <= 6) (Left ("lift takes two to six cells, not " ++ show (length rs)))
        mapM_ scalarCell rs
        expression (length rs) e
        cell TInt
    CSwitch r -> do
        ty <- anyCell r
        case ty of
            TCellOf inner -> cell inner
            _ -> Left (show r ++ " is a cell of " ++ show ty ++ "; switch_cell needs a cell of cells")
    CLoop ty -> loop ty >> cell ty
    SLoop ty -> loop ty >> stream ty
    Close _ r -> resolveType scope r >> return Nothing
  where
    stream = Right . Just . TStreamOf
    cell = Right . Just . TCellOf
    input k
        | k >= 0 && k < length (typeInputs scope) = Right (typeInputs scope !! k)
        | otherwise = Left ("there is no input " ++ show k)
    expression n e = checkE scope n e
    anyStream r = do
        ty <- resolveType scope r
        case ty of
            TStreamOf inner -> Right inner
            _ -> Left (show r ++ " is " ++ show ty ++ ", not a stream")
    anyCell r = do
        ty <- resolveType scope r
        case ty of
            TCellOf inner -> Right inner
            _ -> Left (show r ++ " is " ++ show ty ++ ", not a cell")
    scalarStream r = do
        ty <- anyStream r
        unless (scalar ty) (Left (show r ++ " carries " ++ show ty ++ "; this needs numbers or booleans"))
        return ty
    scalarCell r = do
        ty <- anyCell r
        unless (scalar ty) (Left (show r ++ " holds " ++ show ty ++ "; this needs numbers or booleans"))
        return ty
    same _ [] = Left "the list of choices is empty"
    same what (ty : rest) = do
        unless (all (== ty) rest) (Left ("the listed " ++ what ++ " must have one type"))
        return ty
    loop ty = unless (firstOrder ty) (Left ("a loop carries numbers, booleans or lists, not " ++ show ty))

-- | The type of the token a body emits, checked in a scope of its own.
checkBody :: TypeScope -> Ty -> Body -> Either String Ty
checkBody scope event (Body defs result) = do
    types <- checkScope inner defs
    let here = inner { typeLocal = Just (IntMap.fromList (zip [0 ..] types)) }
    case result of
        RValue e -> prefixed (typePath inner ++ "result: ") (checkE here 0 e) >> return TInt
        RNode r -> prefixed (typePath inner ++ "result: ") (resolveType here r)
  where
    inner = bodyScope scope event

-- | The scope a construct body is checked in. Messages from it are
-- prefixed with the construct's own location on the way out.
bodyScope :: TypeScope -> Ty -> TypeScope
bodyScope scope event = scope
    { typeLocal = Just IntMap.empty
    , typeEvent = Just event
    , typePath = "body of "
    }

-- | Checks an expression taking n arguments.
checkE :: TypeScope -> Int -> E -> Either String ()
checkE scope arguments = go
  where
    go e = case e of
        Arg -> argument "Arg" 0
        Arg2 -> argument "Arg2" 1
        ArgN k -> argument ("ArgN " ++ show k) k
        CArg -> case typeEvent scope of
            Nothing -> Left "CArg is used outside a construct body"
            Just ty
                | scalar ty -> Right ()
                | otherwise -> Left ("CArg: the construct's event is " ++ show ty ++ ", not a number or a boolean")
        Sample r -> do
            ty <- resolveType scope r
            case ty of
                TCellOf inner | scalar inner -> Right ()
                _ -> Left ("Sample " ++ show r ++ ": " ++ show ty ++ " is not a cell of numbers or booleans")
        Lit _ -> Right ()
        Add a b -> go a >> go b
        Sub a b -> go a >> go b
        Mul a b -> go a >> go b
        Mod a k -> do
            when (k <= 0) (Left ("Mod needs a positive divisor, not " ++ show k))
            go a
        Max a b -> go a >> go b
        Min a b -> go a >> go b
        Eq a b -> go a >> go b
        Lt a b -> go a >> go b
        Not a -> go a
        If c a b -> go c >> go a >> go b
    argument label k
        | k >= 0 && k < arguments = Right ()
        | otherwise = Left (label ++ " is not bound: this expression takes " ++ show arguments ++ " argument(s)")

-- | The type of the node a reference names.
resolveType :: TypeScope -> Ref -> Either String Ty
resolveType scope ref = do
    found <- case (ref, typeLocal scope) of
        (N i, _) -> lookUp i (typeTop scope)
        (Local _, Nothing) -> Left (show ref ++ " names a body's node, and this is the top level")
        (Local j, Just local) -> lookUp j local
    maybe (Left (show ref ++ " is a Close, which defines no node")) Right found
  where
    lookUp i nodes = maybe (Left (show ref ++ " does not name a node defined before this one")) Right (IntMap.lookup i nodes)

------------------------------------------------------------------------------
-- Interpreting

-- | How loops are computed.
data LoopMode
  = -- | Explicit iteration from a never-stepping start: the oracle's answer.
    FixedPoint
  | -- | Lazy knot-tying, as the text would write it. It terminates only when
    -- no loop's spine depends on its values; tests compare it with
    -- 'FixedPoint' there.
    Knot
  deriving (Eq, Show)

-- | The rounds the loops may take before the answer is @ERR@, given the
-- size of the largest state a round has made ('stateSize'): 200, and two
-- for every loop, step, event and body run in it. A round settles at least
-- one more instant of a well-founded loop, so a loop that settles needs
-- about a round for each step or event its answer chains through, and
-- these rounds leave it room twice over. A loop that never settles and
-- stays small, such as a same-instant cycle that counts up at one instant,
-- is stopped by them.
roundsAllowed :: Int -> Int
roundsAllowed size = 200 + 2 * size

-- | A node at run time.
data Node = NStream (Stream Value) | NCell (Cell Value) | NClose

-- | An iterate of a loop: a cell's initial value and steps, or a stream's
-- events.
data Iterate = CellIterate (C Value) | StreamIterate (S Value)
  deriving (Eq)

instance NFData Iterate where
    rnf (CellIterate c) = rnf c
    rnf (StreamIterate s) = rnf s

-- | The iterates one round starts from: those of a scope's loops, by node,
-- and those of each run of a construct body that has loops, by construct
-- node and instant, with the runs of that body's own constructs inside. A
-- loop that a state lacks is at its start, a cell that never steps and
-- holds its type's zero or a stream that never fires, and so is every loop
-- of a run that it lacks.
data State = State (IntMap Iterate) (Map (Int, T) State)

instance NFData State where
    rnf (State loops runs) = rnf loops `seq` rnf runs

-- | The state every loop starts from.
emptyState :: State
emptyState = State IntMap.empty Map.empty

-- | Where a scope's definitions are interpreted.
data Context = Context
  { contextMode :: LoopMode
  , contextInputs :: IntMap (Stream Value)
  , contextInputTypes :: [Ty]
  , contextTop :: IntMap Node
  , contextTopTypes :: IntMap NodeType
  , contextLocal :: Maybe (IntMap Node)
    -- ^ This body's nodes; 'Nothing' at the top level.
  , contextLocalTypes :: IntMap NodeType
  , contextTime :: T
    -- ^ The creation time of this scope's nodes.
  , contextEvent :: Maybe Value
    -- ^ The construct's event, in a body.
  }

-- | Interprets a checked program: the observations of its observed nodes,
-- cut to its window, or what is wrong with it. An error found while
-- evaluating, such as a loop that does not converge, is thrown when the
-- observations are forced.
interpret :: LoopMode -> Program -> Either String [Observation]
interpret mode program@(Program window inputs defs observed schedule) = do
    types <- check program
    let inputTypes = [ ty | Input ty _ <- inputs ]
        top = Context
            { contextMode = mode
            , contextInputs = IntMap.fromList (zip [0 ..] (zipWith inputStream [0 ..] inputs))
            , contextInputTypes = inputTypes
            , contextTop = IntMap.empty
            , contextTopTypes = IntMap.fromList (zip [0 ..] types)
            , contextLocal = Nothing
            , contextLocalTypes = IntMap.empty
            , contextTime = B.buildTime
            , contextEvent = Nothing
            }
        nodes = solve top defs
    return (map (observe window . (nodes IntMap.!)) observed)
  where
    inputStream k (Input ty coalescing) = case coalescing of
        Nothing -> B.input (sends k)
        Just e -> B.inputCoalescing (\x y -> coerce ty (evaluateE bare [x, y] e)) (sends k)
    sends k = [ ([n], valueOf v) | (n, transaction) <- zip [1 ..] schedule, (i, v) <- transaction, i == k ]
    bare = Context mode IntMap.empty [] IntMap.empty IntMap.empty Nothing IntMap.empty B.buildTime Nothing

-- | A node cut to the window.
observe :: Window -> Node -> Observation
observe window node = case (node, window) of
    (NStream s, Everything) -> Events (D.occs s)
    (NStream s, FromFirstTransaction) -> Events (fromFirst (D.occs s))
    (NCell c, Everything) -> let (initial, sts) = D.steps c in Steps initial sts
    (NCell c, FromFirstTransaction) -> let sts = D.steps c in Steps (D.at sts [1]) (fromFirst (snd sts))
    (NClose, _) -> throw (OracleError "observed a Close")
  where
    fromFirst = filter ((>= [1]) . fst)

-- | The top level's nodes, every loop solved.
--
-- Every loop iterates in the same rounds, those of the top level and those
-- of every run of a construct body alike. Each round interprets the
-- program once, each body run once, with every loop bound to its iterate
-- in the round's state, and the next state holds the steps or events of
-- each loop's definition. The answer is the round whose state reproduces
-- itself. Each new state is forced whole, so that no round holds on to the
-- one before it.
solve :: Context -> [Def] -> IntMap Node
solve context defs = case contextMode context of
    Knot -> fst (scopeRound context defs Knotted)
    FixedPoint
        | hasLoops defs -> go 1 0 emptyState
        | otherwise -> fst (scopeRound context defs (Iterates emptyState))
  where
    go :: Int -> Int -> State -> IntMap Node
    go round' largest state =
        let (nodes, next) = scopeRound context defs (Iterates state)
            largest' = max largest (stateSize next)
            changing = changes [] defs state next
        in next `deepseq` case changing of
            [] -> nodes
            _ | round' >= roundsAllowed largest' -> throw (OracleError (unsettled round' changing))
              | otherwise -> go (round' + 1) largest' next

-- | The message for loops that did not converge, naming the first few that
-- still change.
unsettled :: Int -> [String] -> String
unsettled rounds changing =
    "the loops did not converge in " ++ show rounds ++ " rounds; still changing: "
        ++ intercalate "; " (take 3 changing)
        ++ (if length changing > 3 then "; and " ++ show (length changing - 3) ++ " more" else "")

-- | How the loop nodes of a scope are bound in one interpretation.
data Binding
  = -- | To concrete terms that hold their iterates in this state.
    Iterates State
  | -- | To their own definitions, lazily: the text's knot.
    Knotted

-- | One interpretation of a scope's definitions: its nodes, and the state
-- the next round starts from. That state holds the steps or events of each
-- loop's definition, and the next state of each run of a construct body
-- that has loops. With 'Iterates', each body run is interpreted once, and
-- the construct's event and the next state share it; with 'Knotted', the
-- text's Execute runs the body at each event.
scopeRound :: Context -> [Def] -> Binding -> (IntMap Node, State)
scopeRound context defs binding = (nodes, next)
  where
    indexed = zip [0 ..] defs
    nodes = IntMap.fromList [ (i, define i def) | (i, def) <- indexed ]
    here = case contextLocal context of
        Nothing -> context { contextTop = nodes }
        Just _ -> context { contextLocal = Just nodes }
    closers = IntMap.fromList [ (j, r) | Close j r <- defs ]
    define i def = case def of
        CLoop ty -> bound i (True, ty)
        SLoop ty -> bound i (False, ty)
        Close _ _ -> NClose
        SConstruct body r -> materialize (NStream (construct i body r))
        _ -> materialize (interpretDef here def)
    bound i declaration = case binding of
        Knotted -> resolve here (closers IntMap.! i)
        Iterates (State loops _) -> case IntMap.findWithDefault (startOf declaration) i loops of
            CellIterate c -> NCell (B.concrete c)
            StreamIterate s -> NStream (MkStream s)
    construct i body r = case binding of
        Knotted -> B.construct (streamAt here r) (\a -> Reactive (\t ->
            fst (runBody here i (contentsAt here r) body a t Knotted)))
        Iterates _ -> B.construct (streamAt here r) (\_ -> Reactive (\t ->
            fst (runs IntMap.! i Map.! t)))
    -- Each construct's body runs of this round, by instant. The map needs
    -- every event of the construct's stream at once, which a round can
    -- give: with the loops bound to their iterates, nothing a construct
    -- emits feeds back into the stream it runs on.
    runs = IntMap.fromList
        [ (i, Map.fromList
            [ (t, runBody here i (contentsAt here r) body a t (Iterates (runState (i, t))))
            | (t, a) <- D.occs (streamAt here r) ])
        | (i, SConstruct body r) <- indexed ]
    runState key = case binding of
        Iterates (State _ states) -> Map.findWithDefault emptyState key states
        Knotted -> emptyState
    next = State
        (IntMap.map (iterateOf . resolve here) closers)
        (Map.fromList
            [ ((i, t), state)
            | (i, SConstruct (Body bodyDefs _) _) <- indexed
            , hasLoops bodyDefs
            , (t, (_, state)) <- Map.toList (runs IntMap.! i) ])
    iterateOf (NCell c) = CellIterate (D.steps c)
    iterateOf (NStream s) = StreamIterate (D.occs s)
    iterateOf NClose = throw (OracleError "a loop is closed by a Close")

-- | Whether a scope, or a construct body in it, declares a loop.
hasLoops :: [Def] -> Bool
hasLoops = any declares
  where
    declares (SConstruct (Body defs _) _) = hasLoops defs
    declares def = isJust (loopDeclaration def)

-- | A loop's iterate at the start: a cell that never steps and holds its
-- type's zero, or a stream that never fires.
startOf :: (Bool, Ty) -> Iterate
startOf (True, ty) = CellIterate (zeroValue ty, [])
startOf (False, _) = StreamIterate []

-- | The loops whose iterates differ between two states of one scope, each
-- named with the scope it is in and where it first differs: "node 1 at
-- [3]", or "node 2, body at [1], node 0 at [1,0]" for a loop of a body run.
-- A loop or a body run that a state lacks is at its start.
changes :: [String] -> [Def] -> State -> State -> [String]
changes path defs (State loops runs) (State loops' runs') =
    [ intercalate ", " (path ++ ["node " ++ show i ++ " " ++ place])
    | (i, declaration) <- declared
    , Just place <- [difference (iterateIn loops i declaration) (iterateIn loops' i declaration)] ]
    ++ concat
        [ changes (path ++ ["node " ++ show i, "body at " ++ show t]) (bodyDefs i) (runIn runs key) (runIn runs' key)
        | key@(i, t) <- Map.keys (Map.union runs runs') ]
  where
    declared = [ (i, declaration) | (i, def) <- zip [0 ..] defs, Just declaration <- [loopDeclaration def] ]
    iterateIn m i declaration = IntMap.findWithDefault (startOf declaration) i m
    runIn m key = Map.findWithDefault emptyState key m
    bodyDefs i = case drop i defs of
        SConstruct (Body inner _) _ : _ -> inner
        _ -> throw (OracleError ("node " ++ show i ++ " ran a body and is not a construct"))

-- | Where two iterates of one loop first differ, if they do: "at [3]", or
-- "before its first step" for a cell whose value before its steps differs.
difference :: Iterate -> Iterate -> Maybe String
difference (CellIterate (a, sts)) (CellIterate (b, uts))
    | a /= b = Just "before its first step"
    | otherwise = ("at " ++) . show <$> firstDifference sts uts
difference (StreamIterate s) (StreamIterate u) = ("at " ++) . show <$> firstDifference s u
difference _ _ = throw (OracleError "a loop's iterate changed from a cell to a stream")

-- | The first time at which two lists of steps or events differ.
firstDifference :: S Value -> S Value -> Maybe T
firstDifference ((t, a) : rest) ((u, b) : rest')
    | t == u && a == b = firstDifference rest rest'
    | otherwise = Just (min t u)
firstDifference ((t, _) : _) [] = Just t
firstDifference [] ((u, _) : _) = Just u
firstDifference [] [] = Nothing

-- | How much a state holds: one for each loop, step, event and body run,
-- over every body run. The rounds allowed grow with it.
stateSize :: State -> Int
stateSize (State loops runs) =
    sum [ 1 + steps' iterate' | iterate' <- IntMap.elems loops ]
        + sum [ 1 + stateSize run' | run' <- Map.elems runs ]
  where
    steps' (CellIterate (_, sts)) = length sts
    steps' (StreamIterate events) = length events

-- | The same node as a concrete term: the text observes a term only through
-- @occs@ and @steps@, so the two cannot be told apart, and the concrete one
-- is computed once however many nodes read it.
materialize :: Node -> Node
materialize (NStream s) = NStream (MkStream (D.occs s))
materialize (NCell c) = NCell (let ~(initial, sts) = D.steps c in Hold initial (MkStream sts) [])
materialize NClose = NClose

-- | The denotation of a node. Loops, closes and constructs are bound by
-- 'scopeRound'.
interpretDef :: Context -> Def -> Node
interpretDef context def = case def of
    SInput k -> NStream (contextInputs context IntMap.! k)
    CInput k e -> NCell (created (B.hold (contextInputs context IntMap.! k)
        (coerce (contextInputTypes context !! k) (expression [] e))))
    SNever _ -> NStream B.never
    CConstant e -> NCell (B.constant (VInt (expression [] e)))
    SLit _ events -> NStream (MkStream [ (t, valueOf v) | (t, v) <- events ])
    SMap e r -> NStream (B.map (stream r) (\a -> VInt (expression [a] e)))
    SFilter e r -> NStream (B.filter (stream r) (\a -> expression [a] e /= 0))
    SFilterMap e1 e2 r -> NStream (B.filterMap (stream r) (\a ->
        if expression [a] e1 /= 0 then Just (VInt (expression [a] e2)) else Nothing))
    SMapTo v r -> NStream (B.mapTo (stream r) (valueOf v))
    SSnapshot e s c -> NStream (B.snapshot (stream s) (cell c) (\a b -> VInt (expression [a, b] e)))
    SGate s c -> NStream (B.gate (stream s) (B.mapCell (cell c) truthy))
    SOnce r -> NStream (created (B.once (stream r)))
    SMapList e1 e2 r -> NStream (B.map (stream r) (\a ->
        VList [ expression [a, VInt k] e2 | k <- [0 .. expression [a] e1 `mod` 4 - 1] ]))
    SPickStream e rs r -> NStream (B.map (stream r) (\a -> VStream (pick (expression [a] e) (map stream rs))))
    SPickCell e rs r -> NStream (B.map (stream r) (\a -> VCell (pick (expression [a] e) (map cell rs))))
    SNode r -> NStream (B.node (stream r))
    SShare r -> NStream (B.share (stream r))
    SMerge e l r -> NStream (B.merge (stream l) (stream r) (\x y -> coerce (contents' l) (expression [x, y] e)))
    SOrElse l r -> NStream (B.orElse (stream l) (stream r))
    SSplit r -> NStream (B.split (B.map (stream r) items))
    SDefer r -> NStream (B.defer (stream r))
    SScan e0 eo es r -> NStream (created (B.scan (stream r) (VInt (expression [] e0))
        (\a s -> (VInt (expression [a, s] eo), VInt (expression [a, s] es)))))
    SSteps r -> NStream (B.steps (cell r))
    SStepsWithCurrent r -> NStream (created (B.stepsWithCurrent (cell r)))
    SSwitch r -> NStream (B.switchStream (B.mapCell (cell r) asStream))
    SConstruct _ _ -> throw (OracleError "a construct reached interpretDef")
    CHold e r -> NCell (created (B.hold (stream r) (coerce (contents' r) (expression [] e))))
    CHoldStream r0 r -> NCell (created (B.hold (stream r) (VStream (stream r0))))
    CHoldCell r0 r -> NCell (created (B.hold (stream r) (VCell (cell r0))))
    CConstantStream r -> NCell (B.constant (VStream (stream r)))
    CConstantCell r -> NCell (B.constant (VCell (cell r)))
    CAccum e0 e r -> NCell (created (B.accumulate (stream r) (VInt (expression [] e0))
        (\a s -> VInt (expression [a, s] e))))
    CAccumMut e0 e r -> NCell (created (B.accumulateMut (stream r) (VInt (expression [] e0))
        (\a s -> VInt (expression [a, s] e))))
    CMapCell e r -> NCell (B.mapCell (cell r) (\a -> VInt (expression [a] e)))
    CToBool r -> NCell (B.mapCell (cell r) (VBool . truthy))
    CMapPickCell e rs r -> NCell (B.mapCell (cell r) (\a -> VCell (pick (expression [a] e) (map cell rs))))
    CLift e rs -> NCell (lift (map cell rs) (\values -> VInt (expression values e)))
    CSwitch r -> NCell (created (B.switchCell (B.mapCell (cell r) asCell)))
    CLoop _ -> throw (OracleError "a loop reached interpretDef")
    SLoop _ -> throw (OracleError "a loop reached interpretDef")
    Close _ _ -> NClose
  where
    created :: Reactive a -> a
    created r = run r (contextTime context)
    expression arguments e = evaluateE context arguments e
    stream = streamAt context
    cell = cellAt context
    contents' = contentsAt context

-- | One run of construct node i's body for the event a at its instant t: a
-- scope of its own, whose nodes are created at t, interpreted once with
-- the loops bound as given. Returns what the body emits, and the state its
-- loops' next round starts from.
runBody :: Context -> Int -> Ty -> Body -> Value -> T -> Binding -> (Value, State)
runBody outer i event (Body defs result) a t binding = (emitted, next)
  where
    scope = outer
        { contextLocal = Just IntMap.empty
        , contextLocalTypes = bodyTypes
        , contextTime = t
        , contextEvent = Just a
        }
    (nodes, next) = scopeRound scope defs binding
    here = scope { contextLocal = Just nodes }
    emitted = case result of
        RValue e -> VInt (evaluateE here [] e)
        RNode r -> case resolve here r of
            NStream s -> VStream s
            NCell c -> VCell c
            NClose -> throw (OracleError "a body emits a Close")
    bodyTypes = case checkScope (bodyScope typeScope event) defs of
        Right types -> IntMap.fromList (zip [0 ..] types)
        Left message -> throw (OracleError ("node " ++ show i ++ ": " ++ message))
    typeScope = TypeScope
        { typeInputs = contextInputTypes outer
        , typeTop = contextTopTypes outer
        , typeLocal = Nothing
        , typeEvent = Nothing
        , typePath = ""
        }

-- | Evaluates an expression: 64-bit wrapping arithmetic on 'Int', booleans
-- as 0 and 1.
evaluateE :: Context -> [Value] -> E -> Int
evaluateE context arguments = go
  where
    go e = case e of
        Arg -> argument 0
        Arg2 -> argument 1
        ArgN k -> argument k
        CArg -> maybe (throw (OracleError "CArg outside a body")) number (contextEvent context)
        Sample r -> case resolve context r of
            NCell c -> number (run (B.sample c) (contextTime context))
            _ -> throw (OracleError ("Sample " ++ show r ++ " is not a cell"))
        Lit n -> n
        Add a b -> go a + go b
        Sub a b -> go a - go b
        Mul a b -> go a * go b
        Mod a k -> go a `mod` k
        Max a b -> max (go a) (go b)
        Min a b -> min (go a) (go b)
        Eq a b -> fromEnum (go a == go b)
        Lt a b -> fromEnum (go a < go b)
        Not a -> fromEnum (go a == 0)
        If c a b -> if go c /= 0 then go a else go b
    argument k
        | k < length arguments = number (arguments !! k)
        | otherwise = throw (OracleError ("argument " ++ show k ++ " is not bound"))

-- | The node a reference names in a context.
resolve :: Context -> Ref -> Node
resolve context ref = case ref of
    N i -> find i (contextTop context)
    Local j -> maybe (throw (OracleError "Local at the top level")) (find j) (contextLocal context)
  where
    find i nodes = maybe (throw (OracleError (show ref ++ " does not exist"))) id (IntMap.lookup i nodes)

-- | The stream a reference names in a context.
streamAt :: Context -> Ref -> Stream Value
streamAt context r = case resolve context r of
    NStream s -> s
    _ -> throw (OracleError (show r ++ " is not a stream"))

-- | The cell a reference names in a context.
cellAt :: Context -> Ref -> Cell Value
cellAt context r = case resolve context r of
    NCell c -> c
    _ -> throw (OracleError (show r ++ " is not a cell"))

-- | What the stream a reference names carries, or what its cell holds.
contentsAt :: Context -> Ref -> Ty
contentsAt context r = case typeOf context r >>= contents of
    Just ty -> ty
    Nothing -> throw (OracleError (show r ++ " has no type"))

-- | The type of the node a reference names in a context.
typeOf :: Context -> Ref -> Maybe Ty
typeOf context (N i) = IntMap.lookup i (contextTopTypes context) >>= id
typeOf context (Local j) = IntMap.lookup j (contextLocalTypes context) >>= id

------------------------------------------------------------------------------
-- Helpers

-- | Whether a definition declares a loop: a cell loop (True) or a stream
-- loop (False), and its type.
loopDeclaration :: Def -> Maybe (Bool, Ty)
loopDeclaration (CLoop ty) = Just (True, ty)
loopDeclaration (SLoop ty) = Just (False, ty)
loopDeclaration _ = Nothing

-- | The name of a definition's constructor, for messages.
name :: Def -> String
name = takeWhile (/= ' ') . show

prefixed :: String -> Either String a -> Either String a
prefixed at = either (Left . (at ++)) Right

scalar :: Ty -> Bool
scalar ty = ty == TInt || ty == TBool

firstOrder :: Ty -> Bool
firstOrder ty = scalar ty || ty == TList

-- | What a stream carries or a cell holds.
contents :: Ty -> Maybe Ty
contents (TStreamOf ty) = Just ty
contents (TCellOf ty) = Just ty
contents _ = Nothing

valueType :: V -> Ty
valueType (I _) = TInt
valueType (B _) = TBool
valueType (L _) = TList

valueOf :: V -> Value
valueOf (I n) = VInt n
valueOf (B b) = VBool b
valueOf (L ns) = VList ns

zeroValue :: Ty -> Value
zeroValue TBool = VBool False
zeroValue TList = VList []
zeroValue _ = VInt 0

-- | An expression's result as a value of the given type: nonzero is true.
coerce :: Ty -> Int -> Value
coerce TBool n = VBool (n /= 0)
coerce TInt n = VInt n
coerce ty _ = throw (OracleError ("an expression cannot give " ++ show ty))

-- | A number or a boolean as an expression reads it.
number :: Value -> Int
number (VInt n) = n
number (VBool b) = fromEnum b
number other = throw (OracleError ("an expression read " ++ show other))

truthy :: Value -> Bool
truthy value = number value /= 0

items :: Value -> [Value]
items (VList ns) = map VInt ns
items other = throw (OracleError ("split read " ++ show other))

asStream :: Value -> Stream Value
asStream (VStream s) = s
asStream other = throw (OracleError ("expected a stream, got " ++ show other))

asCell :: Value -> Cell Value
asCell (VCell c) = c
asCell other = throw (OracleError ("expected a cell, got " ++ show other))

pick :: Int -> [a] -> a
pick n choices = choices !! (n `mod` length choices)

-- | lift at arities two to six: MapC then Apply, as Oracle.Derived defines it.
lift :: [Cell Value] -> ([Value] -> Value) -> Cell Value
lift cells f = case cells of
    [a, b] -> B.lift2 (a, b) (\x y -> f [x, y])
    [a, b, c] -> B.lift3 (a, b, c) (\x y z -> f [x, y, z])
    [a, b, c, d] -> B.lift4 (a, b, c, d) (\x y z w -> f [x, y, z, w])
    [a, b, c, d, e] -> B.lift5 (a, b, c, d, e) (\x y z w v -> f [x, y, z, w, v])
    [a, b, c, d, e, g] -> B.lift6 (a, b, c, d, e, g) (\x y z w v u -> f [x, y, z, w, v, u])
    _ -> throw (OracleError "lift takes two to six cells")
