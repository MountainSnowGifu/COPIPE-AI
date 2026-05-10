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

    // read_file 連打を検知してグリップ — 直近 done_log の大半が ReadFile なら警告
    {
        let recent_reads = done_log.iter().rev().take(5)
            .filter(|s| s.contains("ReadFile("))
            .count();
        if recent_reads >= 4 {
            dynamic_lines.push(
                "[⚠ 連続 read_file を検知] grep でキーワード検索してから必要なファイルだけ読んでください。\
                または十分な情報が揃っているなら今すぐ bot で回答してください。".to_string()
            );
        }
    }

    if dynamic_lines.is_empty() {
        static_ctx
    } else {
        format!("{static_ctx}\n\n## Dynamic context\n{}", dynamic_lines.join("\n"))
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
        let older_list = older.iter()
            .map(|s| short_label(s))
            .collect::<Vec<_>>()
            .join(" | ");
        let recent_str = recent.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" → ");
        format!("[完了済みアクション]\n{header}\n{older_list}\n[Recent] {recent_str}")
    } else {
        // フル圧縮: 古い部分はヘッダーのみ + 直近3件
        let cutoff = n - RECENT_FULL;
        let (older, recent) = done_log.split_at(cutoff);
        let header = compacted_header(older);
        let recent_str = recent.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" → ");
        format!("[完了済みアクション]\n{header}\n[Recent] {recent_str}")
    }
}

/// Claude Code の "[Context compacted — summary of N earlier messages]" 形式のヘッダー
fn compacted_header(slice: &[String]) -> String {
    let success = slice.iter().filter(|s| s.starts_with('✓')).count();
    let fail    = slice.iter().filter(|s| s.starts_with('✗')).count();
    let stats = if fail == 0 {
        format!("✓{success}")
    } else {
        format!("✓{success} ✗{fail}")
    };
    format!(
        "[Context compacted — summary of {} earlier actions ({})]",
        slice.len(), stats
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
    if first.chars().count() > 120 {
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
}
