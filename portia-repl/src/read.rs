//! The reader: one line of text to one [`Form`], before anything checks
//! what the form means.
//!
//! A form is an atom, a parenthesized list of forms, or a bracketed vector
//! of them. An atom is a literal, or else a symbol: an integer such as
//! `-3`, `true` or `false`, a string in double quotes, which may hold
//! spaces and brackets but no escapes, and any other run of characters up
//! to whitespace, a bracket or a quote. A `;` outside a string starts a comment that
//! runs to the end of the line.

use std::fmt;

use crate::ty::Literal;

/// What the reader reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Form {
    /// An integer, a boolean or a string.
    Literal(Literal),
    /// Any other atom: a command, a function, or a binding's name.
    Symbol(String),
    /// A parenthesized list.
    List(Vec<Form>),
    /// A bracketed vector, such as a `defgraph`'s parameters.
    Vector(Vec<Form>),
}

impl fmt::Display for Form {
    /// Prints the form back as it reads, with single spaces.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Form::Literal(literal) => write!(f, "{literal}"),
            Form::Symbol(symbol) => f.write_str(symbol),
            Form::List(forms) => sequence(f, '(', forms, ')'),
            Form::Vector(forms) => sequence(f, '[', forms, ']'),
        }
    }
}

fn sequence(f: &mut fmt::Formatter<'_>, open: char, forms: &[Form], close: char) -> fmt::Result {
    write!(f, "{open}")?;
    for (i, form) in forms.iter().enumerate() {
        if i > 0 {
            f.write_str(" ")?;
        }
        write!(f, "{form}")?;
    }
    write!(f, "{close}")
}

/// Reads a line holding at most one form: `None` if it holds only
/// whitespace and comments.
pub fn read(line: &str) -> Result<Option<Form>, String> {
    let mut tokens = Tokens::new(line);
    let Some(first) = tokens.next()? else {
        return Ok(None);
    };
    let form = form(first, &mut tokens)?;
    match tokens.next()? {
        None => Ok(Some(form)),
        Some(Token::Close(close)) => Err(format!("a {close} closes nothing")),
        Some(_) => Err(format!("more than one form on the line, after {form}")),
    }
}

/// The form `token` begins, with the rest of a list read from `tokens`.
fn form(token: Token, tokens: &mut Tokens) -> Result<Form, String> {
    match token {
        Token::Open(open) => {
            let close = if open == '(' { ')' } else { ']' };
            let mut forms = Vec::new();
            loop {
                match tokens.next()? {
                    None => return Err(format!("a {open} is not closed")),
                    Some(Token::Close(c)) if c == close => break,
                    Some(Token::Close(c)) => return Err(format!("a {open} is closed by {c}")),
                    Some(token) => forms.push(form(token, tokens)?),
                }
            }
            Ok(if open == '(' {
                Form::List(forms)
            } else {
                Form::Vector(forms)
            })
        }
        Token::Close(close) => Err(format!("a {close} closes nothing")),
        Token::Str(text) => Ok(Form::Literal(Literal::Str(text))),
        Token::Word(word) => Ok(atom(word)),
    }
}

/// A word that isn't a string: a literal if it reads as one, else a symbol.
fn atom(word: &str) -> Form {
    if let Ok(n) = word.parse::<i64>() {
        return Form::Literal(Literal::Int(n));
    }
    match word {
        "true" => Form::Literal(Literal::Bool(true)),
        "false" => Form::Literal(Literal::Bool(false)),
        _ => Form::Symbol(word.to_string()),
    }
}

enum Token<'a> {
    /// `(` or `[`.
    Open(char),
    /// `)` or `]`.
    Close(char),
    Str(String),
    Word(&'a str),
}

struct Tokens<'a> {
    rest: &'a str,
}

impl<'a> Tokens<'a> {
    fn new(line: &'a str) -> Tokens<'a> {
        Tokens { rest: line }
    }

    fn next(&mut self) -> Result<Option<Token<'a>>, String> {
        self.rest = self.rest.trim_start();
        let Some(c) = self.rest.chars().next() else {
            return Ok(None);
        };
        let token = match c {
            ';' => {
                self.rest = "";
                return Ok(None);
            }
            '(' | '[' => {
                self.rest = &self.rest[1..];
                Token::Open(c)
            }
            ')' | ']' => {
                self.rest = &self.rest[1..];
                Token::Close(c)
            }
            '"' => {
                let Some(end) = self.rest[1..].find('"') else {
                    return Err("a string is not closed".to_string());
                };
                let text = self.rest[1..1 + end].to_string();
                self.rest = &self.rest[end + 2..];
                Token::Str(text)
            }
            _ => {
                let end = self
                    .rest
                    .find(|c: char| c.is_whitespace() || "()[];\"".contains(c))
                    .unwrap_or(self.rest.len());
                let (word, rest) = self.rest.split_at(end);
                self.rest = rest;
                Token::Word(word)
            }
        };
        Ok(Some(token))
    }
}
