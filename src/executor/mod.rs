mod diff;
pub mod safety;

use diff::apply_unified_diff;
use safety::check_cmd_safety;

use crate::command::AiCommand;
use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const LOG_DIR: &str = ".copipe_logs";

const ALLOWED_LOGS: &[&str] = &["cmd_log", "ai_log", "ai_readonly", "browser_log"];

/// ログファイルへの安全な追記（symlink なら書き込まない）
pub fn safe_append_log(path: &Path, content: &str) {
    if path.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        return; // symlink は無視
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(content.as_bytes());
    }
}

pub fn now_timestamp() -> String {
    // TZ 環境変数があればオフセット参照、なければ JST(+9) をデフォルト
    let offset_hours: i64 = std::env::var("TZ")
        .ok()
        .and_then(|tz| {
            // "Asia/Tokyo" → +9, UTC → 0, etc. を簡易判定
            if tz.contains("Tokyo") || tz.contains("JST") { Some(9) }
            else if tz == "UTC" || tz == "GMT" { Some(0) }
            else { None }
        })
        .unwrap_or(9); // デフォルト JST

    let utc_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let local_secs = (utc_secs + offset_hours * 3600) as u64;
    let (h, m, s) = (local_secs % 86400 / 3600, local_secs % 3600 / 60, local_secs % 60);
    let days = local_secs / 86400;
    let (y, mo, d) = days_to_ymd(days);
    let tz_label = if offset_hours == 9 { "JST" } else { "UTC" };
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02} {tz_label}")
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    let mut y = 1970u64;
    loop {
        let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
        let dy = if leap { 366 } else { 365 };
        if days < dy { break; }
        days -= dy;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let months = [31u64, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut mo = 1u64;
    for &dm in &months {
        if days < dm { break; }
        days -= dm;
        mo += 1;
    }
    (y, mo, days + 1)
}

pub struct ToolResult {
    pub label: String,
    pub output: String,
}

// ─── パス解決 ────────────────────────────────────────────────────────────────

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

// ─── executor ────────────────────────────────────────────────────────────────

pub async fn execute(
    root: &Path,
    commands: &[AiCommand],
    read_files: &mut HashSet<PathBuf>,
) -> (Vec<ToolResult>, Vec<String>) {
    let mut results = Vec::new();
    let mut messages = Vec::new();
    // 1ターンあたりの read_file 合計文字数上限
    // 複数の小ファイルは1ターンで読める・大きいファイルは1ファイルでも制限に当たる
    const MAX_TURN_READ_CHARS: usize = 10_000;
    let mut turn_read_chars = 0usize;

    for cmd in commands {
        match cmd {
            AiCommand::ReadFile { path, offset_lines } => {
                if turn_read_chars >= MAX_TURN_READ_CHARS {
                    results.push(ToolResult {
                        label: format!("ReadFile({path})"),
                        output: format!(
                            "このターンの読み込みバジェット ({MAX_TURN_READ_CHARS} 文字) を超えました。次のターンで読んでください。"
                        ),
                    });
                    continue;
                }
                let output = match resolve(root, path) {
                    Err(e) => format!("ERROR: {e}"),
                    Ok(abs) => match std::fs::read_to_string(&abs) {
                        Err(_) if !abs.exists() => {
                            // ファイルが存在しない場合、親ディレクトリの内容を補足する
                            let hint = abs.parent()
                                .and_then(|p| std::fs::read_dir(p).ok())
                                .map(|entries| {
                                    let names: Vec<String> = entries
                                        .filter_map(|e| e.ok())
                                        .map(|e| e.file_name().to_string_lossy().to_string())
                                        .collect();
                                    format!(" 同ディレクトリの実在ファイル: {}", names.join(", "))
                                })
                                .unwrap_or_default();
                            format!("ERROR: ファイルが存在しません: '{path}'.{hint}")
                        }
                        Ok(content) => {
                            // ターン内残バジェットを考慮した上限（ファイル個別上限も兼ねる）
                            let budget = MAX_TURN_READ_CHARS.saturating_sub(turn_read_chars);
                            let total_lines = content.lines().count();
                            let sliced: String = if *offset_lines > 0 {
                                content.lines().skip(*offset_lines).collect::<Vec<_>>().join("\n")
                            } else {
                                content.clone()
                            };
                            let sliced_lines = total_lines.saturating_sub(*offset_lines);
                            let out = if sliced.chars().count() > budget {
                                // 部分読み込み: read_files に追加しない（上書き・削除を防ぐ）
                                let truncated: String = sliced.chars().take(budget).collect();
                                let shown_lines = truncated.lines().count();
                                let remaining = sliced_lines.saturating_sub(shown_lines);
                                let next_offset = offset_lines + shown_lines;
                                format!("```\n{truncated}\n```\n[残り {remaining} 行。続きは {{\"type\":\"read_file\",\"path\":\"{path}\",\"offset_lines\":{next_offset}}} で取得]")
                            } else {
                                // 全内容を読み切った場合のみ read_files に登録
                                read_files.insert(abs);
                                if *offset_lines > 0 {
                                    format!("```\n{sliced}\n```\n[{offset_lines} 行目以降を表示（全 {total_lines} 行）]")
                                } else {
                                    format!("```\n{sliced}\n```")
                                }
                            };
                            turn_read_chars += out.chars().count();
                            out
                        }
                        Err(e) => format!("ERROR: {e}"),
                    },
                };
                results.push(ToolResult {
                    label: if *offset_lines == 0 {
                        format!("ReadFile({path})")
                    } else {
                        format!("ReadFile({path}@{offset_lines})")
                    },
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
                        // dangling symlink（exists()=false だが symlink は存在）を拒否
                        if abs.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                            format!("ERROR: '{path}' はシンボリックリンクです。書き込みを拒否しました")
                        } else if abs.exists() && !read_files.contains(&abs) {
                            format!(
                                "ERROR: '{path}' は未読です。先に read_file で内容を確認してから上書きしてください。"
                            )
                        } else {
                            if let Some(parent) = abs.parent() {
                                let _ = std::fs::create_dir_all(parent);
                            }
                            match std::fs::write(&abs, content) {
                                Ok(_) => {
                                    read_files.insert(abs);
                                    "OK".to_string()
                                }
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
                    Ok(abs) => {
                        if abs.exists() && !read_files.contains(&abs) {
                            format!(
                                "ERROR: '{path}' は未読です。先に read_file で内容を確認してから削除してください。"
                            )
                        } else {
                            match std::fs::remove_file(&abs) {
                                Ok(_) => {
                                    read_files.remove(&abs);
                                    "OK".to_string()
                                }
                                Err(e) => format!("ERROR: {e}"),
                            }
                        }
                    }
                };
                results.push(ToolResult {
                    label: format!("DeleteFile({path})"),
                    output,
                });
            }
            AiCommand::DeleteFolder { path } => {
                results.push(ToolResult {
                    label: format!("DeleteFolder({path})"),
                    output: "ERROR: delete_folder は無効です。delete_file を使って個別に削除してください。".to_string(),
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
            AiCommand::Cmd {
                name,
                cmd,
                workdir,
                timeout: timeout_secs,
            } => {
                println!("[{name}] {} 実行中...", cmd.join(" "));
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
                                    let prefix = if code == 0 { "" } else { "ERROR: " };
                                    let mut parts = vec![format!("{prefix}exit: {code}")];
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

                // cmd_log に追記（symlink チェック付き）
                let log_dir = root.join(LOG_DIR);
                std::fs::create_dir_all(&log_dir).ok();
                let entry = format!("[{}] $ {}\n{}\n---\n", now_timestamp(), cmd.join(" "), output);
                safe_append_log(&log_dir.join("cmd_log"), &entry);

                results.push(ToolResult {
                    label: format!("Cmd({name})"),
                    output,
                });
            }
            AiCommand::Patch { path, diff } => {
                let output = match resolve(root, path) {
                    Err(e) => format!("ERROR: {e}"),
                    Ok(abs) => {
                        if !read_files.contains(&abs) {
                            format!("ERROR: '{path}' は事前に read_file で読み込んでいません。patch の前に read_file で内容を確認してください。")
                        } else if !abs.exists() {
                            format!("ERROR: '{path}' が存在しません。patch はファイルが存在する場合のみ使用できます。")
                        } else if diff.trim().is_empty() || !diff.contains("@@") {
                            "ERROR: diff が空または形式が不正です。@@ ヘッダーを含む unified diff 形式で指定してください。\n例: \"@@ -5,3 +5,3 @@\\n context\\n-旧行\\n+新行\\n context\"".to_string()
                        } else {
                            match std::fs::read_to_string(&abs) {
                                Err(e) => format!("ERROR: ファイル読み込み失敗: {e}"),
                                Ok(content) => match apply_unified_diff(&content, diff) {
                                    Err(e) => format!("ERROR: {e}"),
                                    Ok(patched) => match std::fs::write(&abs, &patched) {
                                        Err(e) => format!("ERROR: 書き込み失敗: {e}"),
                                        Ok(_) => {
                                            read_files.insert(abs);
                                            "OK".to_string()
                                        }
                                    },
                                },
                            }
                        }
                    }
                };
                results.push(ToolResult {
                    label: format!("Patch({path})"),
                    output,
                });
            }
            AiCommand::ReadLog { filename } => {
                const MAX_LOG_BYTES: u64 = 32 * 1024; // 末尾 32KB のみ返す
                let output = if !ALLOWED_LOGS.contains(&filename.as_str()) {
                    format!(
                        "ERROR: 不正なログ名 '{filename}'。使用可能: {}",
                        ALLOWED_LOGS.join(", ")
                    )
                } else {
                    let log_path = root.join(LOG_DIR).join(filename);
                    // シンボリックリンク経由のルート外読み取りを拒否
                    if log_path.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                        format!("ERROR: '{filename}' はシンボリックリンクです。読み取りを拒否しました")
                    } else {
                        match std::fs::read(&log_path) {
                            Ok(bytes) if bytes.is_empty() => "(ログは空です)".to_string(),
                            Ok(bytes) => {
                                // サイズ制限: 末尾 32KB のみ
                                let start = bytes.len().saturating_sub(MAX_LOG_BYTES as usize);
                                let slice = &bytes[start..];
                                let prefix = if start > 0 { "[先頭部分省略]\n" } else { "" };
                                format!("{prefix}{}", String::from_utf8_lossy(slice))
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                                "(ログファイルが存在しません)".to_string()
                            }
                            Err(e) => format!("ERROR: {e}"),
                        }
                    }
                };
                results.push(ToolResult {
                    label: format!("ReadLog({filename})"),
                    output,
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

pub fn format_tool_results(results: &[ToolResult]) -> String {
    const MAX_TOTAL_CHARS: usize = 10_000;
    let mut parts = vec!["[ツール実行結果]".to_string()];
    let mut used = parts[0].len();
    let total = results.len();
    for (i, r) in results.iter().enumerate() {
        let entry = format!("## {}\n{}", r.label, r.output);
        if used + entry.len() > MAX_TOTAL_CHARS {
            let remaining = total - i;
            parts.push(format!(
                "[残り {} 件の結果を省略（合計文字数制限）。次のターンで続きを確認してください]",
                remaining
            ));
            break;
        }
        used += entry.len() + 2; // +2 for "\n\n"
        parts.push(entry);
    }
    parts.join("\n\n")
}
