use crate::color::{BOLD, DIM, GREEN, RED, RED_BOLD, CYAN_BOLD, RESET};
use crate::command::parse_commands;
use crate::executor::{execute, format_tool_results, ToolResult};
use crate::session::{ai_message_count, get_codeblocks_from_dom, CopilotSession};

// ─── システムプロンプト ───────────────────────────────────────────────────────

pub fn build_system_prompt(root: &std::path::Path) -> String {
    format!(
        r#"あなたはコーディングアシスタントです。ユーザーのタスクをツールを使って実行します。

作業ディレクトリ: {root}
ファイルパスは必ずこのディレクトリからの相対パスで指定してください。

【重要】必ずJSONのコードブロック（```json\n...\n```）で応答してください。
複数コマンドは配列にしてください。
JSONの後に文章を続けてはいけません。

## 使えるツール

- ファイル読み込み: {{"type": "read_file", "path": "相対パス"}}（1ターンに1ファイルのみ、複数ファイルは1つずつ別々に読む）
- ディレクトリ一覧: {{"type": "list_dir", "path": "相対パス"}}
- ファイル書き込み: {{"type": "file", "path": "相対パス", "content": "内容"}}
- 差分編集:         {{"type": "patch", "path": "相対パス", "diff": "@@ -1,3 +1,3 @@\n-旧行\n+新行\n コンテキスト"}}（ファイルの一部だけ変更したい場合に使用。コンテキスト行が一致しない場合はエラーになる）
- ディレクトリ作成: {{"type": "mkdir", "path": "相対パス"}}
- ファイル削除:   {{"type": "delete_file", "path": "相対パス"}}
- コマンド実行:   {{"type": "cmd", "name": "説明", "cmd": ["cargo", "build"], "workdir": "相対パス", "timeout": 30}}
- ユーザーへ表示: {{"type": "txt", "content": "日本語のメッセージ"}}
- タスク完了:     {{"type": "bot", "message": "完了メッセージ"}}

## cmd のルール（必須）

cmd を使う場合、必ず timeout を指定してください。timeout が無い cmd は生成してはいけません。
以下のコマンドは禁止です: rm / shutdown / reboot / curl / wget / apt / apt-get

## 安全ルール

- ../ を含むパスは禁止
- / で始まる絶対パスは禁止

ツール実行結果は「[ツール実行結果]」として返ってきます。
全てのタスクが完了したら必ず {{"type": "bot", "message": "..."}} で終えてください。"#,
        root = root.display()
    )
}

// ─── エージェントループ ────────────────────────────────────────────────────────

fn parse_blocks(blocks: &[String]) -> (Vec<crate::command::AiCommand>, Vec<String>) {
    let mut commands = Vec::new();
    let mut errors = Vec::new();
    for b in blocks {
        match parse_commands(b) {
            Ok(cmds) => commands.extend(cmds),
            Err(e) => errors.push(format!(
                "JSONパースエラー: {e}\n元のブロック:\n```\n{b}\n```\n正しいスキーマで再出力してください。"
            )),
        }
    }
    (commands, errors)
}

async fn get_commands(
    session: &mut CopilotSession,
    prompt: &str,
) -> anyhow::Result<(Vec<crate::command::AiCommand>, Vec<String>)> {
    session.send_raw(prompt).await?;
    let n = ai_message_count(&session.page).await?;
    let blocks = get_codeblocks_from_dom(&session.page, n).await;

    if blocks.is_empty() {
        eprintln!("JSON ブロックなし → 再要求します");
        session
            .send_raw("JSON コードブロックで回答してください。")
            .await?;
        let n2 = ai_message_count(&session.page).await?;
        let blocks2 = get_codeblocks_from_dom(&session.page, n2).await;
        return Ok(parse_blocks(&blocks2));
    }

    Ok(parse_blocks(&blocks))
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
    if first.len() > 120 {
        format!("{}…", &first[..120])
    } else {
        first.to_string()
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

    for turn in 0..MAX_TURNS {
        eprintln!("{DIM}[ターン {}/{}]{RESET}", turn + 1, MAX_TURNS);
        let (commands, parse_errors) = get_commands(session, &prompt).await?;

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

        if tool_results.is_empty() {
            break;
        }

        for r in &tool_results {
            if r.output.starts_with("ERROR:") {
                println!("{RED_BOLD}[{}]{RESET} {}", r.label, r.output);
            } else if verbose {
                println!("[{}] {}", r.label, r.output);
            } else {
                eprintln!("{DIM}[{}] {}{RESET}", r.label, summarize_for_display(&r.label, &r.output));
            }
        }

        if turn + 1 == MAX_TURNS {
            println!("最大ターン数 ({MAX_TURNS}) に達しました。");
            reached_max = true;
            break;
        }

        prompt = format_tool_results(&tool_results);
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
