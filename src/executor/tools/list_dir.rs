use crate::executor::ToolResult;
use crate::executor::context::ToolContext;

pub fn handle(ctx: &ToolContext<'_>, path: &str) -> ToolResult {
    let output = match ctx.resolve(path) {
        Err(e) => crate::executor::errors::tool_error(&e),
        Ok(abs) => match std::fs::read_dir(&abs) {
            Err(e) => crate::executor::errors::tool_error(&e),
            Ok(entries) => {
                let mut lines: Vec<String> = entries
                    .filter_map(|e| e.ok())
                    .map(|e| {
                        let name = e.file_name().to_string_lossy().into_owned();
                        if e.path().is_dir() {
                            format!("{name}/")
                        } else {
                            name
                        }
                    })
                    .collect();
                lines.sort();
                lines.join("\n")
            }
        },
    };
    ToolResult::new(format!("ListDir({path})"), output)
}
