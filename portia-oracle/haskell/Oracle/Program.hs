-- | The program description the oracle answers. The Rust side prints a
-- 'Program' as one line in the syntax of the derived 'Read' instances below,
-- so the constructor names and field orders here are the protocol. Change
-- one only together with @bough-oracle/src/program.rs@.
--
-- A program is a list of definitions. Node @i@ is the @i@-th 'Def'. A
-- definition refers only to nodes defined before it; a loop is the one way
-- to use a node before its definition exists. Closures cannot cross a
-- process boundary, so every function is an expression 'E' that both sides
-- evaluate the same way: 64-bit wrapping arithmetic, 'Mod' with a positive
-- divisor only, booleans as 0 and 1, and anything nonzero is true.
module Oracle.Program
  ( Program (..)
  , Window (..)
  , Input (..)
  , Def (..)
  , Body (..)
  , Result (..)
  , Ref (..)
  , Ty (..)
  , E (..)
  , V (..)
  ) where

-- | A program: what to observe, its inputs, its nodes, which nodes to
-- answer, and the sends. The schedule has one entry per external
-- transaction k = 1, 2, ..., each a list of sends (input index, value) in
-- send order. Transaction k is at time @[k]@; the build is time @[0]@.
data Program = Program Window [Input] [Def] [Int] [[(Int, V)]]
  deriving (Eq, Show, Read)

-- | Which part of each observed node the answer carries.
data Window
  = -- | What the engine comparison uses: a cell's value after transaction
    -- zero and its children, @at (steps c) [1]@, and its steps from @[1]@;
    -- a stream's events from @[1]@.
    FromFirstTransaction
  | -- | @steps c@ and @occs s@ whole, for vectors whose events sit at @[0]@.
    Everything
  deriving (Eq, Show, Read)

-- | An input: the type of its events, and the function that combines two
-- sends in one transaction ('Arg' the earlier send, 'Arg2' the later). An
-- input without one may be sent at most once per transaction.
data Input = Input Ty (Maybe E)
  deriving (Eq, Show, Read)

-- | A value in a literal stream, in a send, or in 'SMapTo'.
data V = I Int | B Bool | L [Int]
  deriving (Eq, Show, Read)

-- | The type of an event or a cell's value. Tokens are values too, for the
-- higher-order operations.
data Ty = TInt | TBool | TList | TStreamOf Ty | TCellOf Ty
  deriving (Eq, Show, Read)

-- | A reference to a node.
data Ref
  = -- | Node i of the program's top level.
    N Int
  | -- | Node j of the innermost enclosing construct body.
    Local Int
  deriving (Eq, Show, Read)

-- | An expression over integers. Each 'Def' says what its arguments are.
data E
  = -- | The first argument: @ArgN 0@.
    Arg
  | -- | The second argument: @ArgN 1@.
    Arg2
  | -- | Argument i: in 'CLift', the value of cell i.
    ArgN Int
  | -- | Inside a construct body: the construct's event.
    CArg
  | -- | The value a cell had before the scope's instant: the construct's
    -- instant inside a body, the build instant @[0]@ at the top level.
    Sample Ref
  | Lit Int
  | Add E E
  | Sub E E
  | Mul E E
  | -- | Haskell's @mod@; the divisor is positive.
    Mod E Int
  | Max E E
  | Min E E
  | -- | 1 when equal, 0 otherwise.
    Eq E E
  | -- | 1 when the first is less, 0 otherwise.
    Lt E E
  | -- | 1 for 0, 0 for anything else.
    Not E
  | -- | The second when the first is nonzero, the third otherwise.
    If E E E
  deriving (Eq, Show, Read)

-- | One node. Adapters are separate definitions; the Rust side fuses a
-- linear run of them into the materializer that consumes it. Events are
-- integers unless a line says otherwise.
data Def
  = -- | The stream of input i.
    SInput Int
  | -- | @input_cell@: a hold over input i, initial E.
    CInput Int E
  | SNever Ty
  | -- | A cell of integers.
    CConstant E
  | -- | @MkStream@ of the given events, times increasing. The engine cannot
    -- build it; the vectors use it.
    SLit Ty [([Int], V)]
  | -- | 'Arg': the event.
    SMap E Ref
  | -- | Keeps the events for which E is nonzero.
    SFilter E Ref
  | -- | Keeps the events for which the first is nonzero, and emits the
    -- second.
    SFilterMap E E Ref
  | -- | Any event type.
    SMapTo V Ref
  | -- | A stream and an integer cell; 'Arg' the event, 'Arg2' the cell's
    -- value before the instant.
    SSnapshot E Ref Ref
  | -- | Any stream and a boolean cell.
    SGate Ref Ref
  | SOnce Ref
  | -- | An integer to a list: its length is the first `mod` 4, and element
    -- i is the second with 'Arg2' = i.
    SMapList E E Ref
  | -- | An integer to one of the listed streams, index E `mod` n.
    SPickStream E [Ref] Ref
  | -- | An integer to one of the listed cells, index E `mod` n.
    SPickCell E [Ref] Ref
  | -- | Identities in the semantics; linearity and fusion in Rust.
    SNode Ref
  | SShare Ref
  | -- | 'Arg' the left event, 'Arg2' the right.
    SMerge E Ref Ref
  | SOrElse Ref Ref
  | -- | A stream of lists.
    SSplit Ref
  | SDefer Ref
  | -- | The initial state, the output, and the new state; 'Arg' the event,
    -- 'Arg2' the state.
    SScan E E E Ref
  | SSteps Ref
  | SStepsWithCurrent Ref
  | -- | A cell of streams.
    SSwitch Ref
  | -- | Runs the body at each event's instant.
    SConstruct Body Ref
  | CHold E Ref
  | -- | An initial stream, and a stream of streams.
    CHoldStream Ref Ref
  | -- | An initial cell, and a stream of cells.
    CHoldCell Ref Ref
  | CConstantStream Ref
  | CConstantCell Ref
  | -- | The initial state, and the new state with 'Arg' the event and
    -- 'Arg2' the state.
    CAccum E E Ref
  | -- | The same denotation as 'CAccum'; Rust uses @accumulate_mut@.
    CAccumMut E E Ref
  | -- | An integer cell to an integer cell.
    CMapCell E Ref
  | -- | An integer cell to a boolean cell: nonzero is true.
    CToBool Ref
  | -- | An integer cell to a cell of the listed cells, index E `mod` n.
    CMapPickCell E [Ref] Ref
  | -- | Two to six integer cells; 'ArgN' i is cell i.
    CLift E [Ref]
  | -- | A cell of cells.
    CSwitch Ref
  | -- | A forward cell, closed later by 'Close'.
    CLoop Ty
  | -- | A forward stream, closed later by 'Close'.
    SLoop Ty
  | -- | Closes loop node i of the same scope with a definition. It defines
    -- no node that can be used.
    Close Int Ref
  deriving (Eq, Show, Read)

-- | A construct body: its local nodes, and what it emits. 'Ref's inside
-- may be 'N' (top level) or 'Local'.
data Body = Body [Def] Result
  deriving (Eq, Show, Read)

-- | What a construct body emits for each event.
data Result
  = -- | An integer.
    RValue E
  | -- | A token: a stream or a cell.
    RNode Ref
  deriving (Eq, Show, Read)
