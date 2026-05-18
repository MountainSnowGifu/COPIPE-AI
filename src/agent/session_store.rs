/// セッション保存・復元（core-internals.md §7 session.mjs を参考）
///
/// run_agent() の状態（read_files / done_log）をターンごとにディスクに保存する。
/// 20ターン上限・Ctrl+C・クラッシュ後も「続きから」再開できる。
///
/// 保存先: プロジェクトルート配下の .copipe_logs/session.json
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const SESSION_VERSION: u32 = 1;
const COMPLETION_VERSION: u32 = 1;

// ─── データ構造 ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionData {
    pub version: u32,
    pub project_dir: String, // root の絶対パス
    pub user_task: String,
    pub turn_count: u32,
    pub saved_at: String,        // JST タイムスタンプ
    pub done_log: Vec<String>,   // "✓ label" / "✗ label" の履歴
    pub read_files: Vec<String>, // root からの相対パス
}

/// タスク完了後のサマリー — 次のタスクの初期プロンプトにコンテキストとして注入する
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionSummary {
    pub version: u32,
    pub user_task: String,
    pub done_log: Vec<String>, // 完了タスクの操作履歴（直近最大20件）
    pub saved_at: String,
}

// ─── セッションストア ─────────────────────────────────────────────────────────

pub struct SessionStore {
    session_path: PathBuf,
}

impl SessionStore {
    /// root に紐づいたセッションストアを作成する
    pub fn new(root: &Path) -> Self {
        let log_dir = root.join(crate::executor::LOG_DIR);
        std::fs::create_dir_all(&log_dir).ok();
        Self {
            session_path: log_dir.join("session.json"),
        }
    }

    /// セッションデータを保存する
    pub fn save(
        &self,
        root: &Path,
        user_task: &str,
        turn_count: u32,
        done_log: &[String],
        read_files_abs: &std::collections::HashSet<PathBuf>,
    ) {
        let read_files: Vec<String> = {
            let mut v: Vec<String> = read_files_abs
                .iter()
                .filter_map(|p| p.strip_prefix(root).ok().map(|r| r.display().to_string()))
                .collect();
            v.sort();
            v
        };

        let data = SessionData {
            version: SESSION_VERSION,
            project_dir: root.display().to_string(),
            user_task: user_task.to_string(),
            turn_count,
            saved_at: crate::executor::now_timestamp(),
            done_log: done_log.to_vec(),
            read_files,
        };

        if let Ok(json) = serde_json::to_string_pretty(&data) {
            if let Err(e) = std::fs::write(&self.session_path, &json) {
                eprintln!(
                    "[WARN] セッションの保存に失敗しました ({}): {}",
                    self.session_path.display(),
                    e
                );
            }
        }
    }

    /// 保存済みセッションをロードする（なければ None）
    pub fn load(&self) -> Option<SessionData> {
        let json = std::fs::read_to_string(&self.session_path).ok()?;
        let data: SessionData = serde_json::from_str(&json).ok()?;
        if data.version != SESSION_VERSION {
            return None;
        }
        Some(data)
    }

    /// セッションファイルを削除する（正常完了時）
    pub fn clear(&self) {
        std::fs::remove_file(&self.session_path).ok();
    }

    pub fn exists(&self) -> bool {
        self.session_path.exists()
    }

    /// タスク完了後のサマリーを保存する（次タスクへのコンテキスト引き継ぎ用）
    pub fn save_completion(&self, user_task: &str, done_log: &[String]) {
        // 直近20件だけ保持（read_file / glob 系は除いてファイル操作を優先）
        let filtered: Vec<String> = done_log
            .iter()
            .filter(|s| {
                s.starts_with("✓ WriteFile(")
                    || s.starts_with("✓ Edit(")
                    || s.starts_with("✓ MultiEdit(")
                    || s.starts_with("✓ Patch(")
                    || s.starts_with("✓ Mkdir(")
                    || s.starts_with("✓ DeleteFile(")
                    || s.starts_with("✓ Cmd(")
                    || s.starts_with("✗ ")
            })
            .cloned()
            .collect();
        let recent: Vec<String> = filtered.into_iter().rev().take(20).rev().collect();

        let summary = CompletionSummary {
            version: COMPLETION_VERSION,
            user_task: user_task.to_string(),
            done_log: recent,
            saved_at: crate::executor::now_timestamp(),
        };

        let completion_path = self.session_path.with_file_name("completion.json");
        if let Ok(json) = serde_json::to_string_pretty(&summary) {
            std::fs::write(&completion_path, &json).ok();
        }
    }

    /// 直前タスクの完了サマリーをロードする（なければ None）
    pub fn load_completion(&self) -> Option<CompletionSummary> {
        let path = self.session_path.with_file_name("completion.json");
        let json = std::fs::read_to_string(&path).ok()?;
        let data: CompletionSummary = serde_json::from_str(&json).ok()?;
        if data.version != COMPLETION_VERSION {
            return None;
        }
        Some(data)
    }

    /// 完了サマリーを削除する（同タスクの再開時など、不要になったとき）
    pub fn clear_completion(&self) {
        let path = self.session_path.with_file_name("completion.json");
        std::fs::remove_file(&path).ok();
    }
}

// ─── テスト ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_save_and_load() {
        let root = std::env::temp_dir().join("copipe_test_sess");
        std::fs::create_dir_all(&root).ok();
        let store = SessionStore::new(&root);

        let mut read_files = HashSet::new();
        read_files.insert(root.join("src/main.rs"));
        read_files.insert(root.join("src/lib.rs"));
        let done_log = vec![
            "✓ ListDir(src)".to_string(),
            "✓ ReadFile(src/main.rs)".to_string(),
        ];

        store.save(&root, "コードレビューして", 5, &done_log, &read_files);
        assert!(store.exists());

        let loaded = store.load().unwrap();
        assert_eq!(loaded.user_task, "コードレビューして");
        assert_eq!(loaded.turn_count, 5);
        assert_eq!(loaded.done_log.len(), 2);
        assert_eq!(loaded.read_files.len(), 2);
        assert!(loaded.read_files.iter().any(|f| f.contains("main.rs")));

        store.clear();
        assert!(!store.exists());
    }
}
