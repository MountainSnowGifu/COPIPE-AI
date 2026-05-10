use crate::command::AiCommand;
use crate::executor::{LOG_DIR, now_timestamp, safe_append_log};
use crate::session::{CopilotSession, page_diagnostic};
use std::io::Write as _;

pub(super) async fn write_browser_log(
    root: &std::path::Path,
    reason: &str,
    session: &CopilotSession,
) {
    let log_dir = root.join(LOG_DIR);
    if std::fs::create_dir_all(&log_dir).is_err() {
        return;
    }
    let log_path = log_dir.join("browser_log");
    if log_path
        .symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        return;
    }
    let mut f = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(f) => f,
        Err(_) => return,
    };

    let header = format!("[{}] {reason}\n", now_timestamp());
    let _ = f.write_all(header.as_bytes());
    let _ = f.flush();

    if is_routine_browser_event(reason) {
        let _ = f.write_all(b"---\n");
        return;
    }

    let diag = match tokio::time::timeout(
        std::time::Duration::from_secs(3),
        page_diagnostic(&session.page),
    )
    .await
    {
        Ok(diag) => diag,
        Err(_) => "{\"error\":\"page diagnostic timeout\"}".to_string(),
    };
    let _ = f.write_all(format!("{diag}\n---\n").as_bytes());
}

fn is_routine_browser_event(reason: &str) -> bool {
    reason.starts_with("before send_raw") || reason == "after send_raw: ok"
}

pub(super) fn write_ai_log(
    root: &std::path::Path,
    turn: u32,
    commands: &[AiCommand],
    parse_errors: &[String],
) {
    let log_dir = root.join(LOG_DIR);
    if std::fs::create_dir_all(&log_dir).is_err() {
        return;
    }

    let mut entry = format!("=== ターン {} [{}] ===\n", turn + 1, now_timestamp());
    for cmd in commands {
        let line = serde_json::to_string(cmd)
            .unwrap_or_else(|_| "{\"error\":\"serialize failed\"}".to_string());
        entry.push_str(&line);
        entry.push('\n');
    }
    for e in parse_errors {
        entry.push_str(&format!("[ParseError] {e}\n"));
    }
    entry.push_str("---\n");

    safe_append_log(&log_dir.join("ai_log"), &entry);
}
