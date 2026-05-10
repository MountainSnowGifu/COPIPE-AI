use crate::executor::context::ToolContext;
use crate::executor::diff::apply_unified_diff;
use crate::executor::ToolResult;

pub fn handle(ctx: &mut ToolContext<'_>, path: &str, diff: &str) -> ToolResult {
    let output = match ctx.resolve(path) {
        Err(e) => crate::executor::errors::tool_error(&e),
        Ok(abs) => {
            if !ctx.read_files.contains(&abs) {
                crate::executor::errors::unread_file(path)
            } else if !abs.exists() {
                format!("ERROR: '{path}' が存在しません。patch はファイルが存在する場合のみ使用できます。")
            } else if diff.trim().is_empty() || !diff.contains("@@") {
                "ERROR: diff が空または形式が不正です。@@ ヘッダーを含む unified diff 形式で指定してください。\n例: \"@@ -5,3 +5,3 @@\\n context\\n-旧行\\n+新行\\n context\"".to_string()
            } else {
                match std::fs::read_to_string(&abs) {
                    Err(e) => format!("ERROR: ファイル読み込み失敗: {e}"),
                    Ok(content) => match apply_unified_diff(&content, diff) {
                        Err(e) => crate::executor::errors::tool_error(&e),
                        Ok(patched) => {
                            ctx.checkpoints.save(&abs, &content, "patch");
                            match std::fs::write(&abs, &patched) {
                                Err(e) => format!("ERROR: 書き込み失敗: {e}"),
                                Ok(_) => { ctx.read_files.insert(abs); "OK".to_string() }
                            }
                        },
                    },
                }
            }
        }
    };
    ToolResult::new(format!("Patch({path})"), output)
}
