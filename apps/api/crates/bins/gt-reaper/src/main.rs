//! `gt-reaper` — the standalone wisp reaper job (hq-t9vt / paso 9.C).
//!
//! Boots the single tokio runtime (per `docs/01-architecture.md`, runtimes live in bins),
//! connects to Dolt, ensures the `wisps` table exists, and runs one
//! scan → reap → purge cycle via [`gt_wisp::run`], printing a JSON summary to stdout.
//!
//! Idempotent and replay-safe: re-running compacts nothing new, and the cycle touches only
//! the `wisps` table, never the event log. The `reaper` skill and the Rust `gt reaper run`
//! command (paso 9.A) invoke this binary.
//!
//! Usage: `gt-reaper run [--dry-run] [--purge-age 7d] [--url mysql://root@127.0.0.1:3307/hq]`
//! Connection defaults to `$GT_DOLT_URL`, then `mysql://root@127.0.0.1:3307/hq`.

use std::process::ExitCode;

use time::{Duration, OffsetDateTime};

use gt_events::AppError;
use gt_store_dolt::DoltWisp;

const DEFAULT_URL: &str = "mysql://root@127.0.0.1:3307/hq";
const DEFAULT_PURGE_AGE: Duration = Duration::days(7);

struct Args {
    dry_run: bool,
    purge_age: Duration,
    url: String,
}

fn usage() -> &'static str {
    "usage: gt-reaper run [--dry-run] [--purge-age <dur>] [--url <dsn>]\n\
     \n\
     Runs one scan -> reap -> purge cycle over the wisps table and prints a JSON summary.\n\
     --purge-age accepts a duration like 24h, 7d, 336h (default 7d).\n\
     --url / $GT_DOLT_URL select the Dolt DSN (default mysql://root@127.0.0.1:3307/hq)."
}

/// Parse a duration like `90s`, `24h`, `7d`. Bare integers are treated as seconds.
fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num, unit) = match s.char_indices().find(|(_, c)| c.is_ascii_alphabetic()) {
        Some((i, _)) => (&s[..i], &s[i..]),
        None => (s, "s"),
    };
    let n: i64 = num
        .parse()
        .map_err(|_| format!("invalid duration number: {s}"))?;
    match unit {
        "s" => Ok(Duration::seconds(n)),
        "m" => Ok(Duration::minutes(n)),
        "h" => Ok(Duration::hours(n)),
        "d" => Ok(Duration::days(n)),
        other => Err(format!("invalid duration unit: {other}")),
    }
}

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut dry_run = false;
    let mut purge_age = DEFAULT_PURGE_AGE;
    let mut url = std::env::var("GT_DOLT_URL").unwrap_or_else(|_| DEFAULT_URL.to_string());
    let mut saw_run = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "run" => saw_run = true,
            "--dry-run" => dry_run = true,
            "--purge-age" => {
                let v = args.next().ok_or("--purge-age needs a value")?;
                purge_age = parse_duration(&v)?;
            }
            "--url" => {
                url = args.next().ok_or("--url needs a value")?;
            }
            "-h" | "--help" => return Err("help".to_string()),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }

    if !saw_run {
        return Err("missing subcommand: run".to_string());
    }
    Ok(Args {
        dry_run,
        purge_age,
        url,
    })
}

async fn reap(args: &Args) -> Result<gt_wisp::ReapSummary, AppError> {
    let repo = DoltWisp::connect(&args.url)?;
    repo.ensure_schema().await?;
    gt_wisp::run(&repo, OffsetDateTime::now_utc(), args.purge_age, args.dry_run).await
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            if msg == "help" {
                println!("{}", usage());
                return ExitCode::SUCCESS;
            }
            eprintln!("gt-reaper: {msg}\n\n{}", usage());
            return ExitCode::from(2);
        }
    };

    match reap(&args).await {
        Ok(summary) => {
            println!("{}", serde_json::to_string_pretty(&summary).unwrap());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("gt-reaper: {e}");
            ExitCode::FAILURE
        }
    }
}
