use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// 毎ターンのプロンプトに付加するコンテキストヘッダー。
/// AI が「何を読んだか・何をしたか・元のタスクは何か」を忘れないようにする。
pub(super) fn build_context_header(
    user_task: &str,
    read_files: &HashSet<PathBuf>,
    root: &Path,
    done_log: &[String],
) -> String {
    let mut lines = vec![format!("[元のタスク] {user_task}")];

    if !read_files.is_empty() {
        let mut files: Vec<String> = read_files
            .iter()
            .filter_map(|p| p.strip_prefix(root).ok())
            .map(|p| p.display().to_string())
            .collect();
        files.sort();
        let total = files.len();
        let shown: Vec<String> = files.iter().take(10).cloned().collect();
        if total > 10 {
            lines.push(format!(
                "[読み込み済みファイル（再読み不要）] {} … ({} 件省略)",
                shown.join(", "),
                total - 10
            ));
        } else {
            lines.push(format!(
                "[読み込み済みファイル（再読み不要）] {}",
                shown.join(", ")
            ));
        }
    }

    if !done_log.is_empty() {
        // 直近 8 件だけ表示（プロンプトを膨らませすぎない）
        let recent: Vec<&str> = done_log
            .iter()
            .rev()
            .take(8)
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        lines.push(format!("[完了済みアクション] {}", recent.join(" → ")));
    }

    lines.join("\n")
}

pub(super) fn summarize_for_display(label: &str, output: &str) -> String {
    if output.starts_with("```") {
        let n = output.lines().count().saturating_sub(2);
        return format!("{n}行");
    }
    if label.starts_with("ListDir(") {
        let n = output.lines().filter(|l| !l.is_empty()).count();
        return format!("{n}エントリ");
    }
    let first = output.lines().next().unwrap_or("").trim();
    let chars: String = first.chars().take(120).collect();
    if first.chars().count() > 120 {
        format!("{chars}…")
    } else {
        chars
    }
}
