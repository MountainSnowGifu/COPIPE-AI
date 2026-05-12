use crate::executor::ToolResult;
use crate::executor::context::ToolContext;
use std::path::{Path, PathBuf};

const MAX_RESULTS: usize = 500;

/// ワイルドカードパターンでファイルを再帰検索する
///
/// 対応パターン:
///   `**`  ゼロ以上の任意パスコンポーネント
///   `*`   パス区切りを除く任意の文字列
///   `?`   スラッシュを除く任意の1文字
///
/// 例:
///   `src/**/*.rs`     src 以下のすべての .rs ファイル
///   `**/*.toml`       プロジェクト全体の .toml ファイル
///   `src/*/mod.rs`    src 直下の各サブディレクトリの mod.rs
pub fn handle(ctx: &ToolContext<'_>, pattern: &str) -> ToolResult {
    if pattern.is_empty() {
        return ToolResult::new("Glob", "ERROR: pattern が空です");
    }

    if !pattern.contains('*') && !pattern.contains('?') {
        return match ctx.resolve(pattern) {
            Err(e) => ToolResult::new(format!("Glob({pattern})"), format!("ERROR: {e}")),
            Ok(path) if path.exists() => {
                let display = display_path(&path, ctx.root);
                ToolResult::new(
                    format!("Glob({pattern})"),
                    format!("{display}\n\n[1 ファイル]"),
                )
            }
            Ok(_) => ToolResult::new(
                format!("Glob({pattern})"),
                format!("マッチなし: '{pattern}' に一致するファイルはありません"),
            ),
        };
    }

    // パターンの非ワイルドカード前置部分を起点ディレクトリとして解決
    let base_str = non_wildcard_prefix(pattern);
    let base = match ctx.resolve(if base_str.is_empty() { "." } else { &base_str }) {
        Err(e) => return ToolResult::new(format!("Glob({pattern})"), format!("ERROR: {e}")),
        Ok(p) => p,
    };

    if !base.exists() {
        return ToolResult::new(
            format!("Glob({pattern})"),
            format!("ERROR: ベースパス '{base_str}' が存在しません"),
        );
    }

    let pattern_tail = pattern
        .strip_prefix(&base_str)
        .unwrap_or(pattern)
        .trim_start_matches(is_path_separator);
    let pattern_parts: Vec<&str> = if pattern_tail.is_empty() {
        Vec::new()
    } else {
        pattern_tail.split(is_path_separator).collect()
    };

    let mut results: Vec<PathBuf> = Vec::new();
    walk(&base, &[], &pattern_parts, ctx.root, &mut results);
    results.sort();

    let truncated = results.len() > MAX_RESULTS;
    let shown = results.len().min(MAX_RESULTS);

    if results.is_empty() {
        return ToolResult::new(
            format!("Glob({pattern})"),
            format!("マッチなし: '{pattern}' に一致するファイルはありません"),
        );
    }

    let lines: Vec<String> = results[..shown]
        .iter()
        .map(|p| display_path(p, ctx.root))
        .collect();

    let mut output = lines.join("\n");
    if truncated {
        output.push_str(&format!(
            "\n\n[最初の {MAX_RESULTS} 件を表示。全 {} 件以上ヒット]",
            results.len()
        ));
    } else {
        output.push_str(&format!(
            "\n\n[{} ファイル。これ以外に '{}' に一致するファイルは存在しません]",
            results.len(),
            pattern
        ));
    }

    ToolResult::new(format!("Glob({pattern})"), output)
}

// ─── ディレクトリ再帰ウォーク ──────────────────────────────────────────────────

/// `current` ディレクトリを `walked` だけ下りた状態で、
/// `pattern_parts` の残りに一致するパスを収集する
fn walk(
    current: &Path,
    walked: &[&str],        // current までに消費したパターン部分
    pattern_parts: &[&str], // まだ消費していないパターン部分
    root: &Path,
    results: &mut Vec<PathBuf>,
) {
    if results.len() >= MAX_RESULTS {
        return;
    }

    // パターンが空 → current 自体がマッチ
    if pattern_parts.is_empty() {
        results.push(current.to_path_buf());
        return;
    }

    let (head, tail) = (&pattern_parts[0], &pattern_parts[1..]);

    if *head == "**" {
        // ** はゼロ以上のコンポーネントに一致
        // 1. ゼロ消費: tail で current を照合
        walk(current, walked, tail, root, results);
        // 2. 一以上消費: current の子ディレクトリへ再帰
        if let Ok(entries) = std::fs::read_dir(current) {
            let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let path = entry.path();
                if path.is_dir() && !is_hidden(&path) && is_inside_root(&path, root) {
                    walk(&path, walked, pattern_parts, root, results);
                }
            }
        }
        return;
    }

    // ** 以外: head と現在ディレクトリの各エントリを照合
    if let Ok(entries) = std::fs::read_dir(current) {
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };
            if !wildcard_match(head, name) {
                continue;
            }
            if tail.is_empty() {
                // パターンを消費しきった → マッチ
                if is_inside_root(&path, root) {
                    results.push(path);
                }
            } else if path.is_dir() && !is_hidden(&path) && is_inside_root(&path, root) {
                // まだパターンが残っている → 子ディレクトリへ
                walk(&path, walked, tail, root, results);
            }
        }
    }
}

// ─── パターンマッチ ───────────────────────────────────────────────────────────

/// `*` と `?` を含む単一コンポーネントの一致判定
/// `*` はパス区切り以外の任意の文字列、`?` は1文字
fn wildcard_match(pattern: &str, s: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = s.chars().collect();
    wildcard_dp(&p, &t, 0, 0)
}

fn wildcard_dp(p: &[char], t: &[char], pi: usize, ti: usize) -> bool {
    if pi == p.len() {
        return ti == t.len();
    }
    if p[pi] == '*' {
        // * はゼロ以上の文字に一致（/ を除く）
        if wildcard_dp(p, t, pi + 1, ti) {
            return true;
        }
        if ti < t.len() && !is_path_separator(t[ti]) {
            return wildcard_dp(p, t, pi, ti + 1);
        }
        return false;
    }
    if ti == t.len() {
        return false;
    }
    if p[pi] == '?' || p[pi] == t[ti] {
        return wildcard_dp(p, t, pi + 1, ti + 1);
    }
    false
}

// ─── ユーティリティ ───────────────────────────────────────────────────────────

/// パターンの最初のワイルドカード文字より前の部分を返す
fn non_wildcard_prefix(pattern: &str) -> String {
    let idx = pattern
        .find(|c| matches!(c, '*' | '?'))
        .unwrap_or(pattern.len());
    let prefix = &pattern[..idx];
    prefix.trim_end_matches(is_path_separator).to_string()
}

fn is_path_separator(c: char) -> bool {
    c == '/' || c == '\\'
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with('.'))
        .unwrap_or(false)
}

fn is_inside_root(path: &Path, root: &Path) -> bool {
    let canon_path = match path.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };
    // root も canonicalize して拡張パス形式（\\?\C:\...）を揃える
    // 揃えないと Windows で starts_with が常に false になり glob が全件スキップする
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    canon_path.starts_with(canon_root)
}

fn display_path(path: &Path, root: &Path) -> String {
    let rel = path.strip_prefix(root).ok().or_else(|| {
        root.canonicalize()
            .ok()
            .and_then(|canon_root| path.strip_prefix(canon_root).ok())
    });
    rel.unwrap_or(path).display().to_string().replace('\\', "/")
}

// ─── テスト ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wildcard_match_star() {
        assert!(wildcard_match("*.rs", "main.rs"));
        assert!(wildcard_match("*.rs", "mod.rs"));
        assert!(!wildcard_match("*.rs", "main.txt"));
        assert!(!wildcard_match("*.rs", "a/b.rs"));
        assert!(!wildcard_match("*.rs", r"a\b.rs"));
    }

    #[test]
    fn test_wildcard_match_question() {
        assert!(wildcard_match("mo?.rs", "mod.rs"));
        assert!(!wildcard_match("mo?.rs", "mode.rs"));
    }

    #[test]
    fn test_wildcard_match_exact() {
        assert!(wildcard_match("mod.rs", "mod.rs"));
        assert!(!wildcard_match("mod.rs", "main.rs"));
    }

    #[test]
    fn test_non_wildcard_prefix() {
        assert_eq!(non_wildcard_prefix("src/**/*.rs"), "src");
        assert_eq!(non_wildcard_prefix(r"src\**\*.rs"), "src");
        assert_eq!(non_wildcard_prefix("**/*.toml"), "");
        assert_eq!(non_wildcard_prefix("src/main.rs"), "src/main.rs");
    }

    #[test]
    fn test_handle_src_recursive_pattern() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/agent")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "").unwrap();
        std::fs::write(dir.path().join("src/agent/mod.rs"), "").unwrap();
        std::fs::write(dir.path().join("src/agent/readme.md"), "").unwrap();

        let mut read_files = std::collections::HashSet::new();
        let mut checkpoints = crate::executor::CheckpointManager::new(dir.path());
        let ctx = crate::executor::ToolContext::new(dir.path(), &mut read_files, &mut checkpoints);

        let result = handle(&ctx, "src/**/*.rs");

        assert!(result.output.contains("src/main.rs"));
        assert!(result.output.contains("src/agent/mod.rs"));
        assert!(!result.output.contains("readme.md"));
    }

    #[test]
    fn test_handle_windows_separator_pattern() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/agent")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "").unwrap();
        std::fs::write(dir.path().join("src/agent/mod.rs"), "").unwrap();
        std::fs::write(dir.path().join("src/agent/readme.md"), "").unwrap();

        let mut read_files = std::collections::HashSet::new();
        let mut checkpoints = crate::executor::CheckpointManager::new(dir.path());
        let ctx = crate::executor::ToolContext::new(dir.path(), &mut read_files, &mut checkpoints);

        let result = handle(&ctx, r"src\**\*.rs");

        assert!(result.output.contains("src/main.rs") || result.output.contains(r"src\main.rs"));
        assert!(
            result.output.contains("src/agent/mod.rs")
                || result.output.contains(r"src\agent\mod.rs")
        );
        assert!(!result.output.contains("readme.md"));
    }

    #[test]
    fn test_handle_outputs_forward_slashes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/agent")).unwrap();
        std::fs::write(dir.path().join("src/agent/mod.rs"), "").unwrap();

        let mut read_files = std::collections::HashSet::new();
        let mut checkpoints = crate::executor::CheckpointManager::new(dir.path());
        let ctx = crate::executor::ToolContext::new(dir.path(), &mut read_files, &mut checkpoints);

        let result = handle(&ctx, r"src\**\*.rs");

        assert!(result.output.contains("src/agent/mod.rs"));
        assert!(!result.output.contains(r"src\agent\mod.rs"));
    }
}
