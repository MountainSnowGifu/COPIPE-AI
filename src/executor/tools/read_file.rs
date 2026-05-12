use crate::executor::ToolResult;
use crate::executor::context::ToolContext;

pub fn handle(ctx: &mut ToolContext<'_>, path: &str, offset_lines: usize) -> ToolResult {
    if ctx.turn_read_chars >= ctx.max_turn_read {
        return ToolResult::new(
            format!("ReadFile({path})"),
            format!(
                "このターンの読み込みバジェット ({} 文字) を超えました。次のターンで読んでください。",
                ctx.max_turn_read
            ),
        );
    }

    let output = match ctx.resolve(path) {
        Err(e) => crate::executor::errors::tool_error(&e),
        Ok(abs) => match std::fs::read_to_string(&abs) {
            Err(_) if !abs.exists() => {
                let hint = abs
                    .parent()
                    .and_then(|p| std::fs::read_dir(p).ok())
                    .map(|entries| {
                        let mut names: Vec<String> = entries
                            .filter_map(|e| e.ok())
                            .map(|e| e.file_name().to_string_lossy().to_string())
                            .collect();
                        names.sort();
                        format!(" 同ディレクトリの実在ファイル: {}", names.join(", "))
                    })
                    .unwrap_or_default();
                format!(
                    "ERROR: ファイルが存在しません: '{path}'.{hint}\n\
                    glob でプロジェクト構成を確認してから read_file してください: \
                    {{\"type\":\"glob\",\"pattern\":\"**/*.rs\"}}"
                )
            }
            Ok(content) => {
                let budget = ctx.max_turn_read.saturating_sub(ctx.turn_read_chars);
                let total_lines = content.lines().count();
                let sliced: String = if offset_lines > 0 {
                    content
                        .lines()
                        .skip(offset_lines)
                        .collect::<Vec<_>>()
                        .join("\n")
                } else {
                    content.clone()
                };
                let sliced_lines = total_lines.saturating_sub(offset_lines);

                let out = if sliced.chars().count() > budget {
                    // 行の途中で切らず最後の完全な改行位置で切り詰める
                    let char_budget: String = sliced.chars().take(budget).collect();
                    let safe_end = char_budget
                        .rfind('\n')
                        .map(|i| i + 1)
                        .unwrap_or(char_budget.len());
                    let truncated = &char_budget[..safe_end];
                    let shown_lines = truncated.lines().count();
                    let remaining = sliced_lines.saturating_sub(shown_lines);
                    let next_offset = offset_lines + shown_lines;
                    format!(
                        "```\n{truncated}\n```\n[残り {remaining} 行。続きは {{\"type\":\"read_file\",\"path\":\"{path}\",\"offset_lines\":{next_offset}}} で取得]"
                    )
                } else {
                    ctx.read_files.insert(abs);
                    if offset_lines > 0 {
                        format!(
                            "```\n{sliced}\n```\n[{offset_lines} 行目以降を表示（全 {total_lines} 行）]"
                        )
                    } else {
                        format!("```\n{sliced}\n```")
                    }
                };
                ctx.turn_read_chars += out.chars().count();
                out
            }
            Err(e) => crate::executor::errors::tool_error(&e),
        },
    };

    ToolResult::new(
        if offset_lines == 0 {
            format!("ReadFile({path})")
        } else {
            format!("ReadFile({path}@{offset_lines})")
        },
        output,
    )
}
