//! The search language of the logs page, parsed into a typed filter. The
//! server turns the filter into SQL with bound values.
//!
//! | Input | Meaning |
//! |---|---|
//! | `timeout`, `"connection reset"` | the body contains the text, any case |
//! | `service:api`, `env:`, `host:`, `trace:` | the column equals the value |
//! | `level:error`, `level:>=warn` | the level, or the level and above |
//! | `http.route:/checkout` | an attribute equals the value |
//! | `-env:dev` | not |
//!
//! Terms are joined with AND.

use crate::logs::Level;

/// The most terms one query may have, so one search cannot build a huge statement.
pub const MAX_TERMS: usize = 20;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub terms: Vec<Term>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Term {
    pub negated: bool,
    pub kind: TermKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TermKind {
    /// Body text, matched without regard to case.
    Text(String),
    Field(Field, String),
    Level(Level),
    LevelAtLeast(Level),
    Attribute {
        key: String,
        value: String,
    },
}

/// The text columns a `key:value` term can name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Service,
    Environment,
    Host,
    Trace,
}

impl Field {
    #[must_use]
    pub const fn column(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::Environment => "environment",
            Self::Host => "host",
            Self::Trace => "trace_id",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct QueryError {
    /// The term as the user typed it.
    pub token: String,
    /// Names the term, so the search box can show it as is.
    pub message: String,
}

impl QueryError {
    fn new(token: &str, message: impl std::fmt::Display) -> Self {
        Self {
            token: token.to_owned(),
            message: format!("{message} in `{token}`."),
        }
    }
}

pub fn parse_query(input: &str) -> Result<Query, QueryError> {
    let tokens = tokenize(input)?;
    if tokens.len() > MAX_TERMS {
        return Err(QueryError {
            token: tokens[MAX_TERMS].to_owned(),
            message: format!("Use at most {MAX_TERMS} search terms."),
        });
    }
    let terms = tokens
        .into_iter()
        .map(parse_term)
        .collect::<Result<_, _>>()?;
    Ok(Query { terms })
}

/// Splits on whitespace outside double quotes.
fn tokenize(input: &str) -> Result<Vec<&str>, QueryError> {
    let mut tokens = Vec::new();
    let mut start = None;
    let mut quoted = false;
    for (index, character) in input.char_indices() {
        if character == '"' {
            quoted = !quoted;
            start.get_or_insert(index);
        } else if character.is_whitespace() && !quoted {
            if let Some(from) = start.take() {
                tokens.push(&input[from..index]);
            }
        } else {
            start.get_or_insert(index);
        }
    }
    if let Some(from) = start {
        if quoted {
            return Err(QueryError::new(&input[from..], "Close the quote"));
        }
        tokens.push(&input[from..]);
    }
    Ok(tokens)
}

fn parse_term(token: &str) -> Result<Term, QueryError> {
    let (negated, rest) = match token.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, token),
    };
    if rest.is_empty() {
        return Err(QueryError::new(token, "Add a term after `-`"));
    }
    let kind = match rest.split_once(':') {
        Some((key, value)) if !rest.starts_with('"') && is_key(key) => {
            let value = unquote(token, value)?;
            if value.is_empty() {
                return Err(QueryError::new(token, "Add a value after `:`"));
            }
            field_term(token, key, value)?
        }
        _ => {
            let text = unquote(token, rest)?;
            if text.is_empty() {
                return Err(QueryError::new(token, "Add text between the quotes"));
            }
            TermKind::Text(text.to_owned())
        }
    };
    Ok(Term { negated, kind })
}

fn field_term(token: &str, key: &str, value: &str) -> Result<TermKind, QueryError> {
    let field = match key {
        "service" => Field::Service,
        "env" => Field::Environment,
        "host" => Field::Host,
        "trace" => {
            let id = value.to_ascii_lowercase();
            if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(QueryError::new(
                    token,
                    "A trace id is 32 hexadecimal characters",
                ));
            }
            return Ok(TermKind::Field(Field::Trace, id));
        }
        "level" => {
            let (at_least, name) = match value.strip_prefix(">=") {
                Some(name) => (true, name),
                None => (false, value),
            };
            let level = Level::parse(&name.to_ascii_lowercase()).ok_or_else(|| {
                QueryError::new(
                    token,
                    "Use a level of trace, debug, info, warn, error, or fatal, optionally after `>=`",
                )
            })?;
            return Ok(if at_least {
                TermKind::LevelAtLeast(level)
            } else {
                TermKind::Level(level)
            });
        }
        _ => {
            return Ok(TermKind::Attribute {
                key: key.to_owned(),
                value: value.to_owned(),
            });
        }
    };
    Ok(TermKind::Field(field, value.to_owned()))
}

/// Attribute keys such as `http.route` or `user_agent.original`.
fn is_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= crate::MAX_KEY_CHARS
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
}

fn unquote<'a>(token: &str, text: &'a str) -> Result<&'a str, QueryError> {
    if !text.contains('"') {
        return Ok(text);
    }
    match text
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
    {
        Some(inner) if !inner.contains('"') => Ok(inner),
        _ => Err(QueryError::new(
            token,
            "Put quotes around the whole text or value",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(negated: bool, kind: TermKind) -> Term {
        Term { negated, kind }
    }

    fn text(value: &str) -> TermKind {
        TermKind::Text(value.to_owned())
    }

    #[test]
    fn parses_every_term_kind() {
        let parsed = parse_query(
            r#"timeout "connection reset" service:api level:>=warn http.route:/checkout -env:dev host:"web 1" trace:5B8EFFF798038103D269B633813FC60C level:Error -"health check""#,
        )
        .unwrap();
        assert_eq!(
            parsed.terms,
            vec![
                term(false, text("timeout")),
                term(false, text("connection reset")),
                term(false, TermKind::Field(Field::Service, "api".into())),
                term(false, TermKind::LevelAtLeast(Level::Warn)),
                term(
                    false,
                    TermKind::Attribute {
                        key: "http.route".into(),
                        value: "/checkout".into()
                    }
                ),
                term(true, TermKind::Field(Field::Environment, "dev".into())),
                term(false, TermKind::Field(Field::Host, "web 1".into())),
                term(
                    false,
                    TermKind::Field(Field::Trace, "5b8efff798038103d269b633813fc60c".into())
                ),
                term(false, TermKind::Level(Level::Error)),
                term(true, text("health check")),
            ]
        );
    }

    #[test]
    fn empty_and_plain_input() {
        assert_eq!(parse_query("").unwrap(), Query::default());
        assert_eq!(parse_query("   ").unwrap(), Query::default());
        assert_eq!(
            parse_query(r#""a:b" 100%_done"#).unwrap().terms,
            vec![term(false, text("a:b")), term(false, text("100%_done"))]
        );
        assert_eq!(
            parse_query("ratio:0.5").unwrap().terms,
            vec![term(
                false,
                TermKind::Attribute {
                    key: "ratio".into(),
                    value: "0.5".into()
                }
            )]
        );
    }

    #[test]
    fn bad_input_names_the_term() {
        let cases = [
            (
                "level:loud",
                "level:loud",
                "Use a level of trace, debug, info, warn, error, or fatal, optionally after `>=` in `level:loud`.",
            ),
            (
                "ok level:>warn",
                "level:>warn",
                "Use a level of trace, debug, info, warn, error, or fatal, optionally after `>=` in `level:>warn`.",
            ),
            (
                "service:",
                "service:",
                "Add a value after `:` in `service:`.",
            ),
            ("a - b", "-", "Add a term after `-` in `-`."),
            (r#"x "open"#, r#""open"#, r#"Close the quote in `"open`."#),
            (r#""""#, r#""""#, r#"Add text between the quotes in `""`."#),
            (
                "trace:abc",
                "trace:abc",
                "A trace id is 32 hexadecimal characters in `trace:abc`.",
            ),
            (
                r#"service:a"b""#,
                r#"service:a"b""#,
                r#"Put quotes around the whole text or value in `service:a"b"`."#,
            ),
        ];
        for (input, token, message) in cases {
            assert_eq!(
                parse_query(input),
                Err(QueryError {
                    token: token.into(),
                    message: message.into()
                }),
                "{input}"
            );
        }
        let many = vec!["x"; MAX_TERMS + 1].join(" ");
        assert_eq!(
            parse_query(&many).unwrap_err().message,
            "Use at most 20 search terms."
        );
    }
}
