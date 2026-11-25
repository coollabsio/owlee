//! What the log and trace searches share: the visible sources, the time range,
//! and a WHERE clause with bound values. No user text goes into the SQL string.

use std::collections::HashMap;

use owlee_platform::TimestampMillis;

use super::sources;
use crate::error::AppError;
use crate::state::AppState;
use crate::workspaces::{Member, ProjectFilter};

/// The live sources the member can see under the project filter, id to name.
pub(super) async fn visible_sources(
    state: &AppState,
    member: &Member,
    project: &ProjectFilter,
) -> Result<HashMap<String, String>, AppError> {
    let project = project.resolve(&state.db, member.workspace_id).await?;
    Ok(sources::list(&state.db, member.workspace_id, project)
        .await?
        .into_iter()
        .map(|source| (source.id, source.name))
        .collect())
}

/// `from` and `to` in Unix milliseconds. `to` defaults to now and `from` to `default_ms` before it.
pub(super) fn time_range(
    from: Option<i64>,
    to: Option<i64>,
    default_ms: i64,
    max_ms: i64,
) -> Result<(i64, i64), AppError> {
    let to = to.unwrap_or_else(|| TimestampMillis::now().as_millis());
    let from = from.unwrap_or(to - default_ms);
    if from > to {
        return Err(AppError::field("to", "The end must be after the start."));
    }
    if to - from > max_ms {
        let days = max_ms / 86_400_000;
        return Err(AppError::field(
            "to",
            format!("Use a time range of at most {days} days."),
        ));
    }
    Ok((from, to))
}

pub(super) enum Bind {
    Text(String),
    Int(i64),
}

#[derive(Default)]
pub(super) struct Filter {
    pub sql: Vec<String>,
    binds: Vec<Bind>,
}

impl Filter {
    pub fn bind(&mut self, value: Bind) -> String {
        self.binds.push(value);
        format!("${}", self.binds.len())
    }

    pub fn text(&mut self, value: &str) -> String {
        self.bind(Bind::Text(value.to_owned()))
    }

    pub fn int(&mut self, value: i64) -> String {
        self.bind(Bind::Int(value))
    }

    /// `column IN ($1, $2)` over the visible source ids. The caller returns early when there are none.
    pub fn sources(&mut self, column: &str, sources: &HashMap<String, String>) {
        let ids: Vec<String> = sources.keys().map(|id| self.text(id)).collect();
        self.sql.push(format!("{column} IN ({})", ids.join(", ")));
    }

    pub fn query<'q>(
        &self,
        sql: &'q str,
    ) -> sqlx::query::Query<'q, sqlx::Any, sqlx::any::AnyArguments<'q>> {
        let mut query = sqlx::query(sql);
        for bind in &self.binds {
            query = match bind {
                Bind::Text(text) => query.bind(text.clone()),
                Bind::Int(number) => query.bind(*number),
            };
        }
        query
    }

    pub fn clause(&self) -> String {
        self.sql.join(" AND ")
    }
}

/// A `LIKE` pattern that matches `text` anywhere, for `ESCAPE '\'`.
pub(super) fn contains_pattern(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len() + 2);
    escaped.push('%');
    for character in text.chars() {
        if matches!(character, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped.push('%');
    escaped
}
