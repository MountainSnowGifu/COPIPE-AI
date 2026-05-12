use crate::command::{TodoItem, TodoStatus};
use crate::executor::context::ToolContext;
use crate::executor::{LOG_DIR, ToolResult};
use std::path::Path;

pub const TODO_FILE: &str = "todo.json";

pub fn init_empty(root: &Path) -> std::io::Result<()> {
    let log_dir = root.join(LOG_DIR);
    std::fs::create_dir_all(&log_dir)?;
    let todo_path = log_dir.join(TODO_FILE);

    if todo_path
        .symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "todo.json is a symlink",
        ));
    }

    let should_initialize = if todo_path.exists() {
        std::fs::read_to_string(&todo_path)
            .map(|s| s.trim().is_empty())
            .unwrap_or(false)
    } else {
        true
    };

    if should_initialize {
        std::fs::write(todo_path, "[]\n")?;
    }

    Ok(())
}

/// タスクリストを更新し、ターミナルに表示して永続化する
pub fn handle(ctx: &ToolContext<'_>, todos: &[TodoItem]) -> ToolResult {
    if todos.is_empty() {
        return ToolResult::new(
            "TodoWrite",
            "ERROR: todos が空です。少なくとも1件のタスクを指定してください。",
        );
    }

    // ステータス検証
    for item in todos {
        if item.id.is_empty() {
            return ToolResult::new("TodoWrite", "ERROR: id が空のタスクがあります。");
        }
        if item.content.is_empty() {
            return ToolResult::new(
                "TodoWrite",
                format!("ERROR: タスク '{}' の content が空です。", item.id),
            );
        }
    }

    let log_dir = ctx.root.join(LOG_DIR);
    std::fs::create_dir_all(&log_dir).ok();
    let todo_path = log_dir.join(TODO_FILE);

    // symlink チェック
    if todo_path
        .symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        return ToolResult::new(
            "TodoWrite",
            "ERROR: todo.json はシンボリックリンクです。書き込みを拒否しました。",
        );
    }

    // JSON として保存
    let json = match serde_json::to_string_pretty(todos) {
        Ok(s) => s,
        Err(e) => return ToolResult::new("TodoWrite", format!("ERROR: シリアライズ失敗: {e}")),
    };
    if std::fs::write(&todo_path, format!("{json}\n")).is_err() {
        return ToolResult::new("TodoWrite", "ERROR: todo.json への書き込みに失敗しました。");
    }

    // フォーマットして表示（ターミナル + ツール結果）
    let formatted = format_todos(todos);
    let guidance = todo_guidance(todos);
    println!("\n{formatted}");

    let output = if guidance.is_empty() {
        format!("OK\n\n{formatted}")
    } else {
        format!("OK\n\n{formatted}\n\n{guidance}")
    };
    ToolResult::new("TodoWrite", output)
}

/// todo リストをターミナル向けにフォーマットする
pub fn format_todos(todos: &[TodoItem]) -> String {
    let mut lines = vec!["── タスクリスト ──────────────────────────────".to_string()];
    for item in todos {
        let icon = match item.status {
            TodoStatus::Completed => "✓",
            TodoStatus::InProgress => "●",
            TodoStatus::Pending => "○",
        };
        let style = match item.status {
            TodoStatus::Completed => "\x1b[2m",     // dim
            TodoStatus::InProgress => "\x1b[1;36m", // cyan bold
            TodoStatus::Pending => "",
        };
        lines.push(format!(
            "  {style}{icon} [{}] {}\x1b[0m",
            item.id, item.content
        ));
    }
    let pending = todos
        .iter()
        .filter(|t| t.status == TodoStatus::Pending)
        .count();
    let in_prog = todos
        .iter()
        .filter(|t| t.status == TodoStatus::InProgress)
        .count();
    let completed = todos
        .iter()
        .filter(|t| t.status == TodoStatus::Completed)
        .count();
    lines.push(format!(
        "──────────────────────────────────────────────\n  完了:{completed}  進行中:{in_prog}  未着手:{pending}"
    ));
    lines.join("\n")
}

/// プロンプトやログ向けに ANSI エスケープなしで todo を整形する
pub fn format_todos_plain(todos: &[TodoItem]) -> String {
    if todos.is_empty() {
        return "(タスクリストは空です)".to_string();
    }

    let mut lines = Vec::new();
    for item in todos {
        let status = match item.status {
            TodoStatus::Completed => "completed",
            TodoStatus::InProgress => "in_progress",
            TodoStatus::Pending => "pending",
        };
        lines.push(format!("- [{}] {status}: {}", item.id, item.content));
    }

    let pending = todos
        .iter()
        .filter(|t| t.status == TodoStatus::Pending)
        .count();
    let in_prog = todos
        .iter()
        .filter(|t| t.status == TodoStatus::InProgress)
        .count();
    let completed = todos
        .iter()
        .filter(|t| t.status == TodoStatus::Completed)
        .count();
    lines.push(format!(
        "summary: completed={completed}, in_progress={in_prog}, pending={pending}"
    ));
    lines.join("\n")
}

pub fn unfinished(todos: &[TodoItem]) -> Vec<&TodoItem> {
    todos
        .iter()
        .filter(|t| t.status != TodoStatus::Completed)
        .collect()
}

fn todo_guidance(todos: &[TodoItem]) -> String {
    let unfinished = unfinished(todos);
    if unfinished.is_empty() {
        return String::new();
    }

    let in_progress = todos
        .iter()
        .filter(|t| t.status == TodoStatus::InProgress)
        .count();
    if in_progress == 0 {
        "[TODO guidance] 未完了タスクがあります。次に実行する1件を in_progress にしてから、そのタスクを実行してください。".to_string()
    } else if in_progress > 1 {
        "[TODO guidance] in_progress が複数あります。並列ではなく順番に進める場合は、現在実行する1件だけを in_progress にしてください。".to_string()
    } else {
        "[TODO guidance] in_progress のタスクを実行し、完了したら todo_write で completed に更新してください。".to_string()
    }
}

/// todo.json を読み込んで Vec<TodoItem> を返す（存在しない場合は空）
pub fn load(root: &Path) -> Vec<TodoItem> {
    let path = root.join(LOG_DIR).join(TODO_FILE);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

// ─── テスト ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_todos() -> Vec<TodoItem> {
        vec![
            TodoItem {
                id: "1".into(),
                content: "executor 分割".into(),
                status: TodoStatus::Completed,
            },
            TodoItem {
                id: "2".into(),
                content: "hooks 追加".into(),
                status: TodoStatus::InProgress,
            },
            TodoItem {
                id: "3".into(),
                content: "テスト追加".into(),
                status: TodoStatus::Pending,
            },
        ]
    }

    #[test]
    fn test_format_todos_contains_icons() {
        let todos = make_todos();
        let out = format_todos(&todos);
        assert!(out.contains('✓'));
        assert!(out.contains('●'));
        assert!(out.contains('○'));
        assert!(out.contains("完了:1"));
        assert!(out.contains("進行中:1"));
        assert!(out.contains("未着手:1"));
    }

    #[test]
    fn test_format_todos_plain_has_no_ansi() {
        let todos = make_todos();
        let out = format_todos_plain(&todos);
        assert!(out.contains("[1] completed: executor 分割"));
        assert!(out.contains("[2] in_progress: hooks 追加"));
        assert!(out.contains("summary: completed=1, in_progress=1, pending=1"));
        assert!(!out.contains("\x1b["));
    }

    #[test]
    fn test_todo_guidance_warns_when_no_in_progress() {
        let todos = vec![TodoItem {
            id: "1".into(),
            content: "調査".into(),
            status: TodoStatus::Pending,
        }];

        let guidance = todo_guidance(&todos);

        assert!(guidance.contains("in_progress"));
    }

    #[test]
    fn test_load_nonexistent_returns_empty() {
        let dir = std::env::temp_dir().join("copipe_todo_test_nonexistent");
        let result = load(&dir);
        assert!(result.is_empty());
    }

    #[test]
    fn test_init_empty_creates_todo_json() {
        let dir = tempfile::tempdir().unwrap();

        init_empty(dir.path()).unwrap();

        let todo_path = dir.path().join(LOG_DIR).join(TODO_FILE);
        assert_eq!(std::fs::read_to_string(todo_path).unwrap(), "[]\n");
    }

    #[test]
    fn test_init_empty_keeps_existing_todo_json() {
        let dir = tempfile::tempdir().unwrap();
        let log_dir = dir.path().join(LOG_DIR);
        std::fs::create_dir_all(&log_dir).unwrap();
        let todo_path = log_dir.join(TODO_FILE);
        std::fs::write(&todo_path, "[{\"id\":\"1\"}]\n").unwrap();

        init_empty(dir.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(todo_path).unwrap(),
            "[{\"id\":\"1\"}]\n"
        );
    }

    #[test]
    fn test_init_empty_repairs_zero_byte_todo_json() {
        let dir = tempfile::tempdir().unwrap();
        let log_dir = dir.path().join(LOG_DIR);
        std::fs::create_dir_all(&log_dir).unwrap();
        let todo_path = log_dir.join(TODO_FILE);
        std::fs::write(&todo_path, "").unwrap();

        init_empty(dir.path()).unwrap();

        assert_eq!(std::fs::read_to_string(todo_path).unwrap(), "[]\n");
    }
}
