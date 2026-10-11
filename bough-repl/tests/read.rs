//! The reader: a line of text to one form, before any command is checked.

use bough_repl::read::{Form, read};
use bough_repl::ty::Literal;

fn symbol(s: &str) -> Form {
    Form::Symbol(s.to_string())
}

fn int(n: i64) -> Form {
    Form::Literal(Literal::Int(n))
}

fn string(s: &str) -> Form {
    Form::Literal(Literal::Str(s.to_string()))
}

fn list(forms: &[Form]) -> Form {
    Form::List(forms.to_vec())
}

fn vector(forms: &[Form]) -> Form {
    Form::Vector(forms.to_vec())
}

fn one(line: &str) -> Form {
    read(line)
        .expect("the line reads")
        .expect("the line holds a form")
}

#[test]
fn atoms_read_as_literals_or_symbols() {
    assert_eq!(one("-3"), int(-3));
    assert_eq!(one("true"), Form::Literal(Literal::Bool(true)));
    assert_eq!(one("false"), Form::Literal(Literal::Bool(false)));
    assert_eq!(one("\"Fizz\""), string("Fizz"));
    assert_eq!(one("\"\""), string(""));
    assert_eq!(one("fizz_buzz"), symbol("fizz_buzz"));
    // Not an i64, so a symbol, which a name check will refuse later.
    assert_eq!(one("9223372036854775808"), symbol("9223372036854775808"));
}

#[test]
fn lists_nest() {
    assert_eq!(
        one("(def splash (if (eq (mod x 7) 0) \"splash\" \"\"))"),
        list(&[
            symbol("def"),
            symbol("splash"),
            list(&[
                symbol("if"),
                list(&[
                    symbol("eq"),
                    list(&[symbol("mod"), symbol("x"), int(7)]),
                    int(0)
                ]),
                string("splash"),
                string(""),
            ]),
        ])
    );
    assert_eq!(one("()"), list(&[]));
    assert_eq!(one("  ( graph )  "), list(&[symbol("graph")]));
}

#[test]
fn a_string_holds_spaces_parentheses_and_semicolons() {
    assert_eq!(
        one("(set s \"Fizz (Buzz); x\")"),
        list(&[symbol("set"), symbol("s"), string("Fizz (Buzz); x")])
    );
}

#[test]
fn parentheses_and_quotes_end_a_word() {
    assert_eq!(
        one("(cat a\"b\")"),
        list(&[symbol("cat"), symbol("a"), string("b")])
    );
}

#[test]
fn whitespace_and_comments_read_as_nothing() {
    assert_eq!(read(""), Ok(None));
    assert_eq!(read("   "), Ok(None));
    assert_eq!(read("; a comment"), Ok(None));
    assert_eq!(
        read("(graph) ; after a form"),
        Ok(Some(list(&[symbol("graph")])))
    );
    // A comment runs to the end of the line, closing parentheses and all.
    assert_eq!(read("(watch x ; y)"), Err("a ( is not closed".to_string()));
}

#[test]
fn malformed_lines_are_errors() {
    assert_eq!(
        read("(def x (add a 1)"),
        Err("a ( is not closed".to_string())
    );
    assert_eq!(read("(watch x))"), Err("a ) closes nothing".to_string()));
    assert_eq!(read(")"), Err("a ) closes nothing".to_string()));
    assert_eq!(
        read("(set s \"hi)"),
        Err("a string is not closed".to_string())
    );
    assert_eq!(
        read("(graph) (graph)"),
        Err("more than one form on the line, after (graph)".to_string())
    );
}

#[test]
fn a_form_prints_back_as_it_reads() {
    let line = "(def out (if (eq (cat fizz buzz) \"\") (str x) (cat fizz buzz)))";
    assert_eq!(one(line).to_string(), line);
    assert_eq!(one("(  add   x\t3 )").to_string(), "(add x 3)");
}

#[test]
fn brackets_read_as_a_vector() {
    assert_eq!(
        one("(defgraph twice [a] (add a a))"),
        list(&[
            symbol("defgraph"),
            symbol("twice"),
            vector(&[symbol("a")]),
            list(&[symbol("add"), symbol("a"), symbol("a")]),
        ])
    );
    assert_eq!(one("[]"), vector(&[]));
    assert_eq!(
        one("[a[b]]"),
        vector(&[symbol("a"), vector(&[symbol("b")])])
    );
    assert_eq!(one("(f [a b] \"[c]\")").to_string(), "(f [a b] \"[c]\")");
}

#[test]
fn a_bracket_is_closed_by_its_own_kind() {
    assert_eq!(read("(f [a b)"), Err("a [ is closed by )".to_string()));
    assert_eq!(read("(f a]"), Err("a ( is closed by ]".to_string()));
    assert_eq!(read("[a"), Err("a [ is not closed".to_string()));
    assert_eq!(read("]"), Err("a ] closes nothing".to_string()));
    assert_eq!(read("[a]]"), Err("a ] closes nothing".to_string()));
}

#[test]
fn a_hyphen_is_part_of_a_symbol_and_a_leading_one_of_an_integer() {
    assert_eq!(one("word-for-mod-n"), symbol("word-for-mod-n"));
    assert_eq!(one("-3"), int(-3));
    assert_eq!(one("-x"), symbol("-x"));
}
