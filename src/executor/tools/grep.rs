use crate::executor::context::ToolContext;
use crate::executor::ToolResult;
use std::path::Path;

/// ファイルまたはディレクトリを再帰的に検索して pattern に一致する行を返す
pub fn handle(
    ctx: &ToolContext<'_>,
    pattern: &str,
    path: &str,
    context_lines: usize,
    file_glob: &Option<String>,
) -> ToolResult {
    if pattern.is_empty() {
        return ToolResult::new(format!("Grep({pattern})"), "ERROR: pattern が空です".to_string());
    }

    let abs = match ctx.resolve(path) {
        Err(e) => return ToolResult::new(format!("Grep({pattern})"), crate::executor::errors::tool_error(&e)),
        Ok(p) => p,
    };

    let mut matches: Vec<String> = Vec::new();
    let mut total_matches = 0usize;
    const MAX_MATCHES: usize = 50;

    let glob_ext = file_glob.as_deref();

    if abs.is_file() {
        search_file(&abs, pattern, context_lines, &mut matches, &mut total_matches, MAX_MATCHES);
    } else if abs.is_dir() {
        collect_files(&abs, glob_ext, &mut |file| {
            if total_matches < MAX_MATCHES {
                search_file(file, pattern, context_lines, &mut matches, &mut total_matches, MAX_MATCHES);
            }
        });
    }

    let output = if matches.is_empty() {
        format!("マッチなし: '{pattern}' は {path} 内に見つかりませんでした")
    } else {
        let mut out = matches.join("\n");
        if total_matches > MAX_MATCHES {
            out.push_str(&format!(
                "\n\n[最初の {MAX_MATCHES} 件を表示。全 {total_matches} 件以上のマッチがあります]"
            ));
        } else {
            out.push_str(&format!("\n\n[{total_matches} 件のマッチ]"));
        }
        out
    };

    ToolResult::new(format!("Grep({pattern} in {path})"), output)
}

/// 1ファイルを検索してマッチ行をコンテキスト付きで追加
fn search_file(
    path: &Path,
    pattern: &str,
    context_lines: usize,
    matches: &mut Vec<String>,
    total: &mut usize,
    max: usize,
) {
    // バイナリファイルをスキップ（先頭512バイトに null が含まれていれば）
    if let Ok(head) = read_bytes(path, 512) {
        if head.contains(&0u8) {
            return;
        }
    } else {
        return;
    }

    // ファイルサイズ上限（5MB）
    if path.metadata().map(|m| m.len()).unwrap_or(0) > 5 * 1024 * 1024 {
        return;
    }

    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return,
    };

    let lines: Vec<&str> = content.lines().collect();
    let mut file_header_added = false;

    for (i, line) in lines.iter().enumerate() {
        if !line.contains(pattern) {
            continue;
        }
        *total += 1;
        if *total > max {
            continue; // カウントは続ける
        }

        if !file_header_added {
            matches.push(format!("\n── {} ──", path.display()));
            file_header_added = true;
        }

        let start = i.saturating_sub(context_lines);
        let end = (i + context_lines + 1).min(lines.len());

        for (j, ctx_line) in lines[start..end].iter().enumerate() {
            let lineno = start + j + 1;
            let marker = if start + j == i { ">" } else { " " };
            matches.push(format!("{marker}{lineno:4}: {ctx_line}"));
        }

        if context_lines > 0 && i + context_lines + 1 < lines.len() {
            matches.push("      ···".to_string());
        }
    }
}

/// ディレクトリを再帰的に走査してテキストファイルを収集
fn collect_files<F: FnMut(&Path)>(dir: &Path, glob_ext: Option<&str>, callback: &mut F) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            // .git などの隠しディレクトリはスキップ
            if path.file_name().and_then(|n| n.to_str()).map(|n| n.starts_with('.')).unwrap_or(false) {
                continue;
            }
            collect_files(&path, glob_ext, callback);
        } else if path.is_file() {
            if let Some(ext_filter) = glob_ext {
                // "*.rs" → ".rs" として末尾一致
                let want = ext_filter.trim_start_matches('*');
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !name.ends_with(want) {
                    continue;
                }
            }
            callback(&path);
        }
    }
}

fn read_bytes(path: &Path, n: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; n];
    let read = f.read(&mut buf)?;
    buf.truncate(read);
    Ok(buf)
}

// ─── テスト ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_file_finds_pattern() {
        let dir = std::env::temp_dir();
        let file = dir.join("grep_test.txt");
        std::fs::write(&file, "line one\nfn hello() {}\nline three\n").unwrap();

        let mut matches = Vec::new();
        let mut total = 0;
        search_file(&file, "fn hello", 0, &mut matches, &mut total, 50);
        assert_eq!(total, 1);
        assert!(matches.iter().any(|m| m.contains("fn hello")));
        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn test_search_file_no_match() {
        let dir = std::env::temp_dir();
        let file = dir.join("grep_test_no.txt");
        std::fs::write(&file, "nothing here\n").unwrap();

        let mut matches = Vec::new();
        let mut total = 0;
        search_file(&file, "xyz_not_found", 0, &mut matches, &mut total, 50);
        assert_eq!(total, 0);
        std::fs::remove_file(&file).ok();
    }
}
