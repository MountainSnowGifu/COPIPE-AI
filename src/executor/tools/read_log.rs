use crate::executor::context::ToolContext;
use crate::executor::tools::todo_write;
use crate::executor::{ALLOWED_LOGS, LOG_DIR, ToolResult};

pub fn handle(ctx: &ToolContext<'_>, filename: &str) -> ToolResult {
    const MAX_LOG_BYTES: usize = 32 * 1024;
    let output = if !ALLOWED_LOGS.contains(&filename) {
        format!(
            "ERROR: 不正なログ名 '{filename}'。使用可能: {}",
            ALLOWED_LOGS.join(", ")
        )
    } else if filename == "todo" {
        // todo は JSON ではなく可読フォーマットで返す
        let todos = todo_write::load(ctx.root);
        if todos.is_empty() {
            "(タスクリストは空です)".to_string()
        } else {
            todo_write::format_todos(&todos)
        }
    } else {
        let log_path = ctx.root.join(LOG_DIR).join(filename);
        if log_path
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            crate::executor::errors::perm_denied(format!("'{filename}' はシンボリックリンクです"))
        } else {
            match std::fs::read(&log_path) {
                Ok(bytes) if bytes.is_empty() => "(ログは空です)".to_string(),
                Ok(bytes) => {
                    let start = bytes.len().saturating_sub(MAX_LOG_BYTES);
                    let slice = &bytes[start..];
                    let prefix = if start > 0 {
                        "[先頭部分省略]\n"
                    } else {
                        ""
                    };
                    format!("{prefix}{}", String::from_utf8_lossy(slice))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    "(ログファイルが存在しません)".to_string()
                }
                Err(e) => crate::executor::errors::tool_error(&e),
            }
        }
    }; // ← else if filename == "todo" の else ブランチを閉じる
    ToolResult::new(format!("ReadLog({filename})"), output)
}
