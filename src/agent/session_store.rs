/// セッション保存・復元（core-internals.md §7 session.mjs を参考）
///
/// run_agent() の状態（read_files / done_log）をターンごとにディスクに保存する。
/// 20ターン上限・Ctrl+C・クラッシュ後も「続きから」再開できる。
///
/// 保存先: ホームディレクトリ配下の .copipe_sessions/<path_hash16>/session.json
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const SESSION_VERSION: u32 = 1;

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

// ─── セッションストア ─────────────────────────────────────────────────────────

pub struct SessionStore {
    session_path: PathBuf,
}

impl SessionStore {
    /// root に紐づいたセッションストアを作成する
    pub fn new(root: &Path) -> Self {
        let hash = path_hash16(root);
        let dir = session_dir_for_hash(&hash);
        Self {
            session_path: dir.join("session.json"),
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
            std::fs::write(&self.session_path, json).ok();
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
}

// ─── ユーティリティ ───────────────────────────────────────────────────────────

/// セッション保存ディレクトリのパスを返す
fn home_sessions_dir() -> PathBuf {
    crate::paths::home_dir()
        .map(|h| h.join(".copipe_sessions"))
        .unwrap_or_else(|| std::env::temp_dir().join("copipe_sessions"))
}

fn session_dir_for_hash(hash: &str) -> PathBuf {
    let primary = home_sessions_dir().join(hash);
    if ensure_writable_dir(&primary) {
        return primary;
    }

    let fallback = std::env::temp_dir().join("copipe_sessions").join(hash);
    ensure_writable_dir(&fallback);
    fallback
}

fn ensure_writable_dir(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".write_probe");
    match std::fs::write(&probe, b"ok") {
        Ok(_) => {
            let _ = std::fs::remove_file(probe);
            true
        }
        Err(_) => false,
    }
}

/// パスを16文字の16進ハッシュに変換する（ディレクトリ名用）
fn path_hash16(path: &Path) -> String {
    let s = path.display().to_string();
    let mut h: u64 = 0xcbf29ce484222325; // FNV-1a offset basis
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3); // FNV prime
    }
    format!("{h:016x}")
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

    #[test]
    fn test_path_hash16_is_consistent() {
        let p = Path::new("/home/user/myproject");
        let h1 = path_hash16(p);
        let h2 = path_hash16(p);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
    }

    #[test]
    fn test_path_hash16_different_paths() {
        let h1 = path_hash16(Path::new("/home/user/project_a"));
        let h2 = path_hash16(Path::new("/home/user/project_b"));
        assert_ne!(h1, h2);
    }
}
