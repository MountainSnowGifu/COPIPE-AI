use crate::executor::context::ToolContext;
use crate::executor::safety::check_cmd_safety;
use crate::executor::{now_timestamp, safe_append_log, ToolResult, LOG_DIR};
use std::time::Duration;

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
                Err(e) => return ToolResult::new(format!("Cmd({name})"), format!("ERROR: workdir の解決に失敗: {e}")),
                Ok(abs) => abs,
            },
            None => ctx.root.to_path_buf(),
        };

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
                match tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output()).await {
                    Err(_) => format!("ERROR: タイムアウト ({timeout_secs}秒) - プロセスを強制終了しました"),
                    Ok(Err(e)) => format!("ERROR: コマンド実行失敗: {e}"),
                    Ok(Ok(out)) => {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        let code = out.status.code().unwrap_or(-1);
                        let prefix = if code == 0 { "" } else { "ERROR: " };
                        let mut parts = vec![format!("{prefix}exit: {code}")];
                        if !stdout.is_empty() { parts.push(format!("stdout:\n{stdout}")); }
                        if !stderr.is_empty() { parts.push(format!("stderr:\n{stderr}")); }
                        parts.join("\n")
                    }
                }
            }
        }
    };

    // cmd_log に追記
    let log_dir = ctx.root.join(LOG_DIR);
    std::fs::create_dir_all(&log_dir).ok();
    let entry = format!("[{}] $ {}\n{}\n---\n", now_timestamp(), cmd.join(" "), output);
    safe_append_log(&log_dir.join("cmd_log"), &entry);

    ToolResult::new(format!("Cmd({name})"), output)
}
