// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Binary {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Expr {
    Int(i64),
    None,
    Name(String),
    Tuple(Vec<Expr>),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Binary(Binary, Box<Expr>, Box<Expr>),
    If(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Token {
    Int(i64),
    Name(String),
    Word(&'static str),
    Punct(&'static str),
}

const WORDS: [&str; 9] = [
    "and", "or", "not", "if", "then", "else", "true", "false", "none",
];
const PUNCTS: [&str; 15] = [
    "==", "!=", "<=", ">=", "<", ">", "(", ")", ",", "+", "-", "*", "/", "%", "!",
];

fn tokens(text: &str) -> Result<Vec<Token>, Box<dyn Error>> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if byte == b'\'' {
            let end = text[index + 1..]
                .find('\'')
                .ok_or_else(|| format!("unclosed quote in `{text}`"))?;
            out.push(Token::Name(text[index + 1..index + 1 + end].to_owned()));
            index += end + 2;
            continue;
        }
        if byte.is_ascii_digit() {
            let end = text[index..]
                .find(|c: char| !c.is_ascii_digit())
                .map_or(bytes.len(), |end| index + end);
            out.push(Token::Int(text[index..end].parse()?));
            index = end;
            continue;
        }
        if byte.is_ascii_alphabetic() || byte == b'_' {
            let end = text[index..]
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
                .map_or(bytes.len(), |end| index + end);
            let word = &text[index..end];
            out.push(
                WORDS
                    .iter()
                    .find(|known| **known == word)
                    .map_or_else(|| Token::Name(word.to_owned()), |known| Token::Word(known)),
            );
            index = end;
            continue;
        }
        let punct = PUNCTS
            .iter()
            .find(|punct| text[index..].starts_with(**punct))
            .ok_or_else(|| format!("unexpected `{}` in `{text}`", &text[index..=index]))?;
        out.push(Token::Punct(punct));
        index += punct.len();
    }
    Ok(out)
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
    text: String,
}

pub(crate) fn parse(text: &str) -> Result<Expr, Box<dyn Error>> {
    let mut parser = Parser {
        tokens: tokens(text)?,
        at: 0,
        text: text.to_owned(),
    };
    let expr = parser.expr()?;
    if parser.at != parser.tokens.len() {
        return Err(format!("unexpected trailing input in `{text}`").into());
    }
    Ok(expr)
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn eat(&mut self, token: &Token) -> bool {
        if self.peek() == Some(token) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, token: &Token) -> Result<(), Box<dyn Error>> {
        if self.eat(token) {
            Ok(())
        } else {
            Err(format!("expected {token:?} in `{}`", self.text).into())
        }
    }

    fn expr(&mut self) -> Result<Expr, Box<dyn Error>> {
        if self.eat(&Token::Word("if")) {
            let condition = self.expr()?;
            self.expect(&Token::Word("then"))?;
            let then = self.expr()?;
            let otherwise = if self.eat(&Token::Word("else")) {
                self.expr()?
            } else {
                Expr::None
            };
            return Ok(Expr::If(
                Box::new(condition),
                Box::new(then),
                Box::new(otherwise),
            ));
        }
        self.or()
    }

    fn or(&mut self) -> Result<Expr, Box<dyn Error>> {
        let mut left = self.and()?;
        while self.eat(&Token::Word("or")) {
            left = Expr::Binary(Binary::Or, Box::new(left), Box::new(self.and()?));
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Expr, Box<dyn Error>> {
        let mut left = self.not()?;
        while self.eat(&Token::Word("and")) {
            left = Expr::Binary(Binary::And, Box::new(left), Box::new(self.not()?));
        }
        Ok(left)
    }

    fn not(&mut self) -> Result<Expr, Box<dyn Error>> {
        if self.eat(&Token::Word("not")) || self.eat(&Token::Punct("!")) {
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        self.comparison()
    }

    fn comparison(&mut self) -> Result<Expr, Box<dyn Error>> {
        let left = self.sum()?;
        let op = match self.peek() {
            Some(Token::Punct("==")) => Binary::Eq,
            Some(Token::Punct("!=")) => Binary::Ne,
            Some(Token::Punct("<")) => Binary::Lt,
            Some(Token::Punct("<=")) => Binary::Le,
            Some(Token::Punct(">")) => Binary::Gt,
            Some(Token::Punct(">=")) => Binary::Ge,
            _ => return Ok(left),
        };
        self.at += 1;
        Ok(Expr::Binary(op, Box::new(left), Box::new(self.sum()?)))
    }

    fn sum(&mut self) -> Result<Expr, Box<dyn Error>> {
        let mut left = self.product()?;
        loop {
            let op = if self.eat(&Token::Punct("+")) {
                Binary::Add
            } else if self.eat(&Token::Punct("-")) {
                Binary::Sub
            } else {
                return Ok(left);
            };
            left = Expr::Binary(op, Box::new(left), Box::new(self.product()?));
        }
    }

    fn product(&mut self) -> Result<Expr, Box<dyn Error>> {
        let mut left = self.unary()?;
        loop {
            let op = if self.eat(&Token::Punct("*")) {
                Binary::Mul
            } else if self.eat(&Token::Punct("/")) {
                Binary::Div
            } else if self.eat(&Token::Punct("%")) {
                Binary::Rem
            } else {
                return Ok(left);
            };
            left = Expr::Binary(op, Box::new(left), Box::new(self.unary()?));
        }
    }

    fn unary(&mut self) -> Result<Expr, Box<dyn Error>> {
        if self.eat(&Token::Punct("-")) {
            return Ok(match self.unary()? {
                Expr::Int(value) => Expr::Int(-value),
                expr => Expr::Neg(Box::new(expr)),
            });
        }
        self.atom()
    }

    fn atom(&mut self) -> Result<Expr, Box<dyn Error>> {
        let token = self
            .peek()
            .cloned()
            .ok_or_else(|| format!("unexpected end of `{}`", self.text))?;
        self.at += 1;
        match token {
            Token::Int(value) => Ok(Expr::Int(value)),
            Token::Word("true") => Ok(Expr::Int(1)),
            Token::Word("false") => Ok(Expr::Int(0)),
            Token::Word("none") => Ok(Expr::None),
            Token::Name(name) => {
                if self.eat(&Token::Punct("(")) {
                    let mut args = vec![self.expr()?];
                    while self.eat(&Token::Punct(",")) {
                        args.push(self.expr()?);
                    }
                    self.expect(&Token::Punct(")"))?;
                    Ok(Expr::Call(name, args))
                } else {
                    Ok(Expr::Name(name))
                }
            }
            Token::Punct("(") => {
                let mut items = vec![self.expr()?];
                while self.eat(&Token::Punct(",")) {
                    items.push(self.expr()?);
                }
                self.expect(&Token::Punct(")"))?;
                Ok(if items.len() == 1 {
                    items.remove(0)
                } else {
                    Expr::Tuple(items)
                })
            }
            token => Err(format!("unexpected {token:?} in `{}`", self.text).into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(text: &str) -> Box<Expr> {
        Box::new(Expr::Name(text.to_owned()))
    }

    #[test]
    fn precedence_and_quoted_names_parse() {
        assert_eq!(
            parse("a + 2 * 'b c' >= 3 and not d").unwrap(),
            Expr::Binary(
                Binary::And,
                Box::new(Expr::Binary(
                    Binary::Ge,
                    Box::new(Expr::Binary(
                        Binary::Add,
                        name("a"),
                        Box::new(Expr::Binary(
                            Binary::Mul,
                            Box::new(Expr::Int(2)),
                            name("b c")
                        )),
                    )),
                    Box::new(Expr::Int(3)),
                )),
                Box::new(Expr::Not(name("d"))),
            )
        );
        assert_eq!(
            parse("if x.y then -1 else max(p, (q, r) < (s, t))").unwrap(),
            Expr::If(
                name("x.y"),
                Box::new(Expr::Int(-1)),
                Box::new(Expr::Call(
                    "max".into(),
                    vec![
                        Expr::Name("p".into()),
                        Expr::Binary(
                            Binary::Lt,
                            Box::new(Expr::Tuple(vec![
                                Expr::Name("q".into()),
                                Expr::Name("r".into())
                            ])),
                            Box::new(Expr::Tuple(vec![
                                Expr::Name("s".into()),
                                Expr::Name("t".into())
                            ])),
                        ),
                    ]
                )),
            )
        );
        assert!(parse("a +").is_err());
        assert!(parse("a b").is_err());
        assert!(parse("'open").is_err());
    }
}
