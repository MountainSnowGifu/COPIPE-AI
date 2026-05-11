use crate::executor::ToolResult;
use crate::executor::context::ToolContext;

pub fn handle(ctx: &mut ToolContext<'_>, path: &str, content: &str) -> ToolResult {
    let output = match ctx.resolve(path) {
        Err(e) => crate::executor::errors::tool_error(&e),
        Ok(abs) => {
            if abs
                .symlink_metadata()
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
            {
                crate::executor::errors::perm_denied(format!("'{path}' はシンボリックリンクです"))
            } else if !abs.exists()
                && !abs
                    .parent()
                    .map(|parent| parent.exists() && parent.is_dir())
                    .unwrap_or(false)
            {
                format!(
                    "ERROR: 親ディレクトリが存在しません。先に mkdir で作成し、必要なら list_dir/glob で配置を確認してください: {path}"
                )
            } else if abs.exists() && !ctx.read_files.contains(&abs) {
                crate::executor::errors::unread_file(path)
            } else {
                // 上書き前にチェックポイント保存（新規ファイルはスキップ）
                if abs.exists() {
                    if let Ok(old) = std::fs::read_to_string(&abs) {
                        ctx.checkpoints.save(&abs, &old, "write");
                    }
                }
                match std::fs::write(&abs, content) {
                    Ok(_) => {
                        ctx.read_files.insert(abs);
                        "OK".to_string()
                    }
                    Err(e) => crate::executor::errors::tool_error(&e),
                }
            }
        }
    };
    ToolResult::new(format!("WriteFile({path})"), output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::CheckpointManager;
    use std::collections::HashSet;

    fn context_for<'a>(
        root: &'a std::path::Path,
        read_files: &'a mut HashSet<std::path::PathBuf>,
        checkpoints: &'a mut CheckpointManager,
    ) -> ToolContext<'a> {
        ToolContext::new(root, read_files, checkpoints)
    }

    #[test]
    fn new_file_requires_existing_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut read_files = HashSet::new();
        let mut checkpoints = CheckpointManager::new(dir.path());
        let mut ctx = context_for(dir.path(), &mut read_files, &mut checkpoints);

        let result = handle(&mut ctx, "docs/spec.md", "# spec\n");

        assert!(result.output.contains("親ディレクトリが存在しません"));
        assert!(!dir.path().join("docs").exists());
    }

    #[test]
    fn new_file_in_existing_parent_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        let mut read_files = HashSet::new();
        let mut checkpoints = CheckpointManager::new(dir.path());
        let mut ctx = context_for(dir.path(), &mut read_files, &mut checkpoints);

        let result = handle(&mut ctx, "src/new.rs", "pub fn new() {}\n");

        assert_eq!(result.output, "OK");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("src/new.rs")).unwrap(),
            "pub fn new() {}\n"
        );
    }
}
