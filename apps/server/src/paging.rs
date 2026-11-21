//! Keyset paging. A list orders by `(key, id)` and hands back the last row's
//! pair as an opaque cursor, so rows inserted while a reader pages never shift
//! or repeat the rows that follow. Cursors are client input: they are decoded
//! here and only ever reach SQL as bound values.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use crate::error::AppError;

/// The sort value of the last row a page returned.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Key {
    Int(i64),
    Text(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cursor {
    #[serde(rename = "k")]
    pub key: Key,
    pub id: String,
}

/// Ids and sort keys are short. Anything longer was not minted here.
const MAX_CURSOR_LEN: usize = 512;

impl Cursor {
    pub fn new(key: Key, id: impl Into<String>) -> Self {
        Self { key, id: id.into() }
    }

    pub fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(self).unwrap_or_default())
    }

    fn decode(raw: &str) -> Option<Self> {
        if raw.len() > MAX_CURSOR_LEN {
            return None;
        }
        let bytes = URL_SAFE_NO_PAD.decode(raw).ok()?;
        let cursor: Self = serde_json::from_slice(&bytes).ok()?;
        let id_ok = !cursor.id.is_empty()
            && cursor
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
        id_ok.then_some(cursor)
    }
}

fn bad_cursor() -> AppError {
    AppError::BadRequest("The cursor is not valid for this list".into())
}

/// `?cursor=&limit=` as sent by the client.
#[derive(Deserialize, Default)]
pub struct PageParams {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// A parsed page request. `limit` is within `1..=max`.
#[derive(Clone, Debug)]
pub struct Paging {
    pub after: Option<Cursor>,
    pub limit: i64,
}

impl Paging {
    /// The first page at the list's largest size.
    pub fn first(max: i64) -> Self {
        Self {
            after: None,
            limit: max,
        }
    }

    /// The cursor of a list sorted by a number. 400 for a cursor minted by a text-sorted list.
    pub fn int_after(&self) -> Result<Option<(i64, &str)>, AppError> {
        match &self.after {
            None => Ok(None),
            Some(Cursor {
                key: Key::Int(key),
                id,
            }) => Ok(Some((*key, id.as_str()))),
            Some(_) => Err(bad_cursor()),
        }
    }

    pub fn text_after(&self) -> Result<Option<(&str, &str)>, AppError> {
        match &self.after {
            None => Ok(None),
            Some(Cursor {
                key: Key::Text(key),
                id,
            }) => Ok(Some((key.as_str(), id.as_str()))),
            Some(_) => Err(bad_cursor()),
        }
    }

    /// Asks the database for one extra row to learn whether another page follows.
    pub fn fetch(&self) -> i64 {
        self.limit + 1
    }

    /// Drops the probe row, and names the cursor of the page that follows.
    pub fn finish<T>(&self, mut rows: Vec<T>, cursor: impl Fn(&T) -> Cursor) -> Paged<T> {
        let more = rows.len() as i64 > self.limit;
        rows.truncate(self.limit as usize);
        let next_cursor = if more {
            rows.last().map(|row| cursor(row).encode())
        } else {
            None
        };
        Paged {
            items: rows,
            next_cursor,
        }
    }
}

impl PageParams {
    pub fn parse(&self, max: i64) -> Result<Paging, AppError> {
        let after = match self.cursor.as_deref().filter(|raw| !raw.is_empty()) {
            Some(raw) => Some(Cursor::decode(raw).ok_or_else(bad_cursor)?),
            None => None,
        };
        Ok(Paging {
            after,
            limit: self.limit.unwrap_or(max).clamp(1, max),
        })
    }
}

#[derive(Debug, Serialize)]
pub struct Paged<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(cursor: &str, limit: Option<i64>) -> PageParams {
        PageParams {
            cursor: Some(cursor.to_owned()),
            limit,
        }
    }

    #[test]
    fn a_cursor_round_trips() {
        let cursor = Cursor::new(Key::Text("2026-11-21T10:00:00.000Z".into()), "abc-1");
        let paging = params(&cursor.encode(), None).parse(50).unwrap();
        assert_eq!(paging.after, Some(cursor));
        assert_eq!(paging.limit, 50);
        let cursor = Cursor::new(Key::Int(-7), "x");
        assert_eq!(
            params(&cursor.encode(), Some(3)).parse(50).unwrap().after,
            Some(cursor)
        );
    }

    #[test]
    fn limits_are_clamped() {
        assert_eq!(params("", Some(0)).parse(50).unwrap().limit, 1);
        assert_eq!(params("", Some(500)).parse(50).unwrap().limit, 50);
    }

    #[test]
    fn malformed_cursors_are_rejected() {
        let quote_id = URL_SAFE_NO_PAD.encode(br#"{"k":1,"id":"a' OR 1=1"}"#);
        let no_id = URL_SAFE_NO_PAD.encode(br#"{"k":1}"#);
        let long = "A".repeat(MAX_CURSOR_LEN + 1);
        for raw in [
            "%%%",
            "bm90IGpzb24",
            quote_id.as_str(),
            no_id.as_str(),
            long.as_str(),
        ] {
            assert!(
                matches!(params(raw, None).parse(50), Err(AppError::BadRequest(_))),
                "{raw}"
            );
        }
    }
}
