use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

// ─── 圧縮閾値（core-internals.md §5 context-manager.mjs を参考） ──────────────

/// この件数以下は全件表示
const MICRO_THRESHOLD: usize = 5;
/// この件数を超えたらフル圧縮（直近3件のみ）
const FULL_THRESHOLD: usize = 15;
/// マイクロ圧縮で表示する直近件数
const RECENT_MICRO: usize = 5;
/// フル圧縮で表示する直近件数
const RECENT_FULL: usize = 3;
/// ファイルリストをディレクトリ集約する閾値
const FILE_LIST_THRESHOLD: usize = 8;

// ─── メイン ──────────────────────────────────────────────────────────────────

/// 毎ターンのプロンプトに付加するコンテキストヘッダー。
/// done_log が増えるほど古い部分を自動圧縮して Copilot のコンテキスト消費を抑制する。
pub(super) fn build_context_header(
    user_task: &str,
    read_files: &HashSet<PathBuf>,
    root: &Path,
    done_log: &[String],
) -> String {
    // 静的部分（タスクはセッション中固定）
    let static_ctx = format!("## Static context\n[元のタスク] {user_task}");

    // 動的部分（毎ターン更新）— system_prompt.md §「動的コンテキストの形式」に準拠
    let mut dynamic_lines: Vec<String> = Vec::new();
    // 警告は先に収集して先頭に挿入する（AI が見落とさないよう優先表示）
    let mut warnings: Vec<String> = Vec::new();

    // 同一 Grep パターンの重複実行を検知
    {
        let mut grep_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for s in done_log.iter() {
            // done_log エントリ例: "✓ Grep(IrInst in src)"
            if let Some(rest) = s.strip_prefix("✓ Grep(") {
                let key = rest.trim_end_matches(')');
                *grep_counts.entry(key).or_insert(0) += 1;
            }
        }
        if let Some((key, &count)) = grep_counts.iter().find(|&(_, &c)| c >= 2) {
            warnings.push(format!(
                "[⚠ 重複 Grep を検知] \"{}\" を既に {count} 回 grep しています。\
                同じパターンを再度検索する必要はありません。\
                別のキーワードを使うか、情報が十分なら今すぐ bot で回答してください。",
                key
            ));
        }
    }

    // read_file 連打を検知 — 直近 done_log の大半が ReadFile なら警告
    {
        let recent_reads = done_log
            .iter()
            .rev()
            .take(5)
            .filter(|s| s.contains("ReadFile("))
            .count();
        if recent_reads >= 3 {
            warnings.push(
                "[⚠ 連続 read_file を検知] grep でキーワード検索してから必要なファイルだけ読んでください。\
                または十分な情報が揃っているなら今すぐ bot で回答してください。".to_string()
            );
        }
    }

    // Glob 後に grep なしで read_file を連続使用するパターンを検知
    // パターン: ✓ Glob(...) の後に Grep なしで 2+ 件の ReadFile → 非効率な探索を警告
    {
        let last_glob_pos = done_log.iter().rposition(|s| s.starts_with("✓ Glob("));
        if let Some(glob_pos) = last_glob_pos {
            let after_glob = &done_log[glob_pos + 1..];
            let has_grep_after = after_glob.iter().any(|s| s.contains("Grep("));
            let read_count_after = after_glob
                .iter()
                .filter(|s| s.contains("ReadFile("))
                .count();
            if read_count_after >= 2 && !has_grep_after {
                warnings.push(format!(
                    "[⚠ Glob 後に連続 read_file を検知 ({read_count_after} 件)] \
                    ファイル一覧取得後にファイルを順番に読むのは非効率です。\
                    grep でシンボルや関数名を検索してから必要なファイルだけ read_file してください。\
                    または十分な情報が揃っているなら今すぐ bot で回答してください。"
                ));
            }
        }
    }

    // 警告を先頭に追加
    dynamic_lines.extend(warnings);

    if !read_files.is_empty() {
        dynamic_lines.push(format!(
            "[読み込み済みファイル（再読み不要）] {}",
            compact_file_list(read_files, root)
        ));

        // ファイル読み過ぎ警告 — 10件超えたら grep 使用を促す
        let count = read_files.len();
        if count >= 10 {
            dynamic_lines.push(format!(
                "[⚠ {count} ファイル読込済] これ以上 read_file を増やすのは非効率です。\
                必要な情報が揃ったら今すぐ bot でレビュー/回答を返してください。\
                まだ必要なら grep でキーワード検索してから必要な箇所だけ read_file してください。"
            ));
        }
    }

    if !done_log.is_empty() {
        dynamic_lines.push(compact_done_log(done_log));
    }

    if dynamic_lines.is_empty() {
        format!("{static_ctx}\n\n→ 次のアクションを ```json コードブロックで出力してください")
    } else {
        format!(
            "{static_ctx}\n\n## Dynamic context\n{}\n\n→ 次のアクションを ```json コードブロックで出力してください",
            dynamic_lines.join("\n")
        )
    }
}

// ─── done_log の2段階圧縮 ────────────────────────────────────────────────────

fn compact_done_log(done_log: &[String]) -> String {
    let n = done_log.len();

    if n <= MICRO_THRESHOLD {
        // 全件表示（圧縮不要）
        let entries: Vec<&str> = done_log.iter().map(|s| s.as_str()).collect();
        return format!("[完了済みアクション] {}", entries.join(" → "));
    }

    if n <= FULL_THRESHOLD {
        // マイクロ圧縮（llm-prompts.md §2 形式）
        // 古い部分: Claude Code スタイルのヘッダー + 操作一覧
        // 直近: そのまま詳細表示
        let cutoff = n - RECENT_MICRO;
        let (older, recent) = done_log.split_at(cutoff);
        let header = compacted_header(older);
        let older_list = older
            .iter()
            .map(|s| short_label(s))
            .collect::<Vec<_>>()
            .join(" | ");
        let recent_str = recent
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(" → ");
        format!("[完了済みアクション]\n{header}\n{older_list}\n[Recent] {recent_str}")
    } else {
        // フル圧縮: 古い部分はヘッダーのみ + 直近3件
        let cutoff = n - RECENT_FULL;
        let (older, recent) = done_log.split_at(cutoff);
        let header = compacted_header(older);
        let recent_str = recent
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(" → ");
        format!("[完了済みアクション]\n{header}\n[Recent] {recent_str}")
    }
}

/// Claude Code の "[Context compacted — summary of N earlier messages]" 形式のヘッダー
fn compacted_header(slice: &[String]) -> String {
    let success = slice.iter().filter(|s| s.starts_with('✓')).count();
    let fail = slice.iter().filter(|s| s.starts_with('✗')).count();
    let stats = if fail == 0 {
        format!("✓{success}")
    } else {
        format!("✓{success} ✗{fail}")
    };
    format!(
        "[Context compacted — summary of {} earlier actions ({})]",
        slice.len(),
        stats
    )
}

/// done_log の1エントリからラベル部分だけを短く返す
/// "✓ ReadFile(src/main.rs)" → "✓ ReadFile(src/main.rs)"
/// "✗ Cmd(cargo build) → exit: 1 ..." → "✗ Cmd(cargo build)"
fn short_label(entry: &str) -> &str {
    // 矢印以降（エラー詳細）は省略
    if let Some(idx) = entry.find(" → ") {
        &entry[..idx]
    } else {
        entry
    }
}

// ─── ファイルリストのディレクトリ集約圧縮 ────────────────────────────────────

fn compact_file_list(read_files: &HashSet<PathBuf>, root: &Path) -> String {
    let mut files: Vec<String> = read_files
        .iter()
        .filter_map(|p| p.strip_prefix(root).ok())
        .map(|p| p.display().to_string())
        .collect();
    files.sort();

    if files.len() <= FILE_LIST_THRESHOLD {
        // 少数ならそのまま列挙
        return files.join(", ");
    }

    // 多数ならディレクトリ単位に集約
    let mut dir_map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for file in &files {
        let dir = Path::new(file)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| ".".to_string());
        dir_map.entry(dir).or_default().push(file.clone());
    }

    let parts: Vec<String> = dir_map
        .iter()
        .map(|(dir, fs)| {
            if fs.len() == 1 {
                fs[0].clone()
            } else {
                format!("{}/({} ファイル)", dir, fs.len())
            }
        })
        .collect();

    format!("{} [計 {} ファイル]", parts.join(", "), files.len())
}

// ─── 表示用サマリー ───────────────────────────────────────────────────────────

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
    let first_count = first.chars().count();
    if first_count > 120 {
        format!("{chars}…")
    } else {
        chars
    }
}

// ─── テスト ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn done(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("✓ Action{i}")).collect()
    }

    #[test]
    fn test_no_compression_when_small() {
        let log = done(4);
        let out = compact_done_log(&log);
        assert!(out.contains("Action0"));
        assert!(out.contains("Action3"));
        assert!(!out.contains("Context compacted"));
    }

    #[test]
    fn test_micro_compression() {
        let log = done(10);
        let out = compact_done_log(&log);
        // Claude Code スタイルのヘッダーが入る
        assert!(out.contains("[Context compacted — summary of 5 earlier actions (✓5)]"));
        // 古い操作が一覧表示される
        assert!(out.contains("Action0"));
        // 直近5件は詳細表示
        assert!(out.contains("Action9"));
        assert!(out.contains("Action5"));
    }

    #[test]
    fn test_full_compression() {
        let log = done(20);
        let out = compact_done_log(&log);
        // Claude Code スタイルのヘッダー
        assert!(out.contains("[Context compacted — summary of 17 earlier actions (✓17)]"));
        // 直近3件のみ詳細
        assert!(out.contains("Action19"));
        assert!(!out.contains("Action15")); // 直近3件に入らない
        assert!(out.contains("[Recent]"));
    }

    #[test]
    fn test_failure_count_in_summary() {
        let mut log = done(8);
        log[2] = "✗ FailedAction".to_string();
        let out = compact_done_log(&log);
        assert!(out.contains("✗1"));
    }

    #[test]
    fn test_file_list_no_compression_when_few() {
        let mut files = HashSet::new();
        let root = Path::new("/root");
        files.insert(PathBuf::from("/root/src/main.rs"));
        files.insert(PathBuf::from("/root/src/lib.rs"));
        let out = compact_file_list(&files, root);
        assert!(out.contains("main.rs"));
        assert!(!out.contains("ファイル)"));
    }

    #[test]
    fn test_file_list_groups_by_dir() {
        let root = Path::new("/root");
        let mut files = HashSet::new();
        for i in 0..5 {
            files.insert(PathBuf::from(format!("/root/src/agent/file{i}.rs")));
        }
        for i in 0..5 {
            files.insert(PathBuf::from(format!("/root/src/executor/file{i}.rs")));
        }
        let out = compact_file_list(&files, root);
        assert!(out.contains("(5 ファイル)"));
        assert!(out.contains("10 ファイル"));
    }

    #[test]
    fn test_consecutive_read_warns_at_3() {
        // 3件連続 ReadFile → soft warning が発動する
        let root = Path::new("/root");
        let log = vec![
            "✓ ReadFile(src/a.rs)".to_string(),
            "✓ ReadFile(src/b.rs)".to_string(),
            "✓ ReadFile(src/c.rs)".to_string(),
        ];
        let out = build_context_header("調査", &HashSet::new(), root, &log);
        assert!(
            out.contains("連続 read_file を検知"),
            "3件で soft warning が出るべき"
        );
    }

    #[test]
    fn test_glob_then_reads_warns_at_2() {
        // Glob 後に grep なしで 2+ 件の ReadFile → 専用の警告が出る
        let root = Path::new("/root");
        let log = vec![
            "✓ Glob(src/**/*.rs)".to_string(),
            "✓ ReadFile(src/main.rs)".to_string(),
            "✓ ReadFile(src/lexer.rs)".to_string(),
        ];
        let out = build_context_header("調査", &HashSet::new(), root, &log);
        assert!(
            out.contains("Glob 後に連続 read_file"),
            "glob+2reads で警告が出るべき"
        );
    }

    #[test]
    fn test_glob_then_reads_no_warn_if_grep_used() {
        // Glob 後に Grep を使っていれば警告しない
        let root = Path::new("/root");
        let log = vec![
            "✓ Glob(src/**/*.rs)".to_string(),
            "✓ Grep(fn execute in src)".to_string(),
            "✓ ReadFile(src/main.rs)".to_string(),
            "✓ ReadFile(src/lexer.rs)".to_string(),
        ];
        let out = build_context_header("調査", &HashSet::new(), root, &log);
        assert!(
            !out.contains("Glob 後に連続 read_file"),
            "grep 使用後は警告不要"
        );
    }

    #[test]
    fn test_glob_then_reads_no_warn_if_only_one_read() {
        // Glob 後の ReadFile が 1件だけなら警告しない
        let root = Path::new("/root");
        let log = vec![
            "✓ Glob(src/**/*.rs)".to_string(),
            "✓ ReadFile(src/main.rs)".to_string(),
        ];
        let out = build_context_header("調査", &HashSet::new(), root, &log);
        assert!(!out.contains("Glob 後に連続 read_file"), "1件では警告不要");
    }
}
