use crate::executor::context::ToolContext;
use crate::executor::safety::{NPM_INSTALL_SUBCMDS, check_cmd_safety, check_file_args_within_root};
use crate::executor::{LOG_DIR, ToolResult, now_timestamp, safe_append_log};
use std::time::Duration;

/// npm install/ci/pack に --ignore-scripts を自動付加してライフサイクルスクリプトを抑制する
fn inject_npm_ignore_scripts(cmd: &[String]) -> Vec<String> {
    if cmd.first().map(|s| s.as_str()) != Some("npm") {
        return cmd.to_vec();
    }
    let subcmd = cmd.get(1).map(|s| s.as_str()).unwrap_or("");
    if !NPM_INSTALL_SUBCMDS.contains(&subcmd) {
        return cmd.to_vec();
    }
    if cmd.iter().any(|a| a == "--ignore-scripts") {
        return cmd.to_vec();
    }
    let mut v = cmd.to_vec();
    v.push("--ignore-scripts".to_string());
    v
}

pub async fn handle(
    ctx: &ToolContext<'_>,
    name: &str,
    cmd: &[String],
    workdir: &Option<String>,
    timeout_secs: u64,
) -> ToolResult {
    println!("[{name}] {} 実行中...", cmd.join(" "));

    let output = if timeout_secs == 0 {
        "ERROR: timeout は必須です。1以上の秒数を指定して再生成してください。".to_string()
    } else if let Err(e) = check_cmd_safety(cmd) {
        // safety.rs のエラーはすべて Permission denied: プレフィックス付きなのでそのまま返す
        e
    } else {
        let workdir_path = match workdir {
            Some(wd) => match ctx.resolve(wd) {
                Err(e) => {
                    return ToolResult::new(
                        format!("Cmd({name})"),
                        format!("ERROR: workdir の解決に失敗: {e}"),
                    );
                }
                Ok(abs) => abs,
            },
            None => ctx.root.to_path_buf(),
        };

        // ファイル引数のシンボリックリンク経由ルート外アクセスを検出
        if let Err(e) = check_file_args_within_root(cmd, &workdir_path, ctx.root) {
            return ToolResult::new(format!("Cmd({name})"), e);
        }

        // npm install/ci/pack は --ignore-scripts を強制付加
        let cmd = inject_npm_ignore_scripts(cmd);

        let child = tokio::process::Command::new(&cmd[0])
            .args(&cmd[1..])
            .current_dir(&workdir_path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn();

        match child {
            Err(e) => format!("ERROR: コマンド起動失敗: {e}"),
            Ok(child) => {
                match tokio::time::timeout(
                    Duration::from_secs(timeout_secs),
                    child.wait_with_output(),
                )
                .await
                {
                    Err(_) => format!(
                        "ERROR: タイムアウト ({timeout_secs}秒) - プロセスを強制終了しました"
                    ),
                    Ok(Err(e)) => format!("ERROR: コマンド実行失敗: {e}"),
                    Ok(Ok(out)) => {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        let code = out.status.code().unwrap_or(-1);
                        let prefix = if code == 0 { "" } else { "ERROR: " };
                        let mut parts = vec![format!("{prefix}exit: {code}")];
                        if !stdout.is_empty() {
                            parts.push(format!("stdout:\n{stdout}"));
                        }
                        if !stderr.is_empty() {
                            parts.push(format!("stderr:\n{stderr}"));
                        }
                        parts.join("\n")
                    }
                }
            }
        }
    };

    // cmd_log に完全な出力を記録する。AI への分割配信は hooks.rs の hook_limit_output が担う
    // （先頭チャンクを返し、続きは read_log("cmd_log") でページネーション）
    let log_dir = ctx.root.join(LOG_DIR);
    std::fs::create_dir_all(&log_dir).ok();
    let entry = format!(
        "[{}] $ {}\n{}\n---\n",
        now_timestamp(),
        cmd.join(" "),
        output
    );
    safe_append_log(&log_dir.join("cmd_log"), &entry);

    ToolResult::new(format!("Cmd({name})"), output)
}
