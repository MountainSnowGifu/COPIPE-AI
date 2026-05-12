/// PreToolUse hook チェーン
///
/// tool-use-flow.md §6 "runPreToolUse" に相当する。
/// 各 hook は Continue（実行を許可）または Block（実行を中止してエラーを返す）を返す。
/// Block された場合、ツールは呼ばれず "BLOCKED: ..." が ToolResult として積まれる。
use crate::command::AiCommand;

// ─── ツール種別分類（llm-prompts.md §4 準拠） ────────────────────────────────

/// 確認不要の読み取り専用ツール（Claude Code の "許可不要" ツールに対応）
#[allow(dead_code)]
pub const READONLY_TOOLS: &[&str] = &[
    "read_file",
    "list_dir",
    "grep",
    "glob",
    "read_log",
    "todo_write", // 状態管理のみ・ファイル破壊なし
    "ask_user",   // ユーザー対話のみ
    "web_fetch",  // 外部読み取りのみ
];

/// 確認が必要な破壊的ツール（ファイル変更・コマンド実行）
pub const DESTRUCTIVE_TOOLS: &[&str] = &[
    "write_file",
    "edit",
    "multi_edit",
    "patch",
    "delete_file",
    "mkdir",
    "cmd",
    "enter_worktree",
    "exit_worktree",
];

/// tool_name が読み取り専用かどうかを判定する
#[allow(dead_code)]
pub fn is_readonly(tool_name: &str) -> bool {
    READONLY_TOOLS.contains(&tool_name)
}

/// tool_name が破壊的操作かどうかを判定する
pub fn is_destructive_tool(tool_name: &str) -> bool {
    DESTRUCTIVE_TOOLS.contains(&tool_name)
}

pub enum PreHookOutcome {
    Continue,
    Block(String),
}

type PreHookFn = fn(&str, &AiCommand, &std::path::Path) -> PreHookOutcome;

/// 適用する hook の順序リスト
const PRE_HOOKS: &[PreHookFn] = &[
    hook_log_action,         // 1. 実行前にユーザーへ通知
    hook_guard_large_file,   // 2. 大きすぎるファイルの読み込みをブロック
    hook_notify_destructive, // 3. 破壊的操作を cmd_log に記録
];

/// すべての PreToolUse hook を順番に適用する
pub fn run(tool_name: &str, cmd: &AiCommand, root: &std::path::Path) -> PreHookOutcome {
    for hook in PRE_HOOKS {
        match hook(tool_name, cmd, root) {
            PreHookOutcome::Continue => {}
            blocked => return blocked,
        }
    }
    PreHookOutcome::Continue
}

// ─── Hook 実装 ────────────────────────────────────────────────────────────────

/// 実行前にユーザーへ何をするか通知する（verbose 向け情報）
fn hook_log_action(tool_name: &str, cmd: &AiCommand, _root: &std::path::Path) -> PreHookOutcome {
    let detail = match cmd {
        AiCommand::ReadFile { path, offset_lines } if *offset_lines > 0 => {
            format!("{path} (offset={offset_lines})")
        }
        AiCommand::ReadFile { path, .. } => path.clone(),
        AiCommand::ListDir { path } => path.clone(),
        AiCommand::File { path, .. } => format!("{path} (上書き)"),
        AiCommand::Patch { path, .. } => format!("{path} (差分適用)"),
        AiCommand::DeleteFile { path } => format!("{path} (削除)"),
        AiCommand::Mkdir { path } => format!("{path} (作成)"),
        AiCommand::Cmd { cmd, .. } => cmd.join(" "),
        AiCommand::ReadLog { filename, .. } => filename.clone(),
        AiCommand::AskUser { question, .. } => format!("質問: {question}"),
        _ => return PreHookOutcome::Continue,
    };
    // #1: color 定数を使用（NO_COLOR 対応）、#3: "予定" → "→"（実行直前なので実態に合わせる）
    eprintln!(
        "  {}→ {tool_name}: {detail}{}",
        crate::color::DIM,
        crate::color::RESET
    );
    PreHookOutcome::Continue
}

/// 2MB を超えるファイルの読み込みをブロック
fn hook_guard_large_file(tool_name: &str, cmd: &AiCommand, root: &std::path::Path) -> PreHookOutcome {
    const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024; // 2MB
    if tool_name != "read_file" {
        return PreHookOutcome::Continue;
    }
    if let AiCommand::ReadFile { path, .. } = cmd {
        // 絶対パス・'..' は ctx.resolve() で後から拒否されるのでここではスキップ
        if !crate::paths::is_absolute_path_arg(path)
            && !crate::paths::has_parent_component_arg(path)
        {
            let abs = root.join(path);
            if let Ok(meta) = std::fs::metadata(&abs) {
                if meta.len() > MAX_FILE_BYTES {
                    return PreHookOutcome::Block(format!(
                        "ファイル '{path}' が大きすぎます ({} bytes)。read_file は {}MB 以下のファイル向けです。",
                        meta.len(),
                        MAX_FILE_BYTES / 1024 / 1024
                    ));
                }
            }
        }
    }
    PreHookOutcome::Continue
}

/// ファイル削除・上書きを cmd_log に記録する（監査ログ）
fn hook_notify_destructive(_tool_name: &str, cmd: &AiCommand, _root: &std::path::Path) -> PreHookOutcome {
    let action = match cmd {
        AiCommand::DeleteFile { path } => Some(format!("DELETE {path}")),
        AiCommand::File { path, .. } => Some(format!("WRITE  {path}")),
        AiCommand::Patch { path, .. } => Some(format!("PATCH  {path}")),
        _ => None,
    };
    if let Some(action) = action {
        // タイムスタンプ付きで記録（ファイルが特定できなくても stderr に出す）
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        eprintln!(
            "  {}[audit] {ts} {action}{}",
            crate::color::DIM,
            crate::color::RESET
        );
    }
    PreHookOutcome::Continue
}

// ─── テスト ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::AiCommand;

    #[test]
    fn test_small_file_allowed() {
        // 存在しないパスは metadata が取れないので Continue になる
        let cmd = AiCommand::ReadFile {
            path: "nonexistent.rs".to_string(),
            offset_lines: 0,
        };
        let root = std::path::Path::new(".");
        assert!(matches!(run("read_file", &cmd, root), PreHookOutcome::Continue));
    }

    #[test]
    fn test_cmd_always_continue_for_non_read() {
        let cmd = AiCommand::ListDir {
            path: "src".to_string(),
        };
        let root = std::path::Path::new(".");
        assert!(matches!(run("list_dir", &cmd, root), PreHookOutcome::Continue));
    }

    #[test]
    fn test_destructive_always_continue() {
        // hook_notify_destructive は Block しない（記録のみ）
        let cmd = AiCommand::DeleteFile {
            path: "old.rs".to_string(),
        };
        let root = std::path::Path::new(".");
        assert!(matches!(run("delete_file", &cmd, root), PreHookOutcome::Continue));
    }
}
