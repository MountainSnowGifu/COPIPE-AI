use crate::executor::ToolResult;
use crate::executor::context::ToolContext;

pub fn handle(ctx: &mut ToolContext<'_>, path: &str) -> ToolResult {
    let output = match ctx.resolve(path) {
        Err(e) => crate::executor::errors::tool_error(&e),
        Ok(abs) => {
            let raw_path = ctx.root.join(path);
            let target = if raw_path
                .symlink_metadata()
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
            {
                raw_path
            } else {
                abs.clone()
            };
            if target.exists() && !ctx.read_files.contains(&abs) {
                crate::executor::errors::perm_denied(format!(
                    "'{path}' は未読です。先に read_file で内容を確認してください"
                ))
            } else {
                match std::fs::remove_file(&target) {
                    Ok(_) => {
                        ctx.read_files.remove(&abs);
                        "OK".to_string()
                    }
                    Err(e) => crate::executor::errors::tool_error(&e),
                }
            }
        }
    };
    ToolResult::new(format!("DeleteFile({path})"), output)
}
