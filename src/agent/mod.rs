mod prompt;
pub use prompt::build_system_prompt;

use crate::color::{BOLD, CYAN_BOLD, DIM, GREEN, RED, RED_BOLD, RESET};
use crate::command::{parse_commands, AiCommand};
use crate::executor::{execute, format_tool_results, now_timestamp, ToolResult, LOG_DIR};
use crate::session::{ai_message_count, get_codeblocks_from_dom, page_diagnostic, CopilotSession};
use std::io::Write as _;

// ─── エージェントループ ────────────────────────────────────────────────────────

const SCHEMA_HINT: &str = r#"【正しいJSON形式の例】
単一コマンド:
```json
{"type": "read_file", "path": "src/main.rs"}
```
複数コマンド（配列）:
```json
[
  {"type": "txt", "content": "作業を開始します"},
  {"type": "cmd", "name": "ビルド", "cmd": ["cargo", "build"], "workdir": ".", "timeout": 60}
]
```
使えるtypeの一覧:
  read_file / list_dir / file / patch / mkdir / delete_file
  cmd / txt / read_log / bot / error
必須フィールド:
  read_file: path
  list_dir:  path
  file:      path, content
  patch:     path, diff
  mkdir:     path
  delete_file: path
  cmd:       name, cmd(配列), timeout(必須・秒数)
  txt:       content
  read_log:  filename(cmd_log/ai_log/ai_readonly)
  bot:       message
JSONの後に文章を続けず、コードブロック(```json ... ```)で出力してください。"#;

fn parse_blocks(blocks: &[String]) -> (Vec<crate::command::AiCommand>, Vec<String>) {
    let mut commands = Vec::new();
    let mut errors = Vec::new();
    for b in blocks {
        match parse_commands(b) {
            Ok(cmds) => commands.extend(cmds),
            Err(e) => errors.push(format!(
                "JSONパースエラー: {e}\n\n元のブロック:\n```\n{b}\n```\n\n{SCHEMA_HINT}"
            )),
        }
    }
    (commands, errors)
}

async fn get_commands(
    session: &mut CopilotSession,
    root: &std::path::Path,
    prompt: &str,
) -> anyhow::Result<(Vec<crate::command::AiCommand>, Vec<String>)> {
    if let Err(e) = session.send_raw(prompt).await {
        write_browser_log(root, &format!("send_raw error: {e}"), session).await;
        return Err(e);
    }
    let n = ai_message_count(&session.page).await?;
    let blocks = get_codeblocks_from_dom(&session.page, n).await;

    if blocks.is_empty() {
        eprintln!("JSON ブロックなし → 再要求します");
        write_browser_log(root, "no JSON code block in latest AI message", session).await;
        if let Err(e) = session.send_raw(
            "次の作業ステップを JSON スキーマ形式で記述してください。\
            例：\n```json\n{\"type\": \"list_dir\", \"path\": \".\"}\n```"
        ).await {
            write_browser_log(root, &format!("retry send_raw error: {e}"), session).await;
            return Err(e);
        }
        let n2 = ai_message_count(&session.page).await?;
        let blocks2 = get_codeblocks_from_dom(&session.page, n2).await;
        if blocks2.is_empty() {
            write_browser_log(root, "still no JSON code block after retry", session).await;
        }
        return Ok(parse_blocks(&blocks2));
    }

    Ok(parse_blocks(&blocks))
}

async fn write_browser_log(root: &std::path::Path, reason: &str, session: &CopilotSession) {
    let log_dir = root.join(LOG_DIR);
    std::fs::create_dir_all(&log_dir).ok();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("browser_log"))
    {
        let diag = page_diagnostic(&session.page).await;
        let entry = format!("[{}] {reason}\n{diag}\n---\n", now_timestamp());
        f.write_all(entry.as_bytes()).ok();
    }
}

/// 毎ターンのプロンプトに付加するコンテキストヘッダー。
/// AI が「何を読んだか・何をしたか・元のタスクは何か」を忘れないようにする。
fn build_context_header(
    user_task: &str,
    read_files: &std::collections::HashSet<std::path::PathBuf>,
    root: &std::path::Path,
    done_log: &[String],
) -> String {
    let mut lines = vec![format!("[元のタスク] {user_task}")];

    if !read_files.is_empty() {
        let mut files: Vec<String> = read_files
            .iter()
            .filter_map(|p| p.strip_prefix(root).ok())
            .map(|p| p.display().to_string())
            .collect();
        files.sort();
        lines.push(format!(
            "[読み込み済みファイル（再読み不要）] {}",
            files.join(", ")
        ));
    }

    if !done_log.is_empty() {
        // 直近 8 件だけ表示（プロンプトを膨らませすぎない）
        let recent: Vec<&str> = done_log
            .iter()
            .rev()
            .take(8)
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        lines.push(format!("[完了済みアクション] {}", recent.join(" → ")));
    }

    lines.join("\n")
}

fn summarize_for_display(label: &str, output: &str) -> String {
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

pub const MAX_TURNS: u32 = 20;

pub async fn run_agent(
    session: &mut CopilotSession,
    root: &std::path::Path,
    user_task: &str,
    verbose: bool,
) -> anyhow::Result<()> {
    let mut prompt = user_task.to_string();
    let mut read_files = std::collections::HashSet::new();
    let mut done_log: Vec<String> = Vec::new();
    let mut reached_max = false;
    let mut consecutive_txt = 0u32;

    for turn in 0..MAX_TURNS {
        eprintln!("{DIM}[ターン {}/{}]{RESET}", turn + 1, MAX_TURNS);
        let (commands, parse_errors) = get_commands(session, root, &prompt).await?;

        // ai_log にこのターンの命令を記録
        {
            let log_dir = root.join(LOG_DIR);
            std::fs::create_dir_all(&log_dir).ok();
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_dir.join("ai_log"))
            {
                let mut entry = format!("=== ターン {} [{}] ===\n", turn + 1, now_timestamp());
                for cmd in &commands {
                    entry.push_str(&format!(
                        "{}\n",
                        serde_json::to_string(cmd).unwrap_or_default()
                    ));
                }
                for e in &parse_errors {
                    entry.push_str(&format!("[ParseError] {e}\n"));
                }
                entry.push_str("---\n");
                f.write_all(entry.as_bytes()).ok();
            }
        }

        let mut tool_results: Vec<ToolResult> = parse_errors
            .into_iter()
            .map(|e| ToolResult {
                label: "ParseError".to_string(),
                output: e,
            })
            .collect();

        if commands.is_empty() && tool_results.is_empty() {
            println!("コマンドが取得できませんでした");
            break;
        }

        // bot + 実ツールが共存する場合はまだ完了とみなさない
        let has_bot = commands.iter().any(|c| matches!(c, AiCommand::Bot { .. }));
        let has_real_tools = commands
            .iter()
            .any(|c| !matches!(c, AiCommand::Bot { .. } | AiCommand::Txt { .. }));
        let is_done = has_bot && !has_real_tools;

        // txt のみかどうか（実際のツール呼び出しがなければ続行を促す）
        let only_txt = !is_done
            && tool_results.is_empty()
            && commands.iter().all(|c| matches!(c, AiCommand::Txt { .. }));

        let (exec_results, messages) = execute(root, &commands, &mut read_files).await;
        for r in &exec_results {
            if r.output.starts_with("ERROR:") {
                done_log.push(format!("✗ {} → {}", r.label, r.output[6..].trim()));
            } else {
                done_log.push(format!("✓ {}", r.label));
            }
        }
        tool_results.extend(exec_results);

        for msg in &messages {
            println!("\n{CYAN_BOLD}[AI]{RESET} {msg}");
        }

        if is_done {
            break;
        }

        let ctx = build_context_header(user_task, &read_files, root, &done_log);

        if only_txt {
            consecutive_txt += 1;
            let file_hint = if read_files.is_empty() {
                "まだファイルを読み込んでいません。read_file コマンドでファイルを読んでください。".to_string()
            } else {
                let mut files: Vec<String> = read_files
                    .iter()
                    .filter_map(|p| p.strip_prefix(root).ok())
                    .map(|p| p.display().to_string())
                    .collect();
                files.sort();
                format!("読み込み済みのファイル（再読不要）: {}。", files.join(", "))
            };
            // txt が続きすぎる場合は強制的に bot で回答させる
            let action_hint = if consecutive_txt >= 2 {
                "\n今すぐ `bot` コマンドで回答を出力してください。省略せず完全な内容を含めること。\
                \n```json\n{\"type\": \"bot\", \"message\": \"（完全な回答をここに）\"}\n```"
            } else {
                "\nタスクが完了していれば `bot` コマンドで完全な回答を返してください（省略不可）：\
                \n```json\n{\"type\": \"bot\", \"message\": \"（完全な回答をここに）\"}\n```\
                \nまだ必要なツールがあれば read_file / list_dir / cmd などを実行してください。"
            };
            prompt = format!("{ctx}\n\n{file_hint}{action_hint}");
            continue;
        }
        consecutive_txt = 0;

        if tool_results.is_empty() {
            break;
        }

        for r in &tool_results {
            if r.output.starts_with("ERROR:") {
                println!("{RED_BOLD}[{}]{RESET} {}", r.label, r.output);
            } else if verbose {
                println!("[{}] {}", r.label, r.output);
            } else {
                println!(
                    "{DIM}[{}] {}{RESET}",
                    r.label,
                    summarize_for_display(&r.label, &r.output)
                );
            }
        }

        if turn + 1 == MAX_TURNS {
            println!("最大ターン数 ({MAX_TURNS}) に達しました。");
            println!("{DIM}タスクを再入力すると続きから作業できます。{RESET}");
            reached_max = true;
            break;
        }

        prompt = format!("{ctx}\n\n{}", format_tool_results(&tool_results));
    }

    if !done_log.is_empty() {
        let header = if reached_max {
            "── 実行サマリー（中断）"
        } else {
            "── 実行サマリー"
        };
        println!("\n{BOLD}{header}{RESET}");
        for item in &done_log {
            if item.starts_with('✓') {
                println!("  {GREEN}{item}{RESET}");
            } else {
                println!("  {RED}{item}{RESET}");
            }
        }
    }

    Ok(())
}
