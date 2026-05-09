use crate::command::AiCommand;
use std::path::{Path, PathBuf};

pub struct ToolResult {
    pub label: String,
    pub output: String,
}

/// `root` 配下に収まる絶対パスを返す。ディレクトリ外・絶対パス・`..` は Err
fn resolve(root: &Path, raw: &str) -> Result<PathBuf, String> {
    let raw_path = Path::new(raw);

    // 絶対パスを即拒否
    if raw_path.is_absolute() {
        return Err(format!("アクセス拒否: 絶対パス '{raw}' は使えません"));
    }

    // .. コンポーネントを即拒否（join 後の canonicalize 頼みにしない）
    if raw_path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!("アクセス拒否: '..' を含むパス '{raw}' は使えません"));
    }

    let root_canonical = root
        .canonicalize()
        .map_err(|e| format!("root の解決に失敗: {e}"))?;

    let joined = root_canonical.join(raw_path);

    // 既存の最近祖先を canonicalize してシンボリックリンクを解決
    // （新規ファイルの親ディレクトリが symlink 経由で外に抜けるケースを防ぐ）
    let ancestor = {
        let mut cur = joined
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| joined.clone());
        loop {
            match cur.canonicalize() {
                Ok(c) => break c,
                Err(_) => match cur.parent() {
                    Some(p) => cur = p.to_path_buf(),
                    None => break root_canonical.clone(),
                },
            }
        }
    };

    if !ancestor.starts_with(&root_canonical) {
        return Err(format!("アクセス拒否: '{raw}' はプロジェクトルート外です"));
    }

    Ok(joined)
}

/// コマンドを実行し、(ツール結果, 表示メッセージ) を返す
pub fn execute(root: &Path, commands: &[AiCommand]) -> (Vec<ToolResult>, Vec<String>) {
    let mut results = Vec::new();
    let mut messages = Vec::new();

    for cmd in commands {
        match cmd {
            AiCommand::ReadFile { path } => {
                let output = match resolve(root, path) {
                    Err(e) => format!("ERROR: {e}"),
                    Ok(abs) => match std::fs::read_to_string(&abs) {
                        Ok(content) => format!("```\n{content}\n```"),
                        Err(e) => format!("ERROR: {e}"),
                    },
                };
                results.push(ToolResult {
                    label: format!("ReadFile({path})"),
                    output,
                });
            }
            AiCommand::File { path, content } => {
                let output = match resolve(root, path) {
                    Err(e) => format!("ERROR: {e}"),
                    Ok(abs) => {
                        if let Some(parent) = abs.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        match std::fs::write(&abs, content) {
                            Ok(_) => "OK".to_string(),
                            Err(e) => format!("ERROR: {e}"),
                        }
                    }
                };
                results.push(ToolResult {
                    label: format!("WriteFile({path})"),
                    output,
                });
            }
            AiCommand::Mkdir { path } => {
                let output = match resolve(root, path) {
                    Err(e) => format!("ERROR: {e}"),
                    Ok(abs) => match std::fs::create_dir_all(&abs) {
                        Ok(_) => "OK".to_string(),
                        Err(e) => format!("ERROR: {e}"),
                    },
                };
                results.push(ToolResult {
                    label: format!("Mkdir({path})"),
                    output,
                });
            }
            AiCommand::DeleteFile { path } => {
                let output = match resolve(root, path) {
                    Err(e) => format!("ERROR: {e}"),
                    Ok(abs) => match std::fs::remove_file(&abs) {
                        Ok(_) => "OK".to_string(),
                        Err(e) => format!("ERROR: {e}"),
                    },
                };
                results.push(ToolResult {
                    label: format!("DeleteFile({path})"),
                    output,
                });
            }
            AiCommand::DeleteFolder { path } => {
                let output = match resolve(root, path) {
                    Err(e) => format!("ERROR: {e}"),
                    Ok(abs) => {
                        // root 自身の削除を明示的に禁止
                        let root_canonical = root
                            .canonicalize()
                            .unwrap_or_else(|_| root.to_path_buf());
                        if abs == root_canonical {
                            "ERROR: アクセス拒否: プロジェクトルート自身は削除できません"
                                .to_string()
                        } else {
                            match std::fs::remove_dir_all(&abs) {
                                Ok(_) => "OK".to_string(),
                                Err(e) => format!("ERROR: {e}"),
                            }
                        }
                    }
                };
                results.push(ToolResult {
                    label: format!("DeleteFolder({path})"),
                    output,
                });
            }
            AiCommand::Txt { content } => {
                messages.push(content.clone());
            }
            AiCommand::Bot { message, content } => {
                let msg = message.as_deref().or(content.as_deref()).unwrap_or("");
                if !msg.is_empty() {
                    messages.push(msg.to_string());
                }
            }
            AiCommand::Cmd { name, .. } => {
                results.push(ToolResult {
                    label: format!("Cmd({name})"),
                    output: "ERROR: Cmd は未実装です。代わりにファイル操作ツールを使ってください。".to_string(),
                });
            }
            AiCommand::Patch { path, .. } => {
                results.push(ToolResult {
                    label: format!("Patch({path})"),
                    output: "ERROR: Patch は未実装です。file コマンドでファイル全体を書き直してください。".to_string(),
                });
            }
            AiCommand::ReadLog { filename } => {
                results.push(ToolResult {
                    label: format!("ReadLog({filename})"),
                    output: "ERROR: ReadLog は未実装です。read_file を使ってください。".to_string(),
                });
            }
            AiCommand::Error { message, content } => {
                let msg = message.as_deref().or(content.as_deref()).unwrap_or("(詳細なし)");
                results.push(ToolResult {
                    label: "Error".to_string(),
                    output: format!("ERROR: AI がエラーを報告しました: {msg}"),
                });
            }
        }
    }

    (results, messages)
}

/// ツール結果をCopilotへ返すプロンプトに整形する
pub fn format_tool_results(results: &[ToolResult]) -> String {
    let mut parts = vec!["[ツール実行結果]".to_string()];
    for r in results {
        parts.push(format!("## {}\n{}", r.label, r.output));
    }
    parts.join("\n\n")
}
