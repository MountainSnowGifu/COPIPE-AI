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
