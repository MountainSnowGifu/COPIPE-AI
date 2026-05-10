/// PostToolUse hook チェーン
///
/// tool_use-flow.md §9 "runPostToolUse" に相当する。
/// 各 hook は (tool_name, ToolResult) → ToolResult の変換で、
/// 順番に適用される。新しい hook は POST_HOOKS に追加するだけでよい。

use super::ToolResult;

type HookFn = fn(&str, ToolResult) -> ToolResult;

/// 適用する hook の順序リスト
const POST_HOOKS: &[HookFn] = &[
    hook_limit_output,     // 1. ツールごとの出力サイズ制限
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
        "cmd"      => 6_000, // cargo build 等は stdout/stderr が巨大になりやすい
        "read_log" => 8_000, // ログは末尾 32KB 読むが念のため
        _          => 12_000, // その他（read_file はバジェット管理で既に制限済み）
    };

    if result.output.chars().count() > limit {
        let truncated: String = result.output.chars().take(limit).collect();
        // 行の途中で切らない
        let safe_end = truncated.rfind('\n').map(|i| i + 1).unwrap_or(truncated.len());
        let kept = &truncated[..safe_end];
        result.output = format!(
            "{kept}\n[出力が長すぎるため省略しました。{} 文字以降を切り捨て]",
            limit
        );
    }
    result
}

/// OS エラー文 "(os error N)" を人間・AI 向けのメッセージに書き換える
fn hook_rewrite_os_errors(_tool_name: &str, mut result: ToolResult) -> ToolResult {
    if !result.output.starts_with("ERROR:") {
        return result;
    }
    // よくある OS エラーを日本語に置換
    let rewrites: &[(&str, &str)] = &[
        ("(os error 2)",  "（ファイルが見つかりません）"),
        ("(os error 13)", "（権限がありません）"),
        ("(os error 17)", "（すでに存在します）"),
        ("(os error 28)", "（ディスク容量不足）"),
        ("(os error 36)", "（ファイル名が長すぎます）"),
    ];
    for (pattern, replacement) in rewrites {
        result.output = result.output.replace(pattern, replacement);
    }
    result
}

/// 出力中の絶対パスを `~/` や相対パス風に短縮して情報漏洩を減らす
fn hook_redact_abs_paths(_tool_name: &str, mut result: ToolResult) -> ToolResult {
    // HOME ディレクトリを `~` に置換
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            result.output = result.output.replace(&home, "~");
        }
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
        if let Ok(home) = std::env::var("HOME") {
            let output = format!("path: {home}/secret/file.txt");
            let result = run("cmd", r(&output));
            assert!(!result.output.contains(&home));
            assert!(result.output.contains("~/secret/file.txt"));
        }
    }
}
