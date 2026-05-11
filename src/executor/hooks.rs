/// PostToolUse hook チェーン
///
/// tool_use-flow.md §9 "runPostToolUse" に相当する。
/// 各 hook は (tool_name, ToolResult) → ToolResult の変換で、
/// 順番に適用される。新しい hook は POST_HOOKS に追加するだけでよい。
use super::ToolResult;

type HookFn = fn(&str, ToolResult) -> ToolResult;

/// 適用する hook の順序リスト
const POST_HOOKS: &[HookFn] = &[
    hook_limit_output,      // 1. ツールごとの出力サイズ制限
    hook_rewrite_os_errors, // 2. OS エラー文を AI 向けに書き換え
    hook_redact_abs_paths,  // 3. 絶対パスを相対パス風に短縮
];

/// すべての PostToolUse hook を順番に適用する
pub fn run(tool_name: &str, result: ToolResult) -> ToolResult {
    POST_HOOKS.iter().fold(result, |r, hook| hook(tool_name, r))
}

// ─── Hook 実装 ────────────────────────────────────────────────────────────────

/// ツールごとの出力文字数上限
/// format_tool_results の「合計」制限とは別に「1件あたり」を制限する
fn hook_limit_output(tool_name: &str, mut result: ToolResult) -> ToolResult {
    let limit = match tool_name {
        "cmd" => 8_000,      // cargo build 等は stdout/stderr が巨大になりやすい
        "read_log" => 8_000, // ログは末尾 32KB 読むが念のため
        _ => 12_000,         // その他（read_file はバジェット管理で既に制限済み）
    };

    if result.output.chars().count() > limit {
        let truncated: String = result.output.chars().take(limit).collect();
        // 行の途中で切らない
        let safe_end = truncated
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(truncated.len());
        let kept = &truncated[..safe_end];
        let continuation = if tool_name == "cmd" {
            "\n続きの出力は次で確認できます:\n```json\n{\"type\":\"read_log\",\"filename\":\"cmd_log\"}\n```".to_string()
        } else if tool_name == "read_log" {
            // label 例: "ReadLog(cmd_log)" or "ReadLog(cmd_log@50)"
            let next_hint = read_log_continuation_hint(&result.label, kept);
            format!("\n{next_hint}")
        } else {
            String::new()
        };
        result.output =
            format!("{kept}\n[出力が {limit} 文字を超えたため省略しました]{continuation}");
    }
    result
}

/// ReadLog の label から filename と現在の offset を取り出し、次の継続ヒントを生成する
fn read_log_continuation_hint(label: &str, kept: &str) -> String {
    // label: "ReadLog(cmd_log)" or "ReadLog(cmd_log@50)"
    let inner = label
        .strip_prefix("ReadLog(")
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or("");
    let (filename, base_offset) = match inner.rsplit_once('@') {
        Some((f, off)) if off.chars().all(|c| c.is_ascii_digit()) => {
            (f, off.parse::<usize>().unwrap_or(0))
        }
        _ => (inner, 0),
    };
    if filename.is_empty() {
        return String::new();
    }
    let shown_lines = kept.lines().count();
    let next_offset = base_offset + shown_lines;
    format!(
        "続きは次で確認できます:\n```json\n{{\"type\":\"read_log\",\"filename\":\"{filename}\",\"offset_lines\":{next_offset}}}\n```"
    )
}

/// OS エラー文 "(os error N)" を人間・AI 向けのメッセージに書き換える
fn hook_rewrite_os_errors(_tool_name: &str, mut result: ToolResult) -> ToolResult {
    if !result.output.starts_with("ERROR:") {
        return result;
    }
    // よくある OS エラーを日本語に置換
    let rewrites: &[(&str, &str)] = &[
        ("(os error 2)", "（ファイルが見つかりません）"),
        ("(os error 3)", "（パスが見つかりません）"), // Windows: ERROR_PATH_NOT_FOUND
        ("(os error 5)", "（権限がありません）"),     // Windows: ERROR_ACCESS_DENIED
        ("(os error 13)", "（権限がありません）"),
        ("(os error 17)", "（すでに存在します）"),
        ("(os error 28)", "（ディスク容量不足）"),
        ("(os error 32)", "（ファイルが使用中です）"), // Windows: ERROR_SHARING_VIOLATION
        ("(os error 36)", "（ファイル名が長すぎます）"),
        ("(os error 112)", "（ディスク容量不足）"), // Windows: ERROR_DISK_FULL
        ("(os error 183)", "（すでに存在します）"), // Windows: ERROR_ALREADY_EXISTS
        ("(os error 206)", "（ファイル名が長すぎます）"), // Windows: ERROR_FILENAME_EXCED_RANGE
    ];
    for (pattern, replacement) in rewrites {
        result.output = result.output.replace(pattern, replacement);
    }
    result
}

/// 出力中の絶対パスを `~/` や相対パス風に短縮して情報漏洩を減らす
fn hook_redact_abs_paths(_tool_name: &str, mut result: ToolResult) -> ToolResult {
    // HOME ディレクトリを `~` に置換
    if let Some(home) = crate::paths::home_dir() {
        result.output = result.output.replace(&home.display().to_string(), "~");
    }
    result
}

// ─── テスト ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::ToolResult;

    fn r(output: &str) -> ToolResult {
        ToolResult::new("Test", output)
    }

    #[test]
    fn test_os_error_rewrite() {
        let result = run("cmd", r("ERROR: 読み込み失敗 (os error 2)"));
        assert!(result.output.contains("ファイルが見つかりません"));
    }

    #[test]
    fn test_limit_output_cmd() {
        let long = "x\n".repeat(5_000);
        let result = run("cmd", r(&long));
        assert!(result.output.len() < long.len());
        assert!(result.output.contains("省略しました"));
    }

    #[test]
    fn test_no_truncation_when_short() {
        let result = run("read_file", r("short output"));
        assert_eq!(result.output, "short output");
    }

    #[test]
    fn test_home_redaction() {
        if let Some(home) = crate::paths::home_dir() {
            let home = home.display().to_string();
            let output = format!("path: {home}/secret/file.txt");
            let result = run("cmd", r(&output));
            assert!(!result.output.contains(&home));
            assert!(result.output.contains("~/secret/file.txt"));
        }
    }

    #[test]
    fn test_read_log_truncation_adds_continuation_hint() {
        let long = "line\n".repeat(3_000); // 5*3000=15000 chars > 8000 limit
        let mut result = ToolResult::new("ReadLog(cmd_log)", long.as_str());
        result.label = "ReadLog(cmd_log)".to_string();
        let result = run("read_log", result);
        assert!(result.output.contains("省略しました"));
        assert!(result.output.contains("\"type\":\"read_log\""));
        assert!(result.output.contains("\"filename\":\"cmd_log\""));
        assert!(result.output.contains("\"offset_lines\":"));
    }

    #[test]
    fn test_read_log_offset_continuation_hint_accumulates() {
        // offset 50 から読み始めた ReadLog が切り詰められた場合、次の offset は 50+shown になる
        let long = "line\n".repeat(3_000);
        let mut result = ToolResult::new("ReadLog(cmd_log@50)", long.as_str());
        result.label = "ReadLog(cmd_log@50)".to_string();
        let result = run("read_log", result);
        let offset: usize = result.output
            .split("\"offset_lines\":")
            .nth(1)
            .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|s| s.parse().ok())
            .expect("offset_lines が含まれるべき");
        assert!(offset > 50, "offset は base(50) + shown_lines より大きいはず");
    }
}
