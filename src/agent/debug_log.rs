/// システムデバッグログ
///
/// --debug フラグ有効時に .copipe_logs/debug_log へ書き込む。
/// 各ターンの送信プロンプト全文・処理時間・レートリミッタ状態・セッション状態を記録する。
use crate::executor::{LOG_DIR, now_timestamp, safe_append_log};
use crate::{command::AiCommand, executor::ToolResult};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub struct DebugLogger {
    root: PathBuf,
    enabled: bool,
    turn_start: Option<Instant>,
}

impl DebugLogger {
    pub fn new(root: &Path, enabled: bool) -> Self {
        if enabled {
            let log_dir = root.join(LOG_DIR);
            // ログディレクトリを作成しておく（存在しなければ作る）
            let _ = std::fs::create_dir_all(&log_dir);

            // debug_log のみ安全に初期化（truncate）する
            #[cfg(unix)]
            {
                use std::io::Write;
                use std::os::unix::fs::OpenOptionsExt;
                #[cfg(target_os = "linux")]
                const O_NOFOLLOW: i32 = 0o400000;
                #[cfg(target_os = "macos")]
                const O_NOFOLLOW: i32 = 0x100;
                #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                const O_NOFOLLOW: i32 = 0;

                let _ = std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .custom_flags(O_NOFOLLOW)
                    .open(log_dir.join("debug_log"))
                    .and_then(|mut f| f.write_all(b""));
            }
            #[cfg(not(unix))]
            {
                // 非Unixでは通常の書き込みで初期化
                let _ = std::fs::write(log_dir.join("debug_log"), "");
            }
        }

        Self {
            root: root.to_path_buf(),
            enabled,
            turn_start: None,
        }
    }

    fn write(&self, content: &str) {
        if !self.enabled {
            return;
        }
        let path = self.root.join(LOG_DIR).join("debug_log");
        safe_append_log(&path, content);
    }

    /// フリーズ切り分け用の軽量イベントログ。
    ///
    /// 重い DOM 診断は browser_log 側に任せ、ここでは「どの段階まで進んだか」を残す。
    pub fn log_event(&self, phase: &str, detail: impl AsRef<str>) {
        if !self.enabled {
            return;
        }
        self.write(&format!(
            "[EVENT] {} phase={} {}\n",
            now_timestamp(),
            phase,
            detail.as_ref()
        ));
    }

    /// ターン開始を記録してタイマー開始
    pub fn turn_start(&mut self, turn: u32, max_turns: u32) {
        if !self.enabled {
            return;
        }
        self.turn_start = Some(Instant::now());
        self.write(&format!(
            "\n=== TURN {}/{} [{}] ===\n",
            turn + 1,
            max_turns,
            now_timestamp()
        ));
    }

    /// 送信プロンプト全文を記録
    pub fn log_prompt(&self, prompt: &str) {
        if !self.enabled {
            return;
        }
        const LIMIT: usize = 20_000;
        let char_count = prompt.chars().count();
        let truncated = if char_count > LIMIT {
            let head: String = prompt.chars().take(LIMIT).collect();
            format!("{}\n...[{} 文字以降省略]", head, char_count - LIMIT)
        } else {
            prompt.to_string()
        };
        self.write(&format!(
            "[PROMPT] {} chars:\n---PROMPT START---\n{}\n---PROMPT END---\n",
            char_count, truncated
        ));
    }

    /// レートリミッタの状態を記録
    pub fn log_rate_limiter(&self, consecutive_issues: u32, wait_ms: u64) {
        if !self.enabled {
            return;
        }
        self.write(&format!(
            "[RATE_LIMITER] consecutive_issues={consecutive_issues} wait={:.1}s\n",
            wait_ms as f64 / 1000.0
        ));
    }

    /// セッション状態（read_files・done_log）を記録
    pub fn log_session(&self, read_files: &HashSet<PathBuf>, done_log: &[String], root: &Path) {
        if !self.enabled {
            return;
        }
        let mut files: Vec<String> = read_files
            .iter()
            .filter_map(|p| p.strip_prefix(root).ok().map(|r| r.display().to_string()))
            .collect();
        files.sort();

        let read_str = if files.is_empty() {
            "(なし)".to_string()
        } else {
            files.join(", ")
        };

        let done_str = if done_log.is_empty() {
            "(なし)".to_string()
        } else {
            let mut recent: Vec<_> = done_log.iter().rev().take(5).cloned().collect();
            recent.reverse();
            recent.join(" → ")
        };

        self.write(&format!(
            "[SESSION] read_files={} done_log={}\n  read: {}\n  done: {}\n",
            read_files.len(),
            done_log.len(),
            read_str,
            done_str,
        ));
    }

    /// AI から返ったコマンドの概要を記録する。
    pub fn log_commands(&self, commands: &[AiCommand], parse_errors: &[String]) {
        if !self.enabled {
            return;
        }
        let summary = commands
            .iter()
            .map(command_label)
            .collect::<Vec<_>>()
            .join(", ");
        self.write(&format!(
            "[COMMANDS] count={} parse_errors={} [{}]\n",
            commands.len(),
            parse_errors.len(),
            summary
        ));
        for err in parse_errors.iter().take(3) {
            self.write(&format!("[PARSE_ERROR] {}\n", one_line(err, 500)));
        }
    }

    /// ツール実行結果の概要を記録する。
    pub fn log_tool_results(&self, results: &[ToolResult]) {
        if !self.enabled {
            return;
        }
        self.write(&format!("[TOOL_RESULTS] count={}\n", results.len()));
        for r in results {
            let status = if crate::executor::errors::is_error_output(&r.output) {
                "error"
            } else {
                "ok"
            };
            self.write(&format!(
                "  [RESULT] #{} {} status={} bytes={} first_line={}\n",
                r.cmd_index,
                r.label,
                status,
                r.output.len(),
                one_line(&r.output, 300)
            ));
        }
    }

    /// 次のループ状態を記録する。
    pub fn log_outcome(&self, outcome: &str) {
        if !self.enabled {
            return;
        }
        self.write(&format!("[OUTCOME] {outcome}\n"));
    }

    /// ターン終了（処理時間を記録）
    pub fn turn_end(&mut self) {
        if !self.enabled {
            return;
        }
        let elapsed = self
            .turn_start
            .take()
            .map(|s| s.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        self.write(&format!("[TIMING] {:.2}s\n---\n", elapsed));
    }

    /// エラーを記録
    #[allow(dead_code)]
    pub fn log_error(&self, err: &str) {
        if !self.enabled {
            return;
        }
        self.write(&format!("[ERROR] {err}\n"));
    }

    #[allow(dead_code)]
    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

fn command_label(cmd: &AiCommand) -> String {
    match cmd {
        AiCommand::ReadFile { path, offset_lines } => {
            if *offset_lines > 0 {
                format!("read_file:{path}@{offset_lines}")
            } else {
                format!("read_file:{path}")
            }
        }
        AiCommand::ListDir { path } => format!("list_dir:{path}"),
        AiCommand::Grep { pattern, path, .. } => format!("grep:{pattern} in {path}"),
        AiCommand::Glob { pattern } => format!("glob:{pattern}"),
        AiCommand::Edit { path, .. } => format!("edit:{path}"),
        AiCommand::MultiEdit { path, edits } => format!("multi_edit:{path}({})", edits.len()),
        AiCommand::Patch { path, .. } => format!("patch:{path}"),
        AiCommand::File { path, content } => format!("write_file:{path}({} bytes)", content.len()),
        AiCommand::DeleteFile { path } => format!("delete_file:{path}"),
        AiCommand::DeleteFolder { path } => format!("delete_folder:{path}"),
        AiCommand::Mkdir { path } => format!("mkdir:{path}"),
        AiCommand::Cmd {
            name, cmd, timeout, ..
        } => {
            format!("cmd:{name} [{}] timeout={}s", cmd.join(" "), timeout)
        }
        AiCommand::AskUser { .. } => "ask_user".to_string(),
        AiCommand::TodoWrite { todos } => format!("todo_write({})", todos.len()),
        AiCommand::ReadLog {
            filename,
            offset_lines,
        } => {
            if *offset_lines > 0 {
                format!("read_log:{filename}@{offset_lines}")
            } else {
                format!("read_log:{filename}")
            }
        }
        AiCommand::WebFetch { url, .. } => format!("web_fetch:{url}"),
        AiCommand::EnterWorktree => "enter_worktree".to_string(),
        AiCommand::ExitWorktree { action, .. } => format!("exit_worktree:{action}"),
        AiCommand::Txt { content } => format!("txt({} chars)", content.chars().count()),
        AiCommand::Bot { message, content } => format!(
            "bot({} chars)",
            message
                .as_deref()
                .or(content.as_deref())
                .unwrap_or("")
                .chars()
                .count()
        ),
        AiCommand::Error { .. } => "error".to_string(),
    }
}

fn one_line(text: &str, limit: usize) -> String {
    let normalized = text
        .lines()
        .next()
        .unwrap_or("")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    let mut out: String = normalized.chars().take(limit).collect();
    if normalized.chars().count() > limit {
        out.push_str("...");
    }
    out
}
