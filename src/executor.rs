use crate::command::AiCommand;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct ToolResult {
    pub label: String,
    pub output: String,
}

// ─── パス解決 ────────────────────────────────────────────────────────────────

/// `root` 配下に収まる絶対パスを返す。ディレクトリ外・絶対パス・`..` は Err
fn resolve(root: &Path, raw: &str) -> Result<PathBuf, String> {
    let raw_path = Path::new(raw);

    if raw_path.is_absolute() {
        return Err(format!("アクセス拒否: 絶対パス '{raw}' は使えません"));
    }
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

    // 既存パス: シンボリックリンクを含む最終パスを完全解決してチェック
    if joined.exists() {
        let canonical = joined
            .canonicalize()
            .map_err(|e| format!("パスの解決に失敗: {e}"))?;
        if !canonical.starts_with(&root_canonical) {
            return Err(format!(
                "アクセス拒否: '{raw}' はプロジェクトルート外を指しています（シンボリックリンク経由の可能性）"
            ));
        }
        return Ok(canonical);
    }

    // 新規パス: 既存の最近祖先を canonicalize してルート内か確認
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

// ─── コマンド安全チェック ─────────────────────────────────────────────────────

/// 実行を許可するコマンド名（allowlist 方式）
const ALLOWED_EXECUTABLES: &[&str] = &[
    // Rust toolchain
    "cargo", "rustc", "rustfmt",
    // バージョン管理
    "git",
    // ファイル閲覧・検索（書き込みなし）
    "cat", "head", "tail", "grep", "rg", "find", "ls", "wc", "diff", "file",
    // テキスト処理
    "sort", "uniq", "tr", "cut", "awk", "sed", "jq",
    // ビルド
    "make",
    // ファイル操作（プロジェクト内限定、引数チェックで保護）
    "cp", "mv", "touch",
    // 情報表示
    "echo", "printf", "date", "env",
];

/// (コマンド名, ブロックするサブコマンド/フラグ) — データ破壊や任意実行に繋がるもの
const BLOCKED_SUBCMDS: &[(&str, &[&str])] = &[
    ("git",  &["clean", "push", "force-push"]),
    ("find", &["-delete", "-exec", "-execdir"]),
];

fn check_cmd_safety(cmd: &[String]) -> Result<(), String> {
    let exe = cmd.first().ok_or_else(|| "cmd が空です".to_string())?;

    if exe.starts_with('/') {
        return Err(format!("アクセス拒否: 絶対パス '{exe}' での実行は禁止です"));
    }

    let basename = Path::new(exe)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(exe.as_str());

    // allowlist: 許可リストにないコマンドはすべて拒否
    if !ALLOWED_EXECUTABLES.contains(&basename) {
        return Err(format!(
            "アクセス拒否: '{basename}' は許可されていません。許可コマンド: {}",
            ALLOWED_EXECUTABLES.join(", ")
        ));
    }

    // 引数チェック: 絶対パス・`..` を拒否
    for arg in &cmd[1..] {
        if arg.starts_with('/') {
            return Err(format!("アクセス拒否: 引数 '{arg}' に絶対パスが含まれています"));
        }
        if arg.contains("..") {
            return Err(format!("アクセス拒否: 引数 '{arg}' に '..' が含まれています"));
        }
    }

    // コマンド固有の危険サブコマンド/フラグを拒否
    for (target, blocked) in BLOCKED_SUBCMDS {
        if basename == *target {
            for arg in &cmd[1..] {
                if blocked.contains(&arg.as_str()) {
                    return Err(format!(
                        "アクセス拒否: '{basename} {arg}' は危険なため禁止です"
                    ));
                }
            }
        }
    }

    Ok(())
}

// ─── executor ────────────────────────────────────────────────────────────────

/// コマンドを実行し、(ツール結果, 表示メッセージ) を返す
/// `read_files` はセッション内で読み込んだファイルを追跡する（ファイル上書きガード用）
pub async fn execute(
    root: &Path,
    commands: &[AiCommand],
    read_files: &mut HashSet<PathBuf>,
) -> (Vec<ToolResult>, Vec<String>) {
    let mut results = Vec::new();
    let mut messages = Vec::new();
    let mut read_file_done = false;

    for cmd in commands {
        match cmd {
            AiCommand::ReadFile { path } => {
                if read_file_done {
                    results.push(ToolResult {
                        label: format!("ReadFile({path})"),
                        output: "ERROR: read_file は1ターンに1ファイルのみ使えます。次のターンで残りのファイルを読んでください。".to_string(),
                    });
                    continue;
                }
                read_file_done = true;
                let output = match resolve(root, path) {
                    Err(e) => format!("ERROR: {e}"),
                    Ok(abs) => match std::fs::read_to_string(&abs) {
                        Ok(content) => {
                            read_files.insert(abs);
                            format!("```\n{content}\n```")
                        }
                        Err(e) => format!("ERROR: {e}"),
                    },
                };
                results.push(ToolResult {
                    label: format!("ReadFile({path})"),
                    output,
                });
            }
            AiCommand::ListDir { path } => {
                let output = match resolve(root, path) {
                    Err(e) => format!("ERROR: {e}"),
                    Ok(abs) => match std::fs::read_dir(&abs) {
                        Err(e) => format!("ERROR: {e}"),
                        Ok(entries) => {
                            let mut lines: Vec<String> = entries
                                .filter_map(|e| e.ok())
                                .map(|e| {
                                    let name = e.file_name().to_string_lossy().into_owned();
                                    if e.path().is_dir() {
                                        format!("{name}/")
                                    } else {
                                        name
                                    }
                                })
                                .collect();
                            lines.sort();
                            lines.join("\n")
                        }
                    },
                };
                results.push(ToolResult {
                    label: format!("ListDir({path})"),
                    output,
                });
            }
            AiCommand::File { path, content } => {
                let output = match resolve(root, path) {
                    Err(e) => format!("ERROR: {e}"),
                    Ok(abs) => {
                        // 既存ファイルは read_files に記録済みの場合のみ上書き可
                        if abs.exists() && !read_files.contains(&abs) {
                            format!(
                                "ERROR: '{path}' は未読です。先に read_file で内容を確認してから上書きしてください。"
                            )
                        } else {
                            if let Some(parent) = abs.parent() {
                                let _ = std::fs::create_dir_all(parent);
                            }
                            match std::fs::write(&abs, content) {
                                Ok(_) => "OK".to_string(),
                                Err(e) => format!("ERROR: {e}"),
                            }
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
                // 作成履歴の追跡なしに remove_dir_all は危険なため無効化
                results.push(ToolResult {
                    label: format!("DeleteFolder({path})"),
                    output: "ERROR: delete_folder は無効です。AI が作成したディレクトリの追跡が未実装のため誤削除防止のため禁止しています。delete_file を使って個別に削除してください。".to_string(),
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
            AiCommand::Cmd { name, cmd, workdir, timeout: timeout_secs } => {
                let output = if *timeout_secs == 0 {
                    "ERROR: timeout は必須です。1以上の秒数を指定して再生成してください。"
                        .to_string()
                } else if let Err(e) = check_cmd_safety(cmd) {
                    format!("ERROR: {e}")
                } else {
                    let workdir_path = match workdir {
                        Some(wd) => match resolve(root, wd) {
                            Err(e) => {
                                results.push(ToolResult {
                                    label: format!("Cmd({name})"),
                                    output: format!("ERROR: workdir の解決に失敗: {e}"),
                                });
                                continue;
                            }
                            Ok(abs) => abs,
                        },
                        None => root.to_path_buf(),
                    };

                    // kill_on_drop(true) でタイムアウト時に子プロセスを確実に kill
                    let child = tokio::process::Command::new(&cmd[0])
                        .args(&cmd[1..])
                        .current_dir(&workdir_path)
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .kill_on_drop(true)
                        .spawn();

                    match child {
                        Err(e) => format!("ERROR: コマンド起動失敗: {e}"),
                        Ok(child) => {
                            match tokio::time::timeout(
                                Duration::from_secs(*timeout_secs),
                                child.wait_with_output(),
                            )
                            .await
                            {
                                Err(_) => format!(
                                    "ERROR: タイムアウト ({}秒) - プロセスを強制終了しました",
                                    timeout_secs
                                ),
                                Ok(Err(e)) => format!("ERROR: コマンド実行失敗: {e}"),
                                Ok(Ok(out)) => {
                                    let stdout = String::from_utf8_lossy(&out.stdout);
                                    let stderr = String::from_utf8_lossy(&out.stderr);
                                    let code = out.status.code().unwrap_or(-1);
                                    let mut parts = vec![format!("exit: {code}")];
                                    if !stdout.is_empty() {
                                        parts.push(format!("stdout:\n{stdout}"));
                                    }
                                    if !stderr.is_empty() {
                                        parts.push(format!("stderr:\n{stderr}"));
                                    }
                                    parts.join("\n")
                                }
                            }
                        }
                    }
                };
                results.push(ToolResult {
                    label: format!("Cmd({name})"),
                    output,
                });
            }
            AiCommand::Patch { path, .. } => {
                results.push(ToolResult {
                    label: format!("Patch({path})"),
                    output: "ERROR: Patch は未実装です。read_file で読んだ後、file コマンドで全文を書き直してください。".to_string(),
                });
            }
            AiCommand::ReadLog { filename } => {
                results.push(ToolResult {
                    label: format!("ReadLog({filename})"),
                    output: "ERROR: ReadLog は未実装です。read_file を使ってください。"
                        .to_string(),
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
