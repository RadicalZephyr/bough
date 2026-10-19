//! The oracle's answers, parsed.
//!
//! One line per program:
//!
//! - `OK <json>`: a JSON array with one element per observed node, in the
//!   order the program lists them. A stream is `[0, [[t, v], …]]` and a cell
//!   is `[1, v0, [[t, v], …]]`, both cut to the program's
//!   [`Window`](crate::Window). A time `t` is an array of integers. A value
//!   `v` is an integer, a boolean as 0 or 1, or a list as an array of
//!   integers.
//! - `ERR <message>`: the program is malformed, or evaluating it failed, for
//!   example a loop that does not converge, or a heap overflow.
//! - `TIMEOUT`: the program took longer than five seconds.

use core::fmt;

use crate::program::Time;

/// The oracle's answer to one program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// `OK`: one observation per observed node, in the order the program
    /// lists them.
    Observed(Vec<Observation>),
    /// `ERR`: the program is malformed, or evaluating it failed.
    Error(String),
    /// `TIMEOUT`: the program took longer than five seconds.
    Timeout,
}

/// What the answer carries for one observed node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Observation {
    /// A stream's events in the window.
    Stream {
        /// The events, in time order.
        events: Vec<(Time, Datum)>,
    },
    /// A cell's value at the start of the window, and its steps in it.
    Cell {
        /// The value at the start of the window.
        initial: Datum,
        /// The steps, in time order.
        steps: Vec<(Time, Datum)>,
    },
}

/// A value in an answer. A boolean arrives as the integer 0 or 1; the
/// program says which nodes carry booleans.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Datum {
    /// An integer, or a boolean as 0 or 1.
    Integer(i64),
    /// A list of integers.
    List(Vec<i64>),
}

impl From<i64> for Datum {
    fn from(value: i64) -> Datum {
        Datum::Integer(value)
    }
}

impl From<bool> for Datum {
    fn from(value: bool) -> Datum {
        Datum::Integer(i64::from(value))
    }
}

impl From<Vec<i64>> for Datum {
    fn from(values: Vec<i64>) -> Datum {
        Datum::List(values)
    }
}

impl Observation {
    /// A stream's expected events, for tests.
    pub fn stream<T, D>(events: impl IntoIterator<Item = (T, D)>) -> Observation
    where
        T: Into<Time>,
        D: Into<Datum>,
    {
        Observation::Stream {
            events: events
                .into_iter()
                .map(|(time, value)| (time.into(), value.into()))
                .collect(),
        }
    }

    /// A cell's expected initial value and steps, for tests.
    pub fn cell<T, D>(
        initial: impl Into<Datum>,
        steps: impl IntoIterator<Item = (T, D)>,
    ) -> Observation
    where
        T: Into<Time>,
        D: Into<Datum>,
    {
        Observation::Cell {
            initial: initial.into(),
            steps: steps
                .into_iter()
                .map(|(time, value)| (time.into(), value.into()))
                .collect(),
        }
    }
}

/// An answer line that does not follow the protocol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MalformedAnswer {
    /// The line, cut to its first 200 characters.
    pub line: String,
    /// What is wrong with it.
    pub reason: String,
}

impl fmt::Display for MalformedAnswer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "the oracle answered `{}`: {}",
            self.line, self.reason
        )
    }
}

impl std::error::Error for MalformedAnswer {}

impl Answer {
    /// Parses one answer line, without its line ending.
    pub fn parse(line: &str) -> Result<Answer, MalformedAnswer> {
        let malformed = |reason: String| MalformedAnswer {
            line: line.chars().take(200).collect(),
            reason,
        };
        if line == "TIMEOUT" {
            Ok(Answer::Timeout)
        } else if line == "ERR" {
            Ok(Answer::Error(String::new()))
        } else if let Some(message) = line.strip_prefix("ERR ") {
            Ok(Answer::Error(message.to_owned()))
        } else if let Some(json) = line.strip_prefix("OK ") {
            let tree = Json::parse(json).map_err(malformed)?;
            observations(&tree).map(Answer::Observed).map_err(malformed)
        } else {
            Err(malformed(
                "an answer starts with OK, ERR or TIMEOUT".to_owned(),
            ))
        }
    }

    /// The observations of an `OK` answer; `None` for `ERR` and `TIMEOUT`.
    pub fn observations(&self) -> Option<&[Observation]> {
        match self {
            Answer::Observed(observations) => Some(observations),
            Answer::Error(_) | Answer::Timeout => None,
        }
    }
}

// ----- the JSON the answers use: integers and arrays -----

/// A JSON value of the kinds the answers use.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Json {
    Integer(i64),
    Array(Vec<Json>),
}

/// The deepest nesting an answer has is five; this leaves room and stops a
/// runaway line from exhausting the stack.
const DEPTH_LIMIT: usize = 32;

impl Json {
    /// Parses a whole text as one value.
    fn parse(text: &str) -> Result<Json, String> {
        let mut parser = Parser {
            bytes: text.as_bytes(),
            position: 0,
        };
        let value = parser.value(0)?;
        parser.skip_whitespace();
        if parser.position == parser.bytes.len() {
            Ok(value)
        } else {
            Err(format!("unexpected text at byte {}", parser.position))
        }
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl Parser<'_> {
    fn skip_whitespace(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.bytes.get(self.position) {
            self.position += 1;
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > DEPTH_LIMIT {
            return Err(format!("arrays nest deeper than {DEPTH_LIMIT}"));
        }
        self.skip_whitespace();
        match self.bytes.get(self.position) {
            Some(b'[') => {
                self.position += 1;
                let mut elements = Vec::new();
                self.skip_whitespace();
                if self.bytes.get(self.position) == Some(&b']') {
                    self.position += 1;
                    return Ok(Json::Array(elements));
                }
                loop {
                    elements.push(self.value(depth + 1)?);
                    self.skip_whitespace();
                    match self.bytes.get(self.position) {
                        Some(b',') => self.position += 1,
                        Some(b']') => {
                            self.position += 1;
                            return Ok(Json::Array(elements));
                        }
                        _ => return Err(format!("expected `,` or `]` at byte {}", self.position)),
                    }
                }
            }
            Some(b'-' | b'0'..=b'9') => {
                let start = self.position;
                if self.bytes[self.position] == b'-' {
                    self.position += 1;
                }
                while let Some(b'0'..=b'9') = self.bytes.get(self.position) {
                    self.position += 1;
                }
                // The slice is ASCII digits with an optional sign.
                let digits = core::str::from_utf8(&self.bytes[start..self.position])
                    .map_err(|error| error.to_string())?;
                digits
                    .parse::<i64>()
                    .map(Json::Integer)
                    .map_err(|error| format!("`{digits}` at byte {start}: {error}"))
            }
            Some(_) => Err(format!(
                "expected an integer or an array at byte {}",
                self.position
            )),
            None => Err("the text ends early".to_owned()),
        }
    }
}

/// The observations an `OK` answer's JSON describes.
fn observations(tree: &Json) -> Result<Vec<Observation>, String> {
    array(tree, "the answer")?.iter().map(observation).collect()
}

fn observation(tree: &Json) -> Result<Observation, String> {
    let elements = array(tree, "an observation")?;
    match elements {
        [Json::Integer(0), events] => Ok(Observation::Stream {
            events: timed(events)?,
        }),
        [Json::Integer(1), initial, steps] => Ok(Observation::Cell {
            initial: datum(initial)?,
            steps: timed(steps)?,
        }),
        _ => Err("an observation is [0, events] or [1, initial, steps]".to_owned()),
    }
}

fn timed(tree: &Json) -> Result<Vec<(Time, Datum)>, String> {
    array(tree, "a list of events")?
        .iter()
        .map(|pair| match array(pair, "an event")? {
            [time, value] => Ok((integers(time, "a time")?, datum(value)?)),
            _ => Err("an event is [time, value]".to_owned()),
        })
        .collect()
}

fn datum(tree: &Json) -> Result<Datum, String> {
    match tree {
        Json::Integer(value) => Ok(Datum::Integer(*value)),
        Json::Array(_) => integers(tree, "a list value").map(Datum::List),
    }
}

fn integers(tree: &Json, what: &str) -> Result<Vec<i64>, String> {
    array(tree, what)?
        .iter()
        .map(|element| match element {
            Json::Integer(value) => Ok(*value),
            Json::Array(_) => Err(format!("{what} holds integers only")),
        })
        .collect()
}

fn array<'a>(tree: &'a Json, what: &str) -> Result<&'a [Json], String> {
    match tree {
        Json::Array(elements) => Ok(elements),
        Json::Integer(_) => Err(format!("{what} is an array")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ok_answer_carries_streams_and_cells() {
        let answer =
            Answer::parse("OK [[0,[[[0],6],[[1,0],11]]],[1,-3,[[[2],[1,-2]]]],[0,[]]]").unwrap();
        assert_eq!(
            answer,
            Answer::Observed(vec![
                Observation::stream([(vec![0], 6), (vec![1, 0], 11)]),
                Observation::cell(-3, [(vec![2], vec![1, -2])]),
                Observation::Stream { events: vec![] },
            ])
        );
        assert_eq!(answer.observations().map(<[Observation]>::len), Some(3));
    }

    #[test]
    fn whitespace_and_extreme_integers_parse() {
        let answer =
            Answer::parse("OK [ [1 , -9223372036854775808 , [ [ [1] , 9223372036854775807 ] ] ] ]")
                .unwrap();
        assert_eq!(
            answer,
            Answer::Observed(vec![Observation::cell(i64::MIN, [(vec![1], i64::MAX)])])
        );
    }

    #[test]
    fn errors_and_timeouts_parse() {
        assert_eq!(
            Answer::parse(
                "ERR the loops did not converge in 202 rounds; still changing: node 1 at [1]"
            )
            .unwrap(),
            Answer::Error(
                "the loops did not converge in 202 rounds; still changing: node 1 at [1]"
                    .to_owned()
            )
        );
        assert_eq!(Answer::parse("ERR").unwrap(), Answer::Error(String::new()));
        assert_eq!(Answer::parse("TIMEOUT").unwrap(), Answer::Timeout);
        assert_eq!(Answer::parse("TIMEOUT").unwrap().observations(), None);
        assert_eq!(Answer::parse("OK []").unwrap(), Answer::Observed(vec![]));
    }

    #[test]
    fn malformed_answers_say_why() {
        for (line, reason) in [
            ("", "starts with OK"),
            ("OK", "starts with OK"),
            ("OK [", "ends early"),
            ("OK [[2,[]]]", "an observation is"),
            ("OK [[0,[[[1],1]]]] extra", "unexpected text"),
            ("OK [[0,[[1,1]]]]", "a time is an array"),
            ("OK [[1,[1,[2]],[]]]", "holds integers only"),
            ("OK [[0,[[[1],99999999999999999999]]]]", "too large"),
            ("OK [[0,{}]]", "expected an integer or an array"),
        ] {
            let error = Answer::parse(line).unwrap_err();
            assert!(error.reason.contains(reason), "{line:?}: {error}");
        }
        let deep = format!("OK {}{}", "[".repeat(100), "]".repeat(100));
        assert!(
            Answer::parse(&deep)
                .unwrap_err()
                .reason
                .contains("nest deeper")
        );
    }
}
