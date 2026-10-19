-- | The oracle process. It reads one Oracle.Program.Program per line on
-- standard input, in the syntax of its derived Read instance, and writes one
-- answer per line on standard output:
--
-- > OK <json>    the observations, as Oracle.Interpret.render writes them
-- > ERR <text>   the program is malformed, or evaluating it failed
-- > TIMEOUT      the program took longer than five seconds
--
-- The process stays up across programs and exits at the end of its input.
-- Build it with -threaded, so the time limit interrupts pure code, and link
-- it with -with-rtsopts=-M1g, so a program that eats the heap answers
-- ERR heap and the process lives on.
module Main (main) where

import Control.Monad (unless)
import System.IO
  ( hFlush, hSetBinaryMode, hSetBuffering, isEOF, stdin, stdout, BufferMode (..) )
import System.Timeout (timeout)
import Oracle.Interpret (answer)

-- | The time one program may take, in microseconds.
limit :: Int
limit = 5000000

main :: IO ()
main = do
    hSetBinaryMode stdin True
    hSetBinaryMode stdout True
    hSetBuffering stdout LineBuffering
    loop
  where
    loop = do
        end <- isEOF
        unless end $ do
            line <- getLine
            reply <- maybe "TIMEOUT" id <$> timeout limit (answer line)
            putStrLn reply
            hFlush stdout
            loop
