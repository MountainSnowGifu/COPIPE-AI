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
            } else if abs.exists() && !ctx.read_files.contains(&abs) {
                crate::executor::errors::unread_file(path)
            } else {
                if let Some(parent) = abs.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
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
