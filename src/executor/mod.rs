pub mod checkpoints;
mod context;
pub(crate) mod diff;
pub mod errors;
pub mod hooks;
pub mod pre_hooks;
pub mod safety;
pub(crate) mod tools;

pub use checkpoints::CheckpointManager;
pub use context::ToolContext;

use crate::command::AiCommand;
use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const LOG_DIR: &str = ".copipe_logs";
pub const ALLOWED_LOGS: &[&str] = &["cmd_log", "ai_log", "browser_log", "todo"];

pub fn init_todo_log(root: &Path) -> std::io::Result<()> {
    tools::todo_write::init_empty(root)
}

/// ログファイルへの安全な追記（O_NOFOLLOW で TOCTOU を防ぐ）
pub fn safe_append_log(path: &Path, content: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        #[cfg(target_os = "linux")]
        const O_NOFOLLOW: i32 = 0o400000;
        #[cfg(target_os = "macos")]
        const O_NOFOLLOW: i32 = 0x100;
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        const O_NOFOLLOW: i32 = 0;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .custom_flags(O_NOFOLLOW)
            .open(path)
        {
            let _ = f.write_all(content.as_bytes());
        }
    }
    #[cfg(not(unix))]
    {
        if path
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            return;
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = f.write_all(content.as_bytes());
        }
    }
}

pub fn now_timestamp() -> String {
    let offset_hours: i64 = std::env::var("TZ")
        .ok()
        .and_then(|tz| {
            if tz.contains("Tokyo") || tz.contains("JST") {
                Some(9)
            } else if tz == "UTC" || tz == "GMT" {
                Some(0)
            } else {
                None
            }
        })
        .unwrap_or(9);
    let utc_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let local_secs = (utc_secs + offset_hours * 3600) as u64;
    let (h, m, s) = (
        local_secs % 86400 / 3600,
        local_secs % 3600 / 60,
        local_secs % 60,
    );
    let (y, mo, d) = days_to_ymd(local_secs / 86400);
    let tz_label = if offset_hours == 9 { "JST" } else { "UTC" };
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02} {tz_label}")
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    let mut y = 1970u64;
    loop {
        let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
        let dy = if leap { 366 } else { 365 };
        if days < dy {
            break;
        }
        days -= dy;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let months = [
        31u64,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut mo = 1u64;
    for &dm in &months {
        if days < dm {
            break;
        }
        days -= dm;
        mo += 1;
    }
    (y, mo, days + 1)
}

pub struct ToolResult {
    pub label: String,
    pub output: String,
    /// 元コマンドの1始まりインデックス（0 = ParseError 等 unindexed）
    pub cmd_index: usize,
}

impl ToolResult {
    pub fn new(label: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            output: output.into(),
            cmd_index: 0,
        }
    }
}

// ─── 薄いルーター ─────────────────────────────────────────────────────────────

pub async fn execute(
    root: &Path,
    commands: &[AiCommand],
    read_files: &mut HashSet<PathBuf>,
    checkpoints: &mut CheckpointManager,
) -> (Vec<ToolResult>, Vec<String>) {
    let mut ctx = ToolContext::new(root, read_files, checkpoints);
    let mut results = Vec::new();
    let mut messages = Vec::new();
    let mut cmd_idx = 0usize; // コマンド配列内の1始まりインデックス

    for cmd in commands {
        cmd_idx += 1;

        let idx = cmd_idx;

        // PreToolUse が Block を返した場合のヘルパー
        let blocked = |label: String, msg: String| {
            let mut r = ToolResult::new(label, errors::blocked_by_hook(msg));
            r.cmd_index = idx;
            r
        };

        // PostToolUse hook を適用して cmd_index を設定するヘルパー
        let post = |name: &str, mut r: ToolResult| {
            r = hooks::run(name, r);
            r.cmd_index = idx;
            r
        };

        // PreToolUse → ツール実行 → PostToolUse のパイプライン
        macro_rules! dispatch {
            ($tool_name:literal, $label:expr, $exec:expr) => {{
                let result = match pre_hooks::run($tool_name, cmd) {
                    pre_hooks::PreHookOutcome::Block(msg) => blocked($label, msg),
                    pre_hooks::PreHookOutcome::Continue => post($tool_name, $exec),
                };
                results.push(result);
            }};
        }

        match cmd {
            AiCommand::ReadFile { path, offset_lines } => dispatch!(
                "read_file",
                format!("ReadFile({path})"),
                tools::read_file::handle(&mut ctx, path, *offset_lines)
            ),

            AiCommand::ListDir { path } => dispatch!(
                "list_dir",
                format!("ListDir({path})"),
                tools::list_dir::handle(&ctx, path)
            ),

            AiCommand::Grep {
                pattern,
                path,
                context_lines,
                file_glob,
            } => dispatch!(
                "grep",
                format!("Grep({pattern} in {path})"),
                tools::grep::handle(&ctx, pattern, path, *context_lines, file_glob)
            ),

            AiCommand::Glob { pattern } => dispatch!(
                "glob",
                format!("Glob({pattern})"),
                tools::glob::handle(&ctx, pattern)
            ),

            AiCommand::Edit {
                path,
                old_string,
                new_string,
            } => dispatch!(
                "edit",
                format!("Edit({path})"),
                tools::edit::handle(&mut ctx, path, old_string, new_string)
            ),

            AiCommand::AskUser { question, hint } => dispatch!(
                "ask_user",
                "AskUser".to_string(),
                tools::ask_user::handle(question, hint).await
            ),

            AiCommand::TodoWrite { todos } => dispatch!(
                "todo_write",
                "TodoWrite".to_string(),
                tools::todo_write::handle(&ctx, todos)
            ),

            AiCommand::MultiEdit { path, edits } => dispatch!(
                "multi_edit",
                format!("MultiEdit({path})"),
                tools::multi_edit::handle(&mut ctx, path, edits)
            ),

            AiCommand::WebFetch { url, selector } => dispatch!(
                "web_fetch",
                format!("WebFetch({url})"),
                tools::web_fetch::handle(url, selector).await
            ),

            AiCommand::EnterWorktree => dispatch!(
                "enter_worktree",
                "EnterWorktree".to_string(),
                tools::worktree::enter(ctx.root).await
            ),

            AiCommand::ExitWorktree {
                action,
                commit_message,
            } => dispatch!(
                "exit_worktree",
                "ExitWorktree".to_string(),
                tools::worktree::exit(ctx.root, action, commit_message).await
            ),

            AiCommand::File { path, content } => dispatch!(
                "write_file",
                format!("WriteFile({path})"),
                tools::write_file::handle(&mut ctx, path, content)
            ),

            AiCommand::Mkdir { path } => dispatch!(
                "mkdir",
                format!("Mkdir({path})"),
                tools::mkdir::handle(&ctx, path)
            ),

            AiCommand::DeleteFile { path } => dispatch!(
                "delete_file",
                format!("DeleteFile({path})"),
                tools::delete_file::handle(&mut ctx, path)
            ),

            AiCommand::DeleteFolder { path } => {
                let mut r = ToolResult::new(
                    format!("DeleteFolder({path})"),
                    "ERROR: delete_folder は無効です。delete_file を使って個別に削除してください。",
                );
                r.cmd_index = idx;
                results.push(r);
            }

            AiCommand::Patch { path, diff } => dispatch!(
                "patch",
                format!("Patch({path})"),
                tools::patch::handle(&mut ctx, path, diff)
            ),

            AiCommand::ReadLog {
                filename,
                offset_lines,
            } => dispatch!(
                "read_log",
                if *offset_lines > 0 {
                    format!("ReadLog({filename}@{offset_lines})")
                } else {
                    format!("ReadLog({filename})")
                },
                tools::read_log::handle(&ctx, filename, *offset_lines)
            ),

            AiCommand::Cmd {
                name,
                cmd,
                workdir,
                timeout,
            } => dispatch!(
                "cmd",
                format!("Cmd({name})"),
                tools::cmd::handle(&ctx, name, cmd, workdir, *timeout).await
            ),

            AiCommand::Txt { content } => messages.push(content.clone()),

            AiCommand::Bot { message, content } => {
                let msg = message.as_deref().or(content.as_deref()).unwrap_or("");
                if !msg.is_empty() {
                    messages.push(msg.to_string());
                }
            }

            AiCommand::Error { message, content } => {
                let mut r = ToolResult::new(
                    "Error",
                    format!(
                        "ERROR: AI がエラーを報告しました: {}",
                        message
                            .as_deref()
                            .or(content.as_deref())
                            .unwrap_or("(詳細なし)")
                    ),
                );
                r.cmd_index = idx;
                results.push(r);
            }
        }
    }

    (results, messages)
}

pub fn format_tool_results(results: &[ToolResult]) -> String {
    const MAX_TOTAL_CHARS: usize = 8_000;
    let mut parts = vec!["[ツール実行結果]".to_string()];
    let mut used = parts[0].len();
    let total = results.len();
    for (i, r) in results.iter().enumerate() {
        // コマンドと結果の対応を明示（tool_use_id パターン）
        let header = if r.cmd_index > 0 {
            format!("[#{} → {}]", r.cmd_index, r.label)
        } else {
            format!("[{}]", r.label)
        };
        let entry = format!("{}\n{}", header, r.output);
        if used + entry.len() > MAX_TOTAL_CHARS {
            let remaining_budget = MAX_TOTAL_CHARS.saturating_sub(used + 2);
            if remaining_budget >= 300 {
                parts.push(truncate_tool_entry(&entry, remaining_budget));
                if i + 1 < total {
                    parts.push(format!(
                        "[残り {} 件の結果を省略（合計文字数制限）。次のターンで続きを確認してください]",
                        total - i - 1
                    ));
                }
            } else {
                parts.push(format!(
                    "[残り {} 件の結果を省略（合計文字数制限）。次のターンで続きを確認してください]",
                    total - i
                ));
            }
            break;
        }
        used += entry.len() + 2;
        parts.push(entry);
    }
    parts.join("\n\n")
}

fn truncate_tool_entry(entry: &str, max_chars: usize) -> String {
    let note = "\n[出力が長すぎるため一部を省略しました]";
    let continuation_hint = entry
        .rfind("\n[残り ")
        .and_then(|idx| entry[idx..].find("offset_lines").map(|_| &entry[idx..]));
    let read_file_hint = continuation_hint
        .is_none()
        .then(|| read_file_continuation_hint_for_truncated_entry(entry, max_chars))
        .flatten();

    let tail = continuation_hint
        .or(read_file_hint.as_deref())
        .unwrap_or("");
    let tail_chars = tail.chars().count();
    let note_chars = note.chars().count();
    let reserve = tail_chars + note_chars;

    if max_chars <= reserve + 20 {
        return entry.chars().take(max_chars).collect();
    }

    let keep = max_chars - reserve;
    let mut head: String = entry.chars().take(keep).collect();
    if let Some(last_newline) = head.rfind('\n') {
        head.truncate(last_newline + 1);
    }

    if tail.is_empty() {
        format!("{head}{note}")
    } else {
        format!("{head}{note}{tail}")
    }
}

fn read_file_continuation_hint_for_truncated_entry(
    entry: &str,
    max_chars: usize,
) -> Option<String> {
    let (path, base_offset) = read_file_label_from_entry(entry)?;
    let head: String = entry.chars().take(max_chars).collect();
    let code_start = head.find("```\n")? + 4;
    let content_head = &head[code_start..];
    let shown_lines = content_head.lines().count();
    if shown_lines == 0 {
        return None;
    }
    let next_offset = base_offset + shown_lines;
    Some(format!(
        "\n[続きは {{\"type\":\"read_file\",\"path\":\"{path}\",\"offset_lines\":{next_offset}}} で取得]"
    ))
}

fn read_file_label_from_entry(entry: &str) -> Option<(String, usize)> {
    let label_start = entry.find("ReadFile(")? + "ReadFile(".len();
    let label_end = entry[label_start..].find(')')? + label_start;
    let inner = &entry[label_start..label_end];
    let (path, offset) = match inner.rsplit_once('@') {
        Some((path, offset)) if offset.chars().all(|c| c.is_ascii_digit()) => {
            (path, offset.parse().ok()?)
        }
        _ => (inner, 0),
    };
    Some((path.to_string(), offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_single_result_keeps_read_file_continuation_hint() {
        let mut output = "x\n".repeat(10_000); // 20,000 chars -> 8,000 制限を超える
        output.push_str(
            "\n[残り 10 行。続きは {\"type\":\"read_file\",\"path\":\"src/agent/runner.rs\",\"offset_lines\":300} で取得]",
        );
        let result = ToolResult::new("ReadFile(src/agent/runner.rs)", output);

        let formatted = format_tool_results(&[result]);

        assert!(formatted.chars().count() <= 8_000);
        assert!(formatted.contains("\"offset_lines\":300"));
        assert!(formatted.contains("一部を省略"));
        assert!(!formatted.contains("残り 1 件の結果を省略"));
    }

    #[test]
    fn truncated_read_file_entry_adds_continuation_hint() {
        let output = format!(
            "```\n{}\n```",
            (0..2000)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let result = ToolResult::new("ReadFile(src/main.rs)", output);

        let formatted = format_tool_results(&[result]);

        assert!(formatted.contains("一部を省略"));
        assert!(formatted.contains("\"type\":\"read_file\""));
        assert!(formatted.contains("\"path\":\"src/main.rs\""));
        assert!(formatted.contains("\"offset_lines\":"));
    }

    #[test]
    fn truncated_offset_read_file_entry_preserves_base_offset() {
        let output = format!(
            "```\n{}\n```",
            (0..2000)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let result = ToolResult::new("ReadFile(src/main.rs@150)", output);

        let formatted = format_tool_results(&[result]);

        let offset = formatted
            .split("\"offset_lines\":")
            .nth(1)
            .and_then(|s| s.split('}').next())
            .and_then(|s| s.parse::<usize>().ok())
            .expect("offset_lines should be present");
        assert!(offset > 150);
    }
}
