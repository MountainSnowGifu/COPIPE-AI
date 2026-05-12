use crate::executor::ToolResult;
use crate::executor::context::ToolContext;

pub fn handle(ctx: &ToolContext<'_>, path: &str) -> ToolResult {
    let label = format!("Mkdir({path})");
    let abs = match ctx.resolve(path) {
        Err(e) => return ToolResult::new(label, crate::executor::errors::tool_error(&e)),
        Ok(p) => p,
    };
    if let Err(e) = std::fs::create_dir_all(&abs) {
        return ToolResult::new(label, crate::executor::errors::tool_error(&e));
    }
    // 作成後に canonicalize して再確認（TOCTOU: 作成中に symlink が差し替えられた場合の対策）
    let root_canonical = match ctx.root.canonicalize() {
        Ok(c) => c,
        Err(e) => return ToolResult::new(label, crate::executor::errors::tool_error(&e)),
    };
    let output = match abs.canonicalize() {
        Ok(canonical) if !canonical.starts_with(&root_canonical) => {
            // abs が symlink に差し替えられている場合は symlink 自体のみ削除し、
            // 外部の実体ディレクトリには一切触れない（競合攻撃でプロジェクト外を削除させない）
            if abs
                .symlink_metadata()
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
            {
                let _ = std::fs::remove_file(&abs);
            }
            crate::executor::errors::perm_denied(format!(
                "'{path}' がプロジェクトルート外に解決されました（作成後確認失敗）"
            ))
        }
        Ok(_) => "OK".to_string(),
        Err(e) => crate::executor::errors::tool_error(&e),
    };
    ToolResult::new(label, output)
}
