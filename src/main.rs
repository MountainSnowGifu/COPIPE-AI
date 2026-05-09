mod browser;
mod command;
mod executor;

use browser::{free_port, launch_edge};
use command::parse_commands;
use executor::{execute, format_tool_results};

use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams;
use futures::StreamExt;
use std::process::Child;
use std::time::Duration;

// ─── システムプロンプト ───────────────────────────────────────────────────────

fn build_system_prompt(root: &std::path::Path) -> String {
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

// ─── Browser / Page ──────────────────────────────────────────────────────────

async fn get_ws_url(port: u16) -> anyhow::Result<String> {
    let url = format!("http://127.0.0.1:{port}/json/version");
    eprintln!("CDP 接続を待機中 (最大15秒)...");
    for i in 0..30 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if i > 0 && i % 6 == 0 {
            eprintln!("  CDP 待機中... ({}秒経過)", i / 2);
        }
        if let Ok(resp) = reqwest::get(&url).await {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(ws) = json["webSocketDebuggerUrl"].as_str() {
                    eprintln!("CDP 接続完了。");
                    return Ok(ws.to_string());
                }
            }
        }
    }
    anyhow::bail!("Edge の CDP に接続できませんでした (port {port})")
}

async fn wait_for_element(
    page: &chromiumoxide::Page,
    selector: &str,
    timeout_secs: u64,
) -> anyhow::Result<chromiumoxide::element::Element> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let start = tokio::time::Instant::now();
    let mut last_report = 0u64;
    loop {
        match page.find_element(selector).await {
            Ok(el) => return Ok(el),
            Err(_) => {
                if tokio::time::Instant::now() >= deadline {
                    anyhow::bail!("タイムアウト: セレクター '{selector}' が見つかりません");
                }
                let elapsed = start.elapsed().as_secs();
                if elapsed >= last_report + 5 && elapsed > 0 {
                    eprintln!("  要素待機中... ({}秒経過)", elapsed);
                    last_report = elapsed;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

async fn prepare_copilot_page(browser: &Browser) -> anyhow::Result<chromiumoxide::Page> {
    let page = browser.new_page("about:blank").await?;
    page.execute(
        SetDeviceMetricsOverrideParams::builder()
            .width(1280u32)
            .height(800u32)
            .device_scale_factor(1.0)
            .mobile(false)
            .build()
            .map_err(|e| anyhow::anyhow!(e))?,
    )
    .await?;
    page.execute(AddScriptToEvaluateOnNewDocumentParams::new(
        "Object.defineProperty(navigator, 'webdriver', {get: () => undefined})",
    ))
    .await?;
    eprintln!("Copilot ページを開いています...");
    page.goto("https://copilot.microsoft.com").await?;
    eprintln!("ページ初期化を待機中 (5秒)...");
    tokio::time::sleep(Duration::from_secs(5)).await;
    eprintln!("入力欄を待機中 (最大20秒)...");
    wait_for_element(&page, "#userInput", 20).await?;
    eprintln!("入力欄を検出しました。");
    Ok(page)
}

async fn ai_message_count(page: &chromiumoxide::Page) -> anyhow::Result<usize> {
    let n = page
        .evaluate_expression(r#"document.querySelectorAll('[data-testid="ai-message"]').length"#)
        .await?
        .value()
        .and_then(|v| v.as_f64())
        .ok_or_else(|| anyhow::anyhow!("ai-message 数の取得に失敗"))?;
    Ok(n as usize)
}

fn extract_ai_text(raw: &str) -> String {
    let text = raw.trim();
    let text = text
        .strip_prefix("Copilot の発言\n")
        .or_else(|| text.strip_prefix("Copilot の発言"))
        .unwrap_or(text);
    let cutoffs = [
        "\nページ内で編集します",
        "\nFluentU",
        "\nhanabira",
        "\n参照:",
    ];
    let mut text = text;
    for cutoff in &cutoffs {
        if let Some(pos) = text.find(cutoff) {
            text = &text[..pos];
        }
    }
    text.trim().to_string()
}

async fn read_nth_ai_text(page: &chromiumoxide::Page, n: usize) -> String {
    let raw = page
        .evaluate_expression(&format!(
            r#"
            (() => {{
                const msgs = document.querySelectorAll('[data-testid="ai-message"]');
                if (msgs.length < {n}) return '';
                const el = msgs[{n} - 1];
                const clone = el.cloneNode(true);
                ['[data-testid="message-item-reactions"]', 'button', 'cite', '.supcontainer']
                    .forEach(sel => clone.querySelectorAll(sel).forEach(e => e.remove()));
                return clone.innerText.trim();
            }})()
        "#
        ))
        .await
        .ok()
        .and_then(|r| r.value().and_then(|v| v.as_str().map(|s| s.to_string())))
        .unwrap_or_default();

    extract_ai_text(&raw)
}

async fn get_codeblocks_from_dom(page: &chromiumoxide::Page, n: usize) -> Vec<String> {
    let raw = page
        .evaluate_expression(&format!(
            r#"
            (() => {{
                const msgs = document.querySelectorAll('[data-testid="ai-message"]');
                if (msgs.length < {n}) return '';
                const el = msgs[{n} - 1];
                const blocks = [...el.querySelectorAll('pre > code')];
                return blocks.map(b => b.innerText.trim()).filter(t => t).join('\x00');
            }})()
        "#
        ))
        .await
        .ok()
        .and_then(|r| r.value().and_then(|v| v.as_str().map(|s| s.to_string())))
        .unwrap_or_default();

    if raw.is_empty() {
        return vec![];
    }
    raw.split('\x00')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .filter(|s| serde_json::from_str::<serde_json::Value>(s).is_ok())
        .collect()
}

// ─── プロンプト分割 ───────────────────────────────────────────────────────────

const PROMPT_CHUNK_SIZE: usize = 10_000;

fn split_prompt(text: &str) -> Vec<String> {
    if text.len() <= PROMPT_CHUNK_SIZE {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        if text.len() - start <= PROMPT_CHUNK_SIZE {
            chunks.push(text[start..].to_string());
            break;
        }
        let mut end = start + PROMPT_CHUNK_SIZE;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let slice = &text[start..end];
        let offset = slice
            .rfind("\n\n")
            .map(|p| p + 2)
            .or_else(|| slice.rfind('\n').map(|p| p + 1))
            .unwrap_or(end - start);
        chunks.push(text[start..start + offset].to_string());
        start += offset;
    }
    chunks
}

// ─── CopilotSession ──────────────────────────────────────────────────────────

struct CopilotSession {
    page: chromiumoxide::Page,
    edge: Child,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for CopilotSession {
    fn drop(&mut self) {
        self.handle.abort();
        self.edge.kill().ok();
        self.edge.wait().ok();
    }
}

impl CopilotSession {
    async fn start() -> anyhow::Result<Self> {
        eprintln!("ブラウザを起動中...");
        let port = free_port();
        let mut edge = launch_edge(port)?;
        eprintln!("ブラウザプロセス起動完了 (port {port})。");
        let result = Self::init(port, &mut edge).await;
        match result {
            Ok((page, handle)) => Ok(Self { page, edge, handle }),
            Err(e) => {
                edge.kill().ok();
                edge.wait().ok();
                Err(e)
            }
        }
    }

    async fn init(
        port: u16,
        edge: &mut Child,
    ) -> anyhow::Result<(chromiumoxide::Page, tokio::task::JoinHandle<()>)> {
        let ws_url = get_ws_url(port).await?;
        eprintln!("WebSocket に接続中...");
        let (browser, mut handler) = Browser::connect(&ws_url).await?;
        eprintln!("ブラウザ接続完了。");
        let handle = tokio::spawn(async move {
            while let Some(h) = handler.next().await {
                if let Err(e) = h {
                    eprintln!("ハンドラエラー: {e}");
                    break;
                }
            }
        });
        match prepare_copilot_page(&browser).await {
            Ok(page) => Ok((page, handle)),
            Err(e) => {
                handle.abort();
                let _ = edge;
                Err(e)
            }
        }
    }

    async fn send_raw(&mut self, prompt: &str) -> anyhow::Result<()> {
        let chunks = split_prompt(prompt);
        let total = chunks.len();
        for (i, chunk) in chunks.into_iter().enumerate() {
            let part = i + 1;
            let msg = if total == 1 {
                chunk
            } else if part < total {
                format!("（{part}/{total}）続きがあります。JSON応答はまだ不要です。\n{chunk}")
            } else {
                format!("（{part}/{total}）全データ送信完了。以降の処理を続けてください。\n{chunk}")
            };
            eprintln!("プロンプト送信 ({part}/{total}, {}文字)", msg.len());
            self.send_raw_single(&msg).await?;
        }
        Ok(())
    }

    async fn send_raw_single(&mut self, prompt: &str) -> anyhow::Result<()> {
        let page = &self.page;
        let baseline = ai_message_count(page).await?;

        wait_for_element(page, "#userInput", 10).await?;
        page.evaluate_expression("document.querySelector('#userInput').focus()")
            .await?;
        tokio::time::sleep(Duration::from_millis(300)).await;

        let js_str = serde_json::to_string(prompt)?;
        page.evaluate_expression(&format!(
            r#"
            (function() {{
                const el = document.querySelector('#userInput');
                if (!el) return 'not found';
                const setter = Object.getOwnPropertyDescriptor(
                    window.HTMLTextAreaElement.prototype, 'value'
                ).set;
                setter.call(el, {js_str});
                el.dispatchEvent(new Event('input',  {{ bubbles: true }}));
                el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                return 'ok';
            }})()
        "#
        ))
        .await?;
        tokio::time::sleep(Duration::from_millis(500)).await;

        page.evaluate_expression(r#"
            (function() {
                var el = document.querySelector('#userInput');
                if (!el) return;
                el.dispatchEvent(new KeyboardEvent('keydown', {key: 'Enter', code: 'Enter', bubbles: true}));
                el.dispatchEvent(new KeyboardEvent('keyup',   {key: 'Enter', code: 'Enter', bubbles: true}));
            })();
        "#)
        .await?;
        tokio::time::sleep(Duration::from_millis(300)).await;

        let target = baseline + 1;
        wait_for_ai_message_count(page, target, 90).await?;
        wait_for_stable_text(page, target, 90).await?;
        Ok(())
    }
}

async fn wait_for_ai_message_count(
    page: &chromiumoxide::Page,
    n: usize,
    timeout_secs: u64,
) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    eprintln!("応答を待機中...");
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if ai_message_count(page).await.unwrap_or(0) >= n {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("応答の開始がタイムアウトしました");
        }
    }
}

async fn wait_for_stable_text(
    page: &chromiumoxide::Page,
    n: usize,
    timeout_secs: u64,
) -> anyhow::Result<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let mut last = String::new();
    let mut stable = 0u64;

    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let text = read_nth_ai_text(page, n).await;

        if !text.is_empty() && text == last {
            stable += 1;
            eprintln!("安定 {stable}/5 ({} 文字)", text.len());
            if stable >= 5 {
                return Ok(text);
            }
        } else if !text.is_empty() {
            eprintln!("生成中... ({} 文字)", text.len());
            stable = 0;
            last = text;
        }

        if tokio::time::Instant::now() >= deadline {
            if !last.is_empty() {
                return Ok(last);
            }
            anyhow::bail!("応答テキストの取得がタイムアウトしました");
        }
    }
}

// ─── エージェントループ ────────────────────────────────────────────────────────

/// JSON ブロックをパースし (コマンド列, エラー列) を返す
fn parse_blocks(blocks: &[String]) -> (Vec<command::AiCommand>, Vec<String>) {
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
) -> anyhow::Result<(Vec<command::AiCommand>, Vec<String>)> {
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

const MAX_TURNS: u32 = 20;

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

async fn run_agent(
    session: &mut CopilotSession,
    root: &std::path::Path,
    user_task: &str,
    verbose: bool,
) -> anyhow::Result<()> {
    let mut prompt = user_task.to_string();
    let mut read_files = std::collections::HashSet::new();
    let mut done_log: Vec<String> = Vec::new();

    for turn in 0..MAX_TURNS {
        eprintln!("[ターン {}/{}]", turn + 1, MAX_TURNS);
        let (commands, parse_errors) = get_commands(session, &prompt).await?;

        // パースエラーを ToolResult として積む
        let mut tool_results: Vec<executor::ToolResult> = parse_errors
            .into_iter()
            .map(|e| executor::ToolResult {
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
            println!("\n[AI] {msg}");
        }

        if tool_results.is_empty() {
            break;
        }

        for r in &tool_results {
            if r.output.starts_with("ERROR:") {
                println!("[{}] {}", r.label, r.output);
            } else if verbose {
                println!("[{}] {}", r.label, r.output);
            } else {
                eprintln!("[{}] {}", r.label, summarize_for_display(&r.label, &r.output));
            }
        }

        if turn + 1 == MAX_TURNS {
            println!("最大ターン数 ({MAX_TURNS}) に達しました。");
            if !done_log.is_empty() {
                println!("\n── 実行サマリー ────────────────────────────────────");
                for item in &done_log {
                    println!("  {item}");
                }
                println!("────────────────────────────────────────────────────");
            }
            break;
        }

        prompt = format_tool_results(&tool_results);
    }

    Ok(())
}

// ─── main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use rustyline::error::ReadlineError;

    // CLI 引数パース: copipe-ai [--verbose|-v] [プロジェクトディレクトリ]
    let args: Vec<String> = std::env::args().skip(1).collect();
    let verbose = args.iter().any(|a| a == "--verbose" || a == "-v");
    let root_dir = args.iter().find(|a| !a.starts_with('-')).cloned();

    let root = match root_dir {
        Some(ref p) => std::path::PathBuf::from(p),
        None => std::env::current_dir()?,
    }
    .canonicalize()?;

    eprintln!("プロジェクトルート: {}", root.display());

    let mut session = CopilotSession::start().await?;
    eprintln!("Copilot に接続しました。");

    eprintln!("システムプロンプト送信中 (最大90秒かかることがあります)...");
    session.send_raw(&build_system_prompt(&root)).await?;
    eprintln!("準備完了。");

    println!(
        "ToyClaudeCode へようこそ。[{}] のタスクを入力してください（終了: Ctrl+D または exit）",
        root.display()
    );
    println!("ヒント: 行末に \\ を付けると次の行に続けられます。タスク実行中は Ctrl+C でキャンセルできます。");

    let mut rl = rustyline::DefaultEditor::new()?;
    let history_path = std::env::var("HOME")
        .ok()
        .map(|h| std::path::PathBuf::from(h).join(".copipe_ai_history"));
    if let Some(ref p) = history_path {
        rl.load_history(p).ok();
    }

    'repl: loop {
        // ── 入力フェーズ（行末 \ でマルチライン継続） ──────────────────
        let mut task = String::new();
        loop {
            let prompt = if task.is_empty() { "\n> " } else { "... " };
            match rl.readline(prompt) {
                Ok(line) => {
                    rl.add_history_entry(line.as_str()).ok();
                    if line.ends_with('\\') {
                        task.push_str(&line[..line.len() - 1]);
                        task.push('\n');
                    } else {
                        task.push_str(&line);
                        break;
                    }
                }
                Err(ReadlineError::Interrupted) => {
                    // Ctrl+C: 入力中ならクリア、空なら案内
                    if task.is_empty() {
                        println!("(Ctrl+D で終了)");
                    } else {
                        task.clear();
                        println!("入力をクリアしました");
                    }
                    continue 'repl;
                }
                Err(ReadlineError::Eof) => {
                    // Ctrl+D: 終了
                    break 'repl;
                }
                Err(e) => {
                    eprintln!("入力エラー: {e}");
                    break 'repl;
                }
            }
        }

        let task = task.trim().to_string();
        if task.is_empty() {
            continue;
        }
        if task == "exit" || task == "quit" {
            break;
        }

        // ── 確認ステップ ──────────────────────────────────────────────
        println!("┌─ タスク ─────────────────────────────────────────");
        for line in task.lines() {
            println!("│ {line}");
        }
        println!("└──────────────────────────────────────────────────");
        match rl.readline("実行しますか? [Y/n] ") {
            Ok(ans) if ans.trim().eq_ignore_ascii_case("n") => {
                println!("キャンセルしました");
                continue 'repl;
            }
            Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => {
                println!("キャンセルしました");
                continue 'repl;
            }
            Err(e) => {
                eprintln!("入力エラー: {e}");
                break 'repl;
            }
            Ok(_) => {}
        }

        // ── 実行フェーズ（Ctrl+C でキャンセル） ───────────────────────
        tokio::select! {
            result = run_agent(&mut session, &root, &task, verbose) => {
                match result {
                    Ok(()) => println!("\n── タスク完了 ─────────────────────────────────────────"),
                    Err(e) => eprintln!("エラー: {e}"),
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!("\nCtrl+C: タスクをキャンセルしました");
            }
        }
    }

    if let Some(ref p) = history_path {
        rl.save_history(p).ok();
    }
    println!("終了します");
    drop(session);
    Ok(())
}
