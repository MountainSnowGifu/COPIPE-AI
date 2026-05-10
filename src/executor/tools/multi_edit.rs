use crate::command::EditPair;
use crate::executor::ToolResult;
use crate::executor::context::ToolContext;

/// 複数の文字列置換を1ファイルにアトミックに適用する
///
/// edit を複数回呼ぶと中間状態が AI に返るが、
/// multi_edit は全置換を一括検証してから書き込む。
///
/// 検証フェーズ:
///   - 各 old_string が1件ずつ存在するか確認
///   - 後続の置換が前の置換に干渉しないか確認
/// 書き込みフェーズ:
///   - 順番に replacen を適用
///   - 全置換完了後に1回だけファイルに書き込む
pub fn handle(ctx: &mut ToolContext<'_>, path: &str, edits: &[EditPair]) -> ToolResult {
    let label = format!("MultiEdit({path})");

    if edits.is_empty() {
        return ToolResult::new(label, "ERROR: edits が空です。");
    }

    let abs = match ctx.resolve(path) {
        Err(e) => return ToolResult::new(label, crate::executor::errors::tool_error(&e)),
        Ok(p) => p,
    };

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

    if !ctx.read_files.contains(&abs) {
        return ToolResult::new(label, crate::executor::errors::unread_file(path));
    }

    if !abs.exists() {
        return ToolResult::new(label, format!("ERROR: '{path}' が存在しません。"));
    }

    let original = match std::fs::read_to_string(&abs) {
        Err(e) => return ToolResult::new(label, format!("ERROR: ファイル読み込み失敗: {e}")),
        Ok(s) => s,
    };

    // CRLF → LF に正規化（Windows で CRLF 保存されたファイルへの対応）
    // AI の old_string は JSON \n エスケープ由来で常に LF のみのため、
    // CRLF ファイルでも old_string がマッチするよう正規化する
    let original = original.replace("\r\n", "\n");

    // ─── 検証フェーズ ────────────────────────────────────────────────────────
    // 実際に適用しながら検証（後続の置換への干渉も検出）
    let mut working = original.clone();
    let mut applied: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for (i, edit) in edits.iter().enumerate() {
        let n = working.matches(&edit.old_string).count();
        match n {
            0 => errors.push(format!(
                "Edit #{}: old_string が見つかりません: {:?}",
                i + 1,
                truncate_preview(&edit.old_string, 60)
            )),
            1 => {
                working = working.replacen(&edit.old_string, &edit.new_string, 1);
                applied.push(format!(
                    "#{}: {:?} → {:?}",
                    i + 1,
                    truncate_preview(&edit.old_string, 40),
                    truncate_preview(&edit.new_string, 40)
                ));
            }
            n => errors.push(format!(
                "Edit #{}: old_string が {n} 箇所マッチしました（曖昧）。前後の行を含めてより長い old_string にしてください",
                i + 1
            )),
        }
    }

    if !errors.is_empty() {
        return ToolResult::new(
            label,
            format!(
                "ERROR: {} 件の置換が失敗しました。修正してください。\n{}",
                errors.len(),
                errors.join("\n")
            ),
        );
    }

    // ─── 書き込みフェーズ（変更前にチェックポイント保存）─────────────────────
    ctx.checkpoints.save(&abs, &original, "multi_edit");
    match std::fs::write(&abs, &working) {
        Err(e) => ToolResult::new(label, format!("ERROR: 書き込み失敗: {e}")),
        Ok(_) => {
            ctx.read_files.insert(abs);
            ToolResult::new(
                label,
                format!(
                    "OK — {} 件の置換を適用しました\n{}",
                    applied.len(),
                    applied.join("\n")
                ),
            )
        }
    }
}

fn truncate_preview(s: &str, max: usize) -> String {
    let first_line = s.lines().next().unwrap_or(s);
    if first_line.chars().count() > max {
        format!("{}…", first_line.chars().take(max).collect::<String>())
    } else {
        first_line.to_string()
    }
}

// ─── テスト ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(src: &str, edits: &[(&str, &str)]) -> Option<String> {
        let mut s = src.to_string();
        for (old, new) in edits {
            let n = s.matches(old).count();
            if n != 1 {
                return None;
            }
            s = s.replacen(old, new, 1);
        }
        Some(s)
    }

    #[test]
    fn test_sequential_apply() {
        let src = "let x = 1;\nlet y = 2;\n";
        let result = apply(
            src,
            &[("let x = 1;", "let x = 10;"), ("let y = 2;", "let y = 20;")],
        );
        assert_eq!(result, Some("let x = 10;\nlet y = 20;\n".to_string()));
    }

    #[test]
    fn test_chained_edit_dependency() {
        // 2番目の置換が1番目の結果に依存するケース
        let src = "fn old() {}";
        let result = apply(
            src,
            &[
                ("fn old()", "fn new()"),
                ("fn new() {}", "fn new() { todo!() }"),
            ],
        );
        assert_eq!(result, Some("fn new() { todo!() }".to_string()));
    }

    #[test]
    fn test_ambiguous_fails() {
        let src = "foo\nfoo\n";
        let result = apply(src, &[("foo", "bar")]);
        assert!(result.is_none()); // 2件マッチで None
    }

    #[test]
    fn test_truncate_preview() {
        let long = "a".repeat(100);
        let preview = truncate_preview(&long, 60);
        assert!(preview.len() <= 64); // 60 chars + "…"
        assert!(preview.ends_with('…'));
    }
}
