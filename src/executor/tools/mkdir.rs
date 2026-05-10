use crate::executor::context::ToolContext;
use crate::executor::ToolResult;

pub fn handle(ctx: &ToolContext<'_>, path: &str) -> ToolResult {
    let output = match ctx.resolve(path) {
        Err(e) => crate::executor::errors::tool_error(&e),
        Ok(abs) => match std::fs::create_dir_all(&abs) {
            Ok(_) => "OK".to_string(),
            Err(e) => crate::executor::errors::tool_error(&e),
        },
    };
    ToolResult::new(format!("Mkdir({path})"), output)
}
