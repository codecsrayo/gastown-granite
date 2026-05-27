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
