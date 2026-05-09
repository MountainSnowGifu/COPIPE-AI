use crate::command::AiCommand;
use crate::executor::{now_timestamp, safe_append_log, LOG_DIR};
use crate::session::{page_diagnostic, CopilotSession};
use std::io::Write as _;

pub(super) async fn write_browser_log(root: &std::path::Path, reason: &str, session: &CopilotSession) {
    let log_dir = root.join(LOG_DIR);
    std::fs::create_dir_all(&log_dir).ok();
    let log_path = log_dir.join("browser_log");
    if log_path.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let header = format!("[{}] {reason}\n", now_timestamp());
        f.write_all(header.as_bytes()).ok();
        f.flush().ok();
        let diag = match tokio::time::timeout(
            std::time::Duration::from_secs(3),
            page_diagnostic(&session.page),
        ).await {
            Ok(diag) => diag,
            Err(_) => "{\"error\":\"page diagnostic timeout\"}".to_string(),
        };
        f.write_all(format!("{diag}\n---\n").as_bytes()).ok();
    }
}

pub(super) fn write_ai_log(
    root: &std::path::Path,
    turn: u32,
    commands: &[AiCommand],
    parse_errors: &[String],
) {
    let log_dir = root.join(LOG_DIR);
    std::fs::create_dir_all(&log_dir).ok();
    let mut entry = format!("=== ターン {} [{}] ===\n", turn + 1, now_timestamp());
    for cmd in commands {
        entry.push_str(&format!(
            "{}\n",
            serde_json::to_string(cmd).unwrap_or_default()
        ));
    }
    for e in parse_errors {
        entry.push_str(&format!("[ParseError] {e}\n"));
    }
    entry.push_str("---\n");
    safe_append_log(&log_dir.join("ai_log"), &entry);
}
