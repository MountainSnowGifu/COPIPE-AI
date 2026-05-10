/// `@@ -old_start[,old_count] +new_start[,new_count] @@` をパースして
/// (old_start_1indexed, declared_old_count) を返す。
/// 行番号が省略された `@@` の場合は (0, 0) を返す（コンテキスト検索で補完する）。
fn parse_hunk_header(line: &str) -> Result<(usize, usize), String> {
    let inner = line
        .trim_start_matches('@')
        .split("@@")
        .next()
        .unwrap_or("")
        .trim();

    // `@@` だけで行番号なし → コンテキスト検索で補完
    if inner.is_empty() {
        return Ok((0, 0));
    }

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

/// ハンクのコンテキスト行・削除行でファイル内を検索して適用開始位置 (0-indexed) を返す
fn find_hunk_position(lines: &[String], hunk: &[(char, String)]) -> Option<usize> {
    let needle: Vec<&str> = hunk
        .iter()
        .filter(|(m, _)| matches!(m, ' ' | '-'))
        .map(|(_, s)| s.as_str())
        .collect();

    if needle.is_empty() {
        return Some(0);
    }

    'outer: for start in 0..=lines.len().saturating_sub(needle.len()) {
        for (i, expected) in needle.iter().enumerate() {
            let actual = lines.get(start + i).map(|s| s.as_str()).unwrap_or("");
            if actual.trim_end() != expected.trim_end() {
                continue 'outer;
            }
        }
        return Some(start);
    }
    None
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

        let (old_start, declared_old_count) = parse_hunk_header(dl)?;
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

        let old_count = hunk.iter().filter(|(m, _)| matches!(m, ' ' | '-')).count();

        // ハンクヘッダーの行数宣言と実際のハンク内容が矛盾する場合は拒否
        // old_count == 0 は行番号省略の @@ なので検証をスキップ
        if declared_old_count > 0 && old_count != declared_old_count {
            return Err(format!(
                "diff ヘッダーの行数宣言 ({declared_old_count}) と実際のハンク内容 ({old_count} 行) が一致しません"
            ));
        }

        // old_start == 0 は行番号省略の @@ → コンテキスト検索で位置を特定
        let apply_at = if old_start == 0 {
            match find_hunk_position(&lines, &hunk) {
                Some(pos) => pos,
                None => {
                    let needle_preview: Vec<&str> = hunk
                        .iter()
                        .filter(|(m, _)| matches!(m, ' ' | '-'))
                        .take(3)
                        .map(|(_, s)| s.as_str())
                        .collect();
                    return Err(format!(
                        "パッチ適用失敗: コンテキスト行がファイル内に見つかりません\n  検索行: {:?}",
                        needle_preview
                    ));
                }
            }
        } else {
            (old_start as i64 - 1 + offset).max(0) as usize
        };

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
