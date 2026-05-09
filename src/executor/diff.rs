/// `@@ -old_start[,old_count] +new_start[,new_count] @@` をパースして
/// (old_start_1indexed, old_line_count) を返す
fn parse_hunk_header(line: &str) -> Result<(usize, usize), String> {
    let inner = line
        .trim_start_matches('@')
        .split("@@")
        .next()
        .unwrap_or("")
        .trim();

    let parts: Vec<&str> = inner.split_whitespace().collect();
    if parts.len() < 2 || !parts[0].starts_with('-') {
        return Err(format!("不正なハンクヘッダー: `{line}`"));
    }

    let old_spec = &parts[0][1..];
    let (start, count) = if let Some((s, c)) = old_spec.split_once(',') {
        let s: usize = s.parse().map_err(|_| format!("行番号解析エラー: `{s}`"))?;
        let c: usize = c.parse().map_err(|_| format!("行数解析エラー: `{c}`"))?;
        (s, c)
    } else {
        let s: usize = old_spec
            .parse()
            .map_err(|_| format!("行番号解析エラー: `{old_spec}`"))?;
        (s, 1)
    };

    Ok((start, count))
}

/// 標準 unified diff を `content` に適用して新しい文字列を返す
pub fn apply_unified_diff(content: &str, diff: &str) -> Result<String, String> {
    let trailing_newline = content.ends_with('\n');
    let mut lines: Vec<String> = content.split('\n').map(|s| s.to_string()).collect();
    if trailing_newline {
        lines.pop();
    }

    let diff_lines: Vec<&str> = diff.lines().collect();
    let mut di = 0usize;
    let mut offset: i64 = 0;

    while di < diff_lines.len() {
        let dl = diff_lines[di];
        if !dl.starts_with("@@") {
            di += 1;
            continue;
        }

        let (old_start, _) = parse_hunk_header(dl)?;
        di += 1;

        // ハンク行を収集（次の @@ または末尾まで）
        let mut hunk: Vec<(char, String)> = Vec::new();
        while di < diff_lines.len() && !diff_lines[di].starts_with("@@") {
            let hl = diff_lines[di];
            // 空行は末尾の \n によるアーティファクトなのでスキップ
            if hl.is_empty() {
                di += 1;
                continue;
            }
            let marker = hl.chars().next().unwrap_or(' ');
            let body = if hl.len() > 1 {
                hl[1..].to_string()
            } else {
                String::new()
            };
            if matches!(marker, ' ' | '-' | '+') {
                hunk.push((marker, body));
            }
            di += 1;
        }

        let apply_at = (old_start as i64 - 1 + offset).max(0) as usize;
        let old_count = hunk
            .iter()
            .filter(|(m, _)| matches!(m, ' ' | '-'))
            .count();

        if apply_at + old_count > lines.len() {
            return Err(format!(
                "パッチ適用失敗: 行 {apply_at}+1 から {old_count} 行を置換できません（ファイルは {} 行）",
                lines.len()
            ));
        }

        // コンテキスト行の一致を検証（末尾スペース差異は許容）
        let mut old_idx = apply_at;
        for (marker, expected) in &hunk {
            if matches!(marker, ' ' | '-') {
                let actual = lines
                    .get(old_idx)
                    .map(|s| s.as_str())
                    .unwrap_or("<ファイル終端>");
                let matches =
                    actual == expected.as_str() || actual.trim_end() == expected.trim_end();
                if !matches {
                    // 周辺行を表示して AI が正しいコンテキストを再生成しやすくする
                    let ctx_start = old_idx.saturating_sub(2);
                    let ctx_end = (old_idx + 3).min(lines.len());
                    let ctx: Vec<String> = lines[ctx_start..ctx_end]
                        .iter()
                        .enumerate()
                        .map(|(i, l)| {
                            let lineno = ctx_start + i + 1;
                            let marker = if lineno == old_idx + 1 { ">" } else { " " };
                            format!("  {marker} {:3}: {l}", lineno)
                        })
                        .collect();
                    return Err(format!(
                        "パッチ適用失敗: 行 {} のコンテキストが一致しません\n  期待: {:?}\n  実際: {:?}\n実ファイルの周辺行:\n{}",
                        old_idx + 1,
                        expected,
                        actual,
                        ctx.join("\n")
                    ));
                }
                old_idx += 1;
            }
        }

        let new_lines: Vec<String> = hunk
            .iter()
            .filter(|(m, _)| matches!(m, ' ' | '+'))
            .map(|(_, c)| c.clone())
            .collect();

        let added = hunk.iter().filter(|(m, _)| *m == '+').count() as i64;
        let removed = hunk.iter().filter(|(m, _)| *m == '-').count() as i64;
        lines.splice(apply_at..apply_at + old_count, new_lines);
        offset += added - removed;
    }

    let mut result = lines.join("\n");
    if trailing_newline {
        result.push('\n');
    }
    Ok(result)
}
