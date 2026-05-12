use serde::{Deserialize, Serialize};
use serde_json::Value;

/// p.txt で定義された全コマンド型
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AiCommand {
    Cmd {
        name: String,
        cmd: Vec<String>,
        workdir: Option<String>,
        timeout: u64,
    },
    Txt {
        content: String,
    },
    File {
        path: String,
        content: String,
    },
    Mkdir {
        path: String,
    },
    DeleteFile {
        path: String,
    },
    DeleteFolder {
        path: String,
    },
    ReadLog {
        filename: String,
        #[serde(default)]
        offset_lines: usize,
    },
    ReadFile {
        path: String,
        #[serde(default)]
        offset_lines: usize,
    },
    ListDir {
        path: String,
    },
    Grep {
        pattern: String,
        path: String,
        #[serde(default = "default_context_lines")]
        context_lines: usize,
        #[serde(default)]
        file_glob: Option<String>,
    },
    Glob {
        pattern: String,
    },
    Edit {
        path: String,
        old_string: String,
        new_string: String,
    },
    AskUser {
        question: String,
        #[serde(default)]
        hint: Option<String>,
    },
    TodoWrite {
        todos: Vec<TodoItem>,
    },
    MultiEdit {
        path: String,
        edits: Vec<EditPair>,
    },
    WebFetch {
        url: String,
        #[serde(default)]
        selector: Option<String>, // CSS セレクター（省略時はテキスト全体）
    },
    EnterWorktree,
    ExitWorktree {
        /// "merge" = squash merge してメインブランチへ反映
        /// "discard" = 変更を破棄
        #[serde(default = "default_exit_action")]
        action: String,
        #[serde(default)]
        commit_message: Option<String>,
    },
    Patch {
        path: String,
        diff: String,
    },
    Bot {
        message: Option<String>,
        content: Option<String>,
    },
    Error {
        message: Option<String>,
        content: Option<String>,
    },
}

fn default_context_lines() -> usize {
    2
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct EditPair {
    pub old_string: String,
    pub new_string: String,
}
fn default_exit_action() -> String {
    "merge".to_string()
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: TodoStatus,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

/// JSON ブロックをパース（単体オブジェクト or 配列を両対応）
pub fn parse_commands(json: &str) -> anyhow::Result<Vec<AiCommand>> {
    let json = normalize_json_text(json);
    // 配列として試みる
    if let Ok(value) = serde_json::from_str::<Value>(&json) {
        let normalized = normalize_command_value(value);
        if let Ok(cmds) = serde_json::from_value::<Vec<AiCommand>>(normalized.clone()) {
            return Ok(cmds);
        }
        // 配列の各要素を個別にパース（一部が未知の type でも残りを救済）
        if let Value::Array(ref items) = normalized {
            let cmds: Vec<AiCommand> = items
                .iter()
                .filter_map(|item| serde_json::from_value::<AiCommand>(item.clone()).ok())
                .collect();
            if !cmds.is_empty() {
                return Ok(cmds);
            }
        }
        if let Ok(cmd) = serde_json::from_value::<AiCommand>(normalized) {
            return Ok(vec![cmd]);
        }
    }

    if let Ok(cmds) = serde_json::from_str::<Vec<AiCommand>>(&json) {
        return Ok(cmds);
    }
    // 単体オブジェクトとして試みる
    let cmd = serde_json::from_str::<AiCommand>(&json)?;
    Ok(vec![cmd])
}

fn normalize_json_text(json: &str) -> String {
    let trimmed = json.trim();
    let trimmed = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```JSON"))
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed)
        .trim();
    let trimmed = trimmed.strip_suffix("```").unwrap_or(trimmed).trim();

    if let Some(rest) = trimmed.strip_prefix("json ") {
        rest.trim_start().to_string()
    } else if let Some(rest) = trimmed.strip_prefix("json\n") {
        rest.trim_start().to_string()
    } else {
        trimmed.to_string()
    }
}

fn normalize_command_value(value: Value) -> Value {
    match value {
        Value::Array(items) => {
            Value::Array(items.into_iter().map(normalize_command_value).collect())
        }
        Value::Object(mut map) => {
            let kind = map.get("type").and_then(Value::as_str).map(str::to_string);
            if let Some(kind) = kind {
                if kind == "add" || kind == "write_file" {
                    map.insert("type".to_string(), Value::String("file".to_string()));
                } else if kind == "grep"
                    && map
                        .get("pattern")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .is_empty()
                    && map
                        .get("path")
                        .and_then(Value::as_str)
                        .is_some_and(looks_like_file_path)
                {
                    map.insert("type".to_string(), Value::String("read_file".to_string()));
                    map.insert(
                        "offset_lines".to_string(),
                        Value::Number(serde_json::Number::from(0)),
                    );
                } else if kind == "edit"
                    && map.contains_key("patch")
                    && !map.contains_key("old_string")
                {
                    map.insert("type".to_string(), Value::String("patch".to_string()));
                    if let Some(patch) = map.remove("patch") {
                        map.insert("diff".to_string(), patch);
                    }
                } else if kind == "edit" && !map.contains_key("old_string") {
                    // AI が edit: { before: "...", after: "..." } 形式で出力した場合に正規化
                    // また before / after / old / new をトップレベルに書いた場合にも対応
                    if let Some(Value::Object(sub)) = map.remove("edit") {
                        let before = sub
                            .get("before")
                            .or_else(|| sub.get("old"))
                            .or_else(|| sub.get("old_string"))
                            .cloned();
                        let after = sub
                            .get("after")
                            .or_else(|| sub.get("new"))
                            .or_else(|| sub.get("new_string"))
                            .cloned();
                        if let (Some(b), Some(a)) = (before, after) {
                            map.insert("old_string".to_string(), b);
                            map.insert("new_string".to_string(), a);
                        }
                    } else {
                        // トップレベルに before / after / old / new がある場合
                        let before = map
                            .remove("before")
                            .or_else(|| map.remove("old"));
                        let after = map
                            .remove("after")
                            .or_else(|| map.remove("new"));
                        if let (Some(b), Some(a)) = (before, after) {
                            map.insert("old_string".to_string(), b);
                            map.insert("new_string".to_string(), a);
                        }
                    }
                }
            }
            Value::Object(map)
        }
        other => other,
    }
}

fn looks_like_file_path(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|name| name.contains('.') && !name.ends_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_commands_accepts_add_alias() {
        let cmds = parse_commands(r#"{"type":"add","path":"a.txt","content":"hello"}"#).unwrap();
        assert!(
            matches!(cmds.as_slice(), [AiCommand::File { path, content }] if path == "a.txt" && content == "hello")
        );
    }

    #[test]
    fn parse_commands_accepts_edit_patch_shape() {
        let cmds =
            parse_commands(r#"{"type":"edit","path":"src/main.rs","patch":"@@\n-old\n+new\n"}"#)
                .unwrap();
        assert!(
            matches!(cmds.as_slice(), [AiCommand::Patch { path, diff }] if path == "src/main.rs" && diff.contains("+new"))
        );
    }

    #[test]
    fn parse_commands_strips_json_prefix() {
        let cmds = parse_commands("json [{\"type\":\"txt\",\"content\":\"ok\"}]").unwrap();
        assert!(matches!(cmds.as_slice(), [AiCommand::Txt { content }] if content == "ok"));
    }

    #[test]
    fn parse_commands_accepts_logged_multi_command_array() {
        let input = r#"[
  {"type":"read_file","path":"src/executor/mod.rs"},
  {"type":"edit","path":"src/executor/mod.rs","patch":"Change `safe_append_log` to return Result."},
  {"type":"add","path":"tests/debug_log_tests.rs","content":"use tempfile::tempdir;\n"}
]"#;

        let cmds = parse_commands(input).unwrap();

        assert_eq!(cmds.len(), 3);
        assert!(matches!(cmds[0], AiCommand::ReadFile { .. }));
        assert!(matches!(cmds[1], AiCommand::Patch { .. }));
        assert!(matches!(cmds[2], AiCommand::File { .. }));
    }

    #[test]
    fn parse_commands_turns_empty_grep_on_file_into_read_file() {
        let cmds = parse_commands(
            r#"{"type":"grep","pattern":"","path":"src/agent/debug_log.rs","context_lines":0}"#,
        )
        .unwrap();

        assert!(
            matches!(cmds.as_slice(), [AiCommand::ReadFile { path, offset_lines }] if path == "src/agent/debug_log.rs" && *offset_lines == 0)
        );
    }
}
