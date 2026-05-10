use crate::command::{TodoItem, TodoStatus};
use crate::executor::context::ToolContext;
use crate::executor::{LOG_DIR, ToolResult};
use std::path::Path;

pub const TODO_FILE: &str = "todo.json";

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
    if std::fs::write(&todo_path, &json).is_err() {
        return ToolResult::new("TodoWrite", "ERROR: todo.json への書き込みに失敗しました。");
    }

    // フォーマットして表示（ターミナル + ツール結果）
    let formatted = format_todos(todos);
    println!("\n{formatted}");

    ToolResult::new("TodoWrite", format!("OK\n\n{formatted}"))
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
    fn test_load_nonexistent_returns_empty() {
        let dir = std::env::temp_dir().join("copipe_todo_test_nonexistent");
        let result = load(&dir);
        assert!(result.is_empty());
    }
}
