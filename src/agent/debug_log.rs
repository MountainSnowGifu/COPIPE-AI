/// システムデバッグログ
///
/// --debug フラグ有効時に .copipe_logs/debug_log へ書き込む。
/// 各ターンの送信プロンプト全文・処理時間・レートリミッタ状態・セッション状態を記録する。
use crate::executor::{LOG_DIR, now_timestamp, safe_append_log};
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
        const LIMIT: usize = 8_000;
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
