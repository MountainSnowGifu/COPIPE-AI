/// システムデバッグログ
///
/// --debug フラグ有効時に .copipe_logs/debug_log へ書き込む。
/// 各ターンの送信プロンプト全文・処理時間・レートリミッタ状態・セッション状態を記録する。

use crate::executor::{now_timestamp, safe_append_log, LOG_DIR};
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
            // debug_log のみリセット（ai_log 等はそのまま）
            if let Ok(path) = std::fs::canonicalize(&log_dir) {
                std::fs::write(path.join("debug_log"), "").ok();
            } else {
                std::fs::write(log_dir.join("debug_log"), "").ok();
            }
        }
        Self { root: root.to_path_buf(), enabled, turn_start: None }
    }

    fn write(&self, content: &str) {
        if !self.enabled { return; }
        let path = self.root.join(LOG_DIR).join("debug_log");
        safe_append_log(&path, content);
    }

    /// ターン開始を記録してタイマー開始
    pub fn turn_start(&mut self, turn: u32, max_turns: u32) {
        if !self.enabled { return; }
        self.turn_start = Some(Instant::now());
        self.write(&format!(
            "\n=== TURN {}/{} [{}] ===\n",
            turn + 1, max_turns, now_timestamp()
        ));
    }

    /// 送信プロンプト全文を記録
    pub fn log_prompt(&self, prompt: &str) {
        if !self.enabled { return; }
        let truncated = if prompt.chars().count() > 8_000 {
            let head: String = prompt.chars().take(8_000).collect();
            format!("{head}\n...[{} 文字以降省略]", prompt.chars().count() - 8_000)
        } else {
            prompt.to_string()
        };
        self.write(&format!(
            "[PROMPT] {} chars:\n---PROMPT START---\n{}\n---PROMPT END---\n",
            prompt.chars().count(),
            truncated
        ));
    }

    /// レートリミッタの状態を記録
    pub fn log_rate_limiter(&self, consecutive_issues: u32, wait_ms: u64) {
        if !self.enabled { return; }
        self.write(&format!(
            "[RATE_LIMITER] consecutive_issues={consecutive_issues} wait={:.1}s\n",
            wait_ms as f64 / 1000.0
        ));
    }

    /// セッション状態（read_files・done_log）を記録
    pub fn log_session(
        &self,
        read_files: &HashSet<PathBuf>,
        done_log: &[String],
        root: &Path,
    ) {
        if !self.enabled { return; }
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
            done_log.iter()
                .rev()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join(" → ")
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
        if !self.enabled { return; }
        let elapsed = self.turn_start.take()
            .map(|s| s.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        self.write(&format!("[TIMING] {:.2}s\n---\n", elapsed));
    }

    /// エラーを記録
    pub fn log_error(&self, err: &str) {
        if !self.enabled { return; }
        self.write(&format!("[ERROR] {err}\n"));
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }
}
