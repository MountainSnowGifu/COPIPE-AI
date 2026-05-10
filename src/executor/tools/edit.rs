use crate::executor::ToolResult;
use crate::executor::context::ToolContext;

/// ファイル内の文字列を完全一致で1箇所だけ置換する
///
/// - old_string が0件 → エラー
/// - old_string が2件以上 → エラー（曖昧。より多くのコンテキストを含めるよう案内）
/// - ちょうど1件 → 置換して書き込み
///
/// patch と違い行番号・diff 形式が不要なため AI が失敗しにくい。
pub fn handle(
    ctx: &mut ToolContext<'_>,
    path: &str,
    old_string: &str,
    new_string: &str,
) -> ToolResult {
    // ctx は checkpoints アクセスのために mut が必要
    let label = format!("Edit({path})");

    if old_string.is_empty() {
        return ToolResult::new(
            label,
            "ERROR: old_string が空です。置換したい文字列を指定してください。",
        );
    }

    let abs = match ctx.resolve(path) {
        Err(e) => return ToolResult::new(label, crate::executor::errors::tool_error(&e)),
        Ok(p) => p,
    };

    // symlink チェック
    if abs
        .symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        return ToolResult::new(
            label,
            crate::executor::errors::perm_denied(format!("'{path}' はシンボリックリンクです")),
        );
    }

    // 事前 read_file チェック
    if !ctx.read_files.contains(&abs) {
        return ToolResult::new(label, crate::executor::errors::unread_file(path));
    }

    if !abs.exists() {
        return ToolResult::new(label, format!("ERROR: '{path}' が存在しません。"));
    }

    let content = match std::fs::read_to_string(&abs) {
        Err(e) => return ToolResult::new(label, format!("ERROR: ファイル読み込み失敗: {e}")),
        Ok(s) => s,
    };

    // 出現回数をカウント
    let count = content.matches(old_string).count();

    let output = match count {
        0 => {
            // 行番号情報を添えて原因を診断しやすくする
            let similar = find_similar_lines(&content, old_string);
            let hint = if similar.is_empty() {
                String::new()
            } else {
                format!("\n\n近い行（参考）:\n{similar}")
            };
            format!(
                "ERROR: old_string がファイル内に見つかりませんでした。\n\
                read_file で現在の内容を確認し、old_string を正確にコピーしてください。{hint}"
            )
        }
        1 => {
            // ちょうど1件 → 置換（変更前にチェックポイント保存）
            ctx.checkpoints.save(&abs, &content, "edit");
            let patched = content.replacen(old_string, new_string, 1);
            match std::fs::write(&abs, &patched) {
                Err(e) => format!("ERROR: 書き込み失敗: {e}"),
                Ok(_) => {
                    ctx.read_files.insert(abs);
                    "OK".to_string()
                }
            }
        }
        n => {
            // 2件以上 → 曖昧なので拒否
            format!(
                "ERROR: old_string がファイル内に {n} 箇所マッチしました。\n\
                どの箇所を変更するか特定できないため、より多くのコンテキスト（前後の行）を\
                old_string に含めて再度試してください。"
            )
        }
    };

    ToolResult::new(label, output)
}

/// old_string と近い行を探してデバッグヒントとして返す（先頭3行まで）
fn find_similar_lines<'a>(content: &'a str, old_string: &str) -> String {
    // old_string の最初の単語で部分一致する行を探す
    let keyword = old_string.split_whitespace().next().unwrap_or("");
    if keyword.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = content
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains(keyword))
        .take(3)
        .map(|(i, l)| format!("  {:4}: {}", i + 1, l.trim()))
        .collect();
    lines.join("\n")
}

// ─── テスト ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn content_after_edit(src: &str, old: &str, new: &str) -> Option<String> {
        if src.matches(old).count() == 1 {
            Some(src.replacen(old, new, 1))
        } else {
            None
        }
    }

    #[test]
    fn test_single_match_replaces() {
        let src = "let x = 1;\nlet y = 2;\n";
        let result = content_after_edit(src, "let x = 1;", "let x = 42;");
        assert_eq!(result, Some("let x = 42;\nlet y = 2;\n".to_string()));
    }

    #[test]
    fn test_zero_match_returns_none() {
        let src = "let x = 1;\n";
        // マッチなし → count==0 → content_after_edit は None を返す
        assert_eq!(src.matches("let z = 999;").count(), 0);
        let result = content_after_edit(src, "let z = 999;", "");
        assert!(result.is_none());
    }

    #[test]
    fn test_multiple_matches_returns_none() {
        let src = "foo\nfoo\n";
        assert_eq!(src.matches("foo").count(), 2);
        let result = content_after_edit(src, "foo", "bar");
        assert!(result.is_none());
    }

    #[test]
    fn test_multiline_old_string() {
        let src = "fn hello() {\n    println!(\"hi\");\n}\n";
        let old = "fn hello() {\n    println!(\"hi\");\n}";
        let new = "fn hello() {\n    println!(\"hello!\");\n}";
        let result = content_after_edit(src, old, new);
        assert!(result.is_some());
        assert!(result.unwrap().contains("hello!"));
    }
}
