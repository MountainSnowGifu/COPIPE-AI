use crate::executor::checkpoints::CheckpointManager;
use crate::executor::errors::{perm_denied, tool_error};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub struct ToolContext<'a> {
    pub root: &'a Path,
    pub read_files: &'a mut HashSet<PathBuf>,
    pub turn_read_chars: usize,
    pub max_turn_read: usize,
    pub checkpoints: &'a mut CheckpointManager,
}

impl<'a> ToolContext<'a> {
    pub fn new(
        root: &'a Path,
        read_files: &'a mut HashSet<PathBuf>,
        checkpoints: &'a mut CheckpointManager,
    ) -> Self {
        Self {
            root,
            read_files,
            turn_read_chars: 0,
            max_turn_read: 7_000,
            checkpoints,
        }
    }

    /// パスをルート相対で解決してセキュリティチェックを行う
    pub fn resolve(&self, raw: &str) -> Result<PathBuf, String> {
        let raw_path = Path::new(raw);
        if crate::paths::is_absolute_path_arg(raw) {
            return Err(perm_denied(format!("絶対パス '{raw}' は使えません")));
        }
        if crate::paths::has_parent_component_arg(raw) {
            return Err(perm_denied(format!("'..' を含むパス '{raw}' は使えません")));
        }

        let root_canonical = self
            .root
            .canonicalize()
            .map_err(|e| tool_error(format!("root の解決に失敗: {e}")))?;
        let joined = root_canonical.join(raw_path);

        if joined.exists() {
            let canonical = joined
                .canonicalize()
                .map_err(|e| tool_error(format!("パスの解決に失敗: {e}")))?;
            if !canonical.starts_with(&root_canonical) {
                return Err(perm_denied(format!(
                    "'{raw}' はプロジェクトルート外を指しています（シンボリックリンク経由の可能性）"
                )));
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
            return Err(perm_denied(format!("'{raw}' はプロジェクトルート外です")));
        }
        Ok(joined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context_for<'a>(
        root: &'a Path,
        read_files: &'a mut HashSet<PathBuf>,
        checkpoints: &'a mut CheckpointManager,
    ) -> ToolContext<'a> {
        ToolContext::new(root, read_files, checkpoints)
    }

    #[test]
    fn rejects_windows_absolute_paths_on_all_targets() {
        let dir = tempfile::tempdir().unwrap();
        let mut read_files = HashSet::new();
        let mut checkpoints = CheckpointManager::new(dir.path());
        let ctx = context_for(dir.path(), &mut read_files, &mut checkpoints);

        assert!(ctx.resolve(r"C:\Users\akira\secret.txt").is_err());
        assert!(ctx.resolve(r"\\server\share\secret.txt").is_err());
    }

    #[test]
    fn rejects_windows_parent_components_on_all_targets() {
        let dir = tempfile::tempdir().unwrap();
        let mut read_files = HashSet::new();
        let mut checkpoints = CheckpointManager::new(dir.path());
        let ctx = context_for(dir.path(), &mut read_files, &mut checkpoints);

        assert!(ctx.resolve(r"src\..\secret.txt").is_err());
    }
}
