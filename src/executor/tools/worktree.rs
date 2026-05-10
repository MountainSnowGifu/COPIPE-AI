use crate::executor::LOG_DIR;
use crate::executor::ToolResult;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const STATE_FILE: &str = "worktree.json";

#[derive(Debug, Serialize, Deserialize)]
pub struct WorktreeState {
    pub path: PathBuf,
    pub branch: String,
    pub original_root: PathBuf,
}

/// アクティブな worktree の状態を読み込む
pub fn load_state(root: &Path) -> Option<WorktreeState> {
    let path = root.join(LOG_DIR).join(STATE_FILE);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

fn save_state(root: &Path, state: &WorktreeState) -> std::io::Result<()> {
    let path = root.join(LOG_DIR).join(STATE_FILE);
    std::fs::write(path, serde_json::to_string_pretty(state).unwrap())
}

fn remove_state(root: &Path) {
    let _ = std::fs::remove_file(root.join(LOG_DIR).join(STATE_FILE));
}

// ─── git コマンドヘルパー（check_cmd_safety を経由しない内部操作）─────────────

async fn git(args: &[&str], cwd: &Path) -> Result<String, String> {
    let out = tokio::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .await
        .map_err(|e| format!("git 起動失敗: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

// ─── EnterWorktree ────────────────────────────────────────────────────────────

pub async fn enter(root: &Path) -> ToolResult {
    // 既にアクティブな worktree があればエラー
    if let Some(state) = load_state(root) {
        return ToolResult::new(
            "EnterWorktree",
            format!(
                "ERROR: すでに worktree がアクティブです ({})\n\
                先に exit_worktree で終了してください。",
                state.path.display()
            ),
        );
    }

    // git リポジトリかチェック
    if git(&["rev-parse", "--git-dir"], root).await.is_err() {
        return ToolResult::new(
            "EnterWorktree",
            "ERROR: git リポジトリではありません。EnterWorktree は git リポジトリ内でのみ使えます。",
        );
    }

    // 一時ブランチ名とパスを生成
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let branch = format!("copipe-wt-{ts}");
    let wt_path = std::env::temp_dir().join(&branch);

    // git worktree add
    match git(
        &[
            "worktree",
            "add",
            wt_path.to_str().unwrap_or(""),
            "-b",
            &branch,
        ],
        root,
    )
    .await
    {
        Err(e) => {
            return ToolResult::new("EnterWorktree", format!("ERROR: worktree 作成失敗: {e}"));
        }
        Ok(_) => {}
    }

    // 状態を保存
    let state = WorktreeState {
        path: wt_path.clone(),
        branch: branch.clone(),
        original_root: root.to_path_buf(),
    };
    std::fs::create_dir_all(root.join(LOG_DIR)).ok();
    if save_state(root, &state).is_err() {
        // 状態保存失敗 → worktree を削除してロールバック
        let _ = git(
            &[
                "worktree",
                "remove",
                "--force",
                wt_path.to_str().unwrap_or(""),
            ],
            root,
        )
        .await;
        let _ = git(&["branch", "-D", &branch], root).await;
        return ToolResult::new("EnterWorktree", "ERROR: 状態ファイルの保存に失敗しました。");
    }

    ToolResult::new(
        "EnterWorktree",
        format!(
            "OK\n\
            隔離ブランチ '{branch}' で作業を開始しました。\n\
            以降の file / patch / edit 操作はこのブランチに反映されます。\n\
            完了後: {{\"type\": \"exit_worktree\", \"action\": \"merge\"}}\n\
            破棄する場合: {{\"type\": \"exit_worktree\", \"action\": \"discard\"}}"
        ),
    )
}

// ─── ExitWorktree ─────────────────────────────────────────────────────────────

pub async fn exit(root: &Path, action: &str, commit_message: &Option<String>) -> ToolResult {
    let state = match load_state(root) {
        Some(s) => s,
        None => {
            return ToolResult::new(
                "ExitWorktree",
                "ERROR: アクティブな worktree がありません。enter_worktree を先に実行してください。",
            );
        }
    };

    let wt_path = &state.path;
    let branch = &state.branch;

    let result = match action {
        "merge" => do_merge(root, wt_path, branch, commit_message).await,
        "discard" => do_discard(root, wt_path, branch).await,
        other => Err(format!(
            "不正なアクション '{other}'。'merge' または 'discard' を指定してください。"
        )),
    };

    // クリーンアップは成否にかかわらず実施
    let _ = git(
        &[
            "worktree",
            "remove",
            "--force",
            wt_path.to_str().unwrap_or(""),
        ],
        root,
    )
    .await;
    let _ = git(&["branch", "-D", branch], root).await;
    remove_state(root);

    match result {
        Ok(msg) => ToolResult::new("ExitWorktree", format!("OK\n{msg}")),
        Err(e) => ToolResult::new("ExitWorktree", format!("ERROR: {e}")),
    }
}

async fn do_merge(
    root: &Path,
    wt_path: &Path,
    branch: &str,
    commit_message: &Option<String>,
) -> Result<String, String> {
    // worktree 内の変更をコミット
    let staged = git(&["status", "--porcelain"], wt_path)
        .await
        .unwrap_or_default();
    if !staged.is_empty() {
        git(&["add", "-A"], wt_path).await?;
        let msg = format!("copipe-ai: worktree changes ({})", branch);
        git(&["commit", "-m", &msg], wt_path).await?;
    }

    // メインブランチへ squash merge
    git(&["merge", "--squash", branch], root)
        .await
        .map_err(|e| format!("squash merge 失敗: {e}"))?;

    // コミット
    let msg = commit_message
        .as_deref()
        .unwrap_or("copipe-ai: apply worktree changes");
    git(&["commit", "-m", msg], root)
        .await
        .map_err(|e| format!("コミット失敗: {e}"))?;

    Ok(format!(
        "変更をメインブランチにマージしました（コミット: '{msg}'）"
    ))
}

async fn do_discard(_root: &Path, _wt_path: &Path, branch: &str) -> Result<String, String> {
    // worktree 内の変更は git worktree remove --force で削除済みになる
    Ok(format!("ブランチ '{branch}' の変更を破棄しました"))
}
