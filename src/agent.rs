use crate::color::{BOLD, DIM, GREEN, RED, RED_BOLD, CYAN_BOLD, RESET};
use crate::command::{parse_commands, AiCommand};
use crate::executor::{execute, format_tool_results, ToolResult, LOG_DIR};
use crate::session::{ai_message_count, get_codeblocks_from_dom, CopilotSession};
use std::io::Write as _;

// ─── システムプロンプト ───────────────────────────────────────────────────────

pub fn build_system_prompt(root: &std::path::Path) -> String {
    format!(
        r#"あなたはコーディングアシスタントです。ユーザーのタスクをツールを使って完全に達成するまで自律的に作業を継続します。

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
- ファイル削除:     {{"type": "delete_file", "path": "相対パス"}}
- コマンド実行:     {{"type": "cmd", "name": "説明", "cmd": ["cargo", "build"], "workdir": "相対パス", "timeout": 30}}
- ユーザーへ表示:   {{"type": "txt", "content": "日本語のメッセージ"}}
- ログ読み取り:     {{"type": "read_log", "filename": "cmd_log"}}（cmd_log / ai_log / ai_readonly のいずれか）
- タスク完了:       {{"type": "bot", "message": "完了メッセージ"}}

## cmd のルール（必須）

cmd を使う場合、必ず timeout を指定してください。timeout が無い cmd は生成してはいけません。
以下のコマンドは禁止です: rm / shutdown / reboot / curl / wget / apt / apt-get

## 安全ルール

- ../ を含むパスは禁止
- / で始まる絶対パスは禁止

## 自律動作のルール（重要）

**ファイル編集のルール:**
- patch を使う前に必ず read_file でファイルの現在の内容を確認してください
- コンテキスト行は read_file で得た内容と一字一句一致させてください（スペース・インデントも完全に合わせる）
- diff 文字列の末尾に余分な \n を付けないでください
- patch が失敗した場合は read_file で再確認し、正しいコンテキストで再生成してください
- 同じ patch が2回連続で失敗したら file コマンドでファイル全体を書き直してください

**エラーが発生した場合:**
- エラー内容を分析し、原因を特定してから別のアプローチで再試行してください
- ユーザーへの確認は不要です。自分で判断して作業を継続してください

**コマンド実行後:**
- ビルドやテストを実行したら、必ず read_log でコマンドの出力を確認してください
- エラーがあれば修正して再実行してください。成功を確認してから次へ進んでください

**タスク完了の判断:**
- ユーザーの依頼が完全に達成できたと確認できるまで bot を使わないでください
- 途中で状況が分からなくなったら read_log: ai_log で自分の過去の判断を、read_log: cmd_log でコマンド履歴を確認してください
- bot は「全作業完了」のときだけ使います

ツール実行結果は「[ツール実行結果]」として返ってきます。"#,
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
    let mut listed_dirs: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    let mut done_log: Vec<String> = Vec::new();
    let mut reached_max = false;

    for turn in 0..MAX_TURNS {
        eprintln!("{DIM}[ターン {}/{}]{RESET}", turn + 1, MAX_TURNS);
        let (commands, parse_errors) = get_commands(session, &prompt).await?;

        // ai_log にこのターンの命令を記録
        {
            let log_dir = root.join(LOG_DIR);
            std::fs::create_dir_all(&log_dir).ok();
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true).append(true)
                .open(log_dir.join("ai_log"))
            {
                let mut entry = format!("=== ターン {} ===\n", turn + 1);
                for cmd in &commands {
                    entry.push_str(&format!("{}\n", serde_json::to_string(cmd).unwrap_or_default()));
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

        // bot + 実ツールが共存する場合はまだ完了とみなさない（コマンド結果をAIに返す必要がある）
        let has_bot = commands.iter().any(|c| matches!(c, AiCommand::Bot { .. }));
        let has_real_tools = commands.iter().any(|c| {
            !matches!(c, AiCommand::Bot { .. } | AiCommand::Txt { .. })
        });
        let is_done = has_bot && !has_real_tools;

        // txt のみかどうか（実際のツール呼び出しがなければ続行を促す）
        let only_txt = !is_done
            && tool_results.is_empty()
            && commands.iter().all(|c| matches!(c, AiCommand::Txt { .. }));

        let (exec_results, messages) = execute(root, &commands, &mut read_files, &mut listed_dirs).await;
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
            // txt のみで止まっている → コンテキスト付きで次のアクションを促す
            prompt = format!(
                "{ctx}\n\n読み込み済みのファイルは再読不要です。\
                上記の完了済みアクションを踏まえ、タスクを完了するために次に必要なツールを実行してください。"
            );
            continue;
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
                println!("{DIM}[{}] {}{RESET}", r.label, summarize_for_display(&r.label, &r.output));
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
