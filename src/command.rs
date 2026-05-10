use serde::{Deserialize, Serialize};

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

fn default_context_lines() -> usize { 2 }

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct EditPair {
    pub old_string: String,
    pub new_string: String,
}
fn default_exit_action() -> String { "merge".to_string() }

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
    // 配列として試みる
    if let Ok(cmds) = serde_json::from_str::<Vec<AiCommand>>(json) {
        return Ok(cmds);
    }
    // 単体オブジェクトとして試みる
    let cmd = serde_json::from_str::<AiCommand>(json)?;
    Ok(vec![cmd])
}
