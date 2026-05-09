use super::context::{build_context_header, summarize_for_display};
use super::log::{write_ai_log, write_browser_log};
use super::parser::parse_blocks;
use crate::color::{BOLD, CYAN_BOLD, DIM, GREEN, RED, RED_BOLD, RESET, YELLOW};
use crate::command::AiCommand;
use crate::executor::{execute, format_tool_results, ToolResult};
use crate::session::{ai_message_count, get_codeblocks_from_dom, CopilotSession};
use std::collections::HashSet;
use std::io::Write as _;
use std::time::Duration;

pub const MAX_TURNS: u32 = 20;

async fn get_commands(
    session: &mut CopilotSession,
    root: &std::path::Path,
    prompt: &str,
    verbose: bool,
) -> anyhow::Result<(Vec<AiCommand>, Vec<String>)> {
    write_browser_log(root, &format!("before send_raw: prompt_len={}", prompt.len()), session).await;
    match tokio::time::timeout(Duration::from_secs(210), session.send_raw(prompt)).await {
        Ok(Ok(())) => {
            write_browser_log(root, "after send_raw: ok", session).await;
        }
        Ok(Err(e)) => {
            write_browser_log(root, &format!("send_raw error: {e}"), session).await;
            return Err(e);
        }
        Err(_) => {
            write_browser_log(root, "send_raw timeout after 210s", session).await;
            anyhow::bail!("Copilot との通信がタイムアウトしました。再実行してください");
        }
    }

    let n = match ai_message_count(&session.page).await {
        Ok(n) => n,
        Err(e) => {
            write_browser_log(root, &format!("ai_message_count error: {e}"), session).await;
            return Err(e);
        }
    };

    // コードブロックが安定するまで最大3回リトライ（レンダリング遅延対策）
    let blocks = {
        let mut last = get_codeblocks_from_dom(&session.page, n).await;
        for attempt in 1..=3u32 {
            let (_, errs) = parse_blocks(&last);
            if !errs.iter().any(|e| e.contains("EOF")) {
                break;
            }
            if verbose { eprintln!("{DIM}[再取得中 {attempt}/3]{RESET}"); }
            tokio::time::sleep(Duration::from_secs(attempt as u64 * 2)).await;
            let refreshed = get_codeblocks_from_dom(&session.page, n).await;
            if refreshed != last { last = refreshed; }
        }
        last
    };

    if blocks.is_empty() {
        if verbose { eprintln!("{DIM}[応答再要求]{RESET}"); }
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

/// Ok(true) = 正常完了、Ok(false) = 最大ターン数到達
pub async fn run_agent(
    session: &mut CopilotSession,
    root: &std::path::Path,
    user_task: &str,
    verbose: bool,
) -> anyhow::Result<bool> {
    let mut prompt = user_task.to_string();
    let mut read_files = HashSet::new();
    let mut done_log: Vec<String> = Vec::new();
    let mut reached_max = false;
    let mut consecutive_txt = 0u32;
    let mut parse_error_count = 0u32;

    for turn in 0..MAX_TURNS {
        // ターン間に人間らしいランダム待機（bot 検知回避）
        if turn > 0 {
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0) as u64;
            let wait_ms = if seed % 10 == 0 {
                8_000 + seed % 6_000
            } else {
                2_000 + seed % 3_000
            };
            if verbose {
                let wait_secs = wait_ms as f64 / 1000.0;
                eprintln!("{DIM}待機 {wait_secs:.1}秒...{RESET}");
            } else {
                let dot_count = ((wait_ms / 1000) as usize).min(5);
                let dots = ".".repeat(dot_count);
                print!("{DIM}{dots}{RESET}");
                std::io::stdout().flush().ok();
            }
            tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
            if !verbose {
                let dot_count = ((wait_ms / 1000) as usize).min(5);
                let clear = " ".repeat(dot_count);
                print!("\r{clear}\r");
                std::io::stdout().flush().ok();
            }
        }

        println!("{DIM}● ステップ {}/{}{RESET}", turn + 1, MAX_TURNS);
        let (commands, parse_errors) = get_commands(session, root, &prompt, verbose).await?;

        write_ai_log(root, turn, &commands, &parse_errors);

        let mut tool_results: Vec<ToolResult> = parse_errors
            .into_iter()
            .map(|e| ToolResult { label: "ParseError".to_string(), output: e })
            .collect();

        if commands.is_empty() && tool_results.is_empty() {
            parse_error_count += 1;
            let hint = if parse_error_count >= 2 {
                "\n  ヒント: タスクをより具体的に書くか、短い指示（例: list_dir src）から始めてみてください"
            } else {
                ""
            };
            println!("{YELLOW}応答からコマンドを取得できませんでした。タスクを再入力してください。{hint}{RESET}");
            return Ok(false);
        }

        let has_bot = commands.iter().any(|c| matches!(c, AiCommand::Bot { .. }));
        let has_real_tools = commands
            .iter()
            .any(|c| !matches!(c, AiCommand::Bot { .. } | AiCommand::Txt { .. }));
        let is_done = has_bot && !has_real_tools;

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
            if r.label == "ParseError" {
                if verbose {
                    println!("  {RED_BOLD}[ParseError]{RESET} JSON パース失敗（詳細は browser_log）");
                }
            } else if r.output.starts_with("ERROR:") {
                println!("  {RED_BOLD}[{}]{RESET} {}", r.label, r.output);
            } else if verbose {
                let preview: String = r.output.lines().take(15).collect::<Vec<_>>().join("\n");
                let suffix = if r.output.lines().count() > 15 { "\n  …" } else { "" };
                println!("  {DIM}[{}]{RESET}\n{}{}", r.label, preview, suffix);
            } else {
                println!(
                    "  {DIM}[{}] {}{RESET}",
                    r.label,
                    summarize_for_display(&r.label, &r.output)
                );
            }
        }

        if verbose {
            let multi_reads: Vec<_> = commands.iter()
                .filter(|c| matches!(c, AiCommand::ReadFile { .. }))
                .collect();
            if multi_reads.len() > 1 {
                println!("  {DIM}複数ファイル読み込みのため複数ターンを使用します{RESET}");
            }
        }

        if turn + 1 == MAX_TURNS {
            reached_max = true;
            break;
        }

        prompt = format!("{ctx}\n\n{}", format_tool_results(&tool_results));
    }

    if !done_log.is_empty() {
        let header = if reached_max { "── 実行サマリー（中断）" } else { "── 実行サマリー" };
        println!("\n{BOLD}{header}{RESET}");
        for item in &done_log {
            if item.starts_with('✓') {
                println!("  {GREEN}{item}{RESET}");
            } else {
                println!("  {RED}{item}{RESET}");
            }
        }
    }

    if reached_max {
        println!("\n{YELLOW}最大ターン数 ({MAX_TURNS}) に達しました。{RESET}");
        println!("{DIM}※ タスクを再入力しても読み込み済みファイルの記録はリセットされます。{RESET}");
        if !done_log.is_empty() {
            println!("{DIM}  続行する場合は以下の完了済み作業を踏まえてタスクを絞り込んでください:{RESET}");
            for item in done_log.iter().filter(|i| i.starts_with('✓')).take(5) {
                println!("{DIM}    {item}{RESET}");
            }
        }
    }

    Ok(!reached_max)
}
