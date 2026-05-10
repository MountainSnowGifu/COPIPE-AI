/// ファイル編集のチェックポイント管理（core-internals.md §8 checkpoints.mjs を参考）
///
/// 書き込み系ツール（write_file / edit / multi_edit / patch）が
/// ファイルを変更する前に元の内容をここに保存する。
/// `:undo` で直前の状態に戻せる。git 不要・軽量・常に使える。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const CHECKPOINT_DIR: &str = ".copipe_checkpoints";
const MAX_CHECKPOINTS: usize = 50;

#[derive(Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: String,
    pub file_path: PathBuf,      // absolute
    pub relative_path: String,   // root からの相対パス（表示用）
    pub content: String,         // 変更前のファイル内容
    pub operation: String,       // "write" | "edit" | "patch" など
    pub timestamp_ms: u64,
}

pub struct CheckpointManager {
    root: PathBuf,
    history: Vec<String>, // checkpoint ID のスタック（LIFO）
    counter: u64,         // 同一ミリ秒内の衝突防止
}

impl CheckpointManager {
    /// root に紐づいたマネージャを作成する
    pub fn new(root: &Path) -> Self {
        let dir = root.join(CHECKPOINT_DIR);
        std::fs::create_dir_all(&dir).ok();
        Self {
            root: root.to_path_buf(),
            history: Vec::new(),
            counter: 0,
        }
    }

    fn checkpoint_dir(&self) -> PathBuf {
        self.root.join(CHECKPOINT_DIR)
    }

    fn checkpoint_path(&self, id: &str) -> PathBuf {
        self.checkpoint_dir().join(format!("{id}.json"))
    }

    /// ファイルの現在内容をチェックポイントとして保存する
    /// 保存に成功したらチェックポイント ID を返す
    pub fn save(&mut self, file_path: &Path, content: &str, operation: &str) -> Option<String> {
        // symlink チェック（セキュリティ）
        if file_path.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
            return None;
        }

        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.counter += 1;
        let id = format!("ckpt_{ts}_{:06}", self.counter);

        let relative_path = file_path
            .strip_prefix(&self.root)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| file_path.display().to_string());

        let ckpt = Checkpoint {
            id: id.clone(),
            file_path: file_path.to_path_buf(),
            relative_path,
            content: content.to_string(),
            operation: operation.to_string(),
            timestamp_ms: ts,
        };

        let json = match serde_json::to_string(&ckpt) {
            Ok(s) => s,
            Err(_) => return None,
        };

        if std::fs::write(self.checkpoint_path(&id), json).is_err() {
            return None;
        }

        self.history.push(id.clone());

        // 上限超えたら最古を削除
        if self.history.len() > MAX_CHECKPOINTS {
            let oldest = self.history.remove(0);
            let _ = std::fs::remove_file(self.checkpoint_path(&oldest));
        }

        Some(id)
    }

    /// 直前のチェックポイントを復元する
    pub fn undo(&mut self) -> anyhow::Result<Option<(String, String)>> {
        self.undo_at(0)
    }

    /// N番目（新しい順）のチェックポイントを復元する
    pub fn undo_at(&mut self, n: usize) -> anyhow::Result<Option<(String, String)>> {
        if self.history.is_empty() {
            return Ok(None);
        }
        let rev_idx = self.history.len().saturating_sub(1 + n);
        let id = self.history.remove(rev_idx);

        let path = self.checkpoint_path(&id);
        let json = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("チェックポイントファイルの読み込みに失敗: {e}"))?;

        let ckpt: Checkpoint = serde_json::from_str(&json)
            .map_err(|e| anyhow::anyhow!("チェックポイントのパースに失敗: {e}"))?;

        // ファイルを元の内容に戻す
        std::fs::write(&ckpt.file_path, &ckpt.content)
            .map_err(|e| anyhow::anyhow!("ファイルの復元に失敗: {e}"))?;

        // チェックポイントファイルを削除
        let _ = std::fs::remove_file(&path);

        Ok(Some((ckpt.relative_path, ckpt.operation)))
    }

    /// 現在のスタックを一覧表示用に返す（新しい順）
    pub fn list(&self) -> Vec<(String, String)> {
        self.history
            .iter()
            .rev()
            .filter_map(|id| {
                let path = self.checkpoint_path(id);
                std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| serde_json::from_str::<Checkpoint>(&s).ok())
                    .map(|c| (c.relative_path, c.operation))
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.history.len()
    }

    pub fn is_empty(&self) -> bool {
        self.history.is_empty()
    }
}

// ─── テスト ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, CheckpointManager) {
        let dir = tempfile::tempdir().unwrap();
        let mgr = CheckpointManager::new(dir.path());
        (dir, mgr)
    }

    #[test]
    fn test_save_and_undo() {
        let (dir, mut mgr) = setup();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "original content").unwrap();

        // チェックポイント保存
        let id = mgr.save(&file, "original content", "edit");
        assert!(id.is_some());
        assert_eq!(mgr.len(), 1);

        // ファイルを変更
        std::fs::write(&file, "modified content").unwrap();

        // Undo で復元
        let result = mgr.undo().unwrap();
        assert!(result.is_some());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "original content");
        assert_eq!(mgr.len(), 0);
    }

    #[test]
    fn test_undo_empty_returns_none() {
        let (_dir, mut mgr) = setup();
        let result = mgr.undo().unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_max_checkpoints_eviction() {
        let (dir, mut mgr) = setup();
        mgr.history.clear();

        let file = dir.path().join("f.rs");
        std::fs::write(&file, "x").unwrap();

        for _ in 0..=MAX_CHECKPOINTS {
            mgr.save(&file, "x", "edit");
        }
        // MAX_CHECKPOINTS を超えない
        assert!(mgr.len() <= MAX_CHECKPOINTS);
    }

    #[test]
    fn test_list_newest_first() {
        let (dir, mut mgr) = setup();
        let file1 = dir.path().join("a.rs");
        let file2 = dir.path().join("b.rs");
        std::fs::write(&file1, "a").unwrap();
        std::fs::write(&file2, "b").unwrap();

        mgr.save(&file1, "a", "write");
        mgr.save(&file2, "b", "edit");

        let list = mgr.list();
        assert_eq!(list.len(), 2);
        // 新しい順（b が先）
        assert!(list[0].0.contains('b'));
        assert!(list[1].0.contains('a'));
    }
}
