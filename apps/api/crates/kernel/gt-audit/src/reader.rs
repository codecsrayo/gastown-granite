use std::path::Path;

use gt_events::AppError;

use crate::record::EventRecord;

/// Lee todos los records del log en orden. Una línea jsonl vacía se ignora.
pub fn read_all(path: &Path) -> Result<Vec<EventRecord>, AppError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(AppError::Other(format!("read log: {e}"))),
    };
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str::<EventRecord>(l)
                .map_err(|e| AppError::Other(format!("decode record: {e}")))
        })
        .collect()
}

/// Últimos `n` records (tail). Útil para feeds / debugging.
pub fn tail(path: &Path, n: usize) -> Result<Vec<EventRecord>, AppError> {
    let all = read_all(path)?;
    let start = all.len().saturating_sub(n);
    Ok(all[start..].to_vec())
}

/// Records whose `ts` is strictly greater than `since` (RFC3339), capped at `limit` from the
/// tail end (most recent). When `since` is `None` returns the last `limit` records. Used by
/// `gt-web`'s `GET /api/feed?since=` historico (hq-fe-api-r.5) to seed the SSE consumer.
///
/// `ts` comparison is **string lexicographic** on the RFC3339 form; all writers in this
/// project emit timezone-`Z` records (`record::from_envelope` uses `Rfc3339`) so the lex
/// order matches chronological order. A malformed `since` is treated as "no filter" — the
/// caller decides whether to surface that as a 400; the reader stays infallible on its
/// query input so the gateway can short-circuit empty logs uniformly.
pub fn since(
    path: &Path,
    since: Option<&str>,
    limit: usize,
) -> Result<Vec<EventRecord>, AppError> {
    let all = read_all(path)?;
    let filtered: Vec<EventRecord> = match since {
        Some(s) if !s.is_empty() => all.into_iter().filter(|r| r.ts.as_str() > s).collect(),
        _ => all,
    };
    let start = filtered.len().saturating_sub(limit);
    Ok(filtered[start..].to_vec())
}
