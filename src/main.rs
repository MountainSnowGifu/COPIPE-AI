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

使えるツール:
- ファイル読み込み: {{"type": "read_file", "path": "相対パス"}}
- ファイル書き込み: {{"type": "file", "path": "相対パス", "content": "内容"}}
- ディレクトリ作成: {{"type": "mkdir", "path": "相対パス"}}
- ファイル削除:   {{"type": "delete_file", "path": "相対パス"}}
- ユーザーへ表示: {{"type": "txt", "content": "メッセージ"}}
- タスク完了:     {{"type": "bot", "message": "完了メッセージ"}}

ツール実行結果は「[ツール実行結果]」として返ってきます。
全てのタスクが完了したら必ず {{"type": "bot", "message": "..."}} で終えてください。"#,
        root = root.display()
    )
}

// ─── Browser / Page ──────────────────────────────────────────────────────────

async fn get_ws_url(port: u16) -> anyhow::Result<String> {
    let url = format!("http://127.0.0.1:{port}/json/version");
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Ok(resp) = reqwest::get(&url).await {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(ws) = json["webSocketDebuggerUrl"].as_str() {
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
    loop {
        match page.find_element(selector).await {
            Ok(el) => return Ok(el),
            Err(_) => {
                if tokio::time::Instant::now() >= deadline {
                    anyhow::bail!("タイムアウト: セレクター '{selector}' が見つかりません");
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
    eprintln!("Loading Copilot...");
    page.goto("https://copilot.microsoft.com").await?;
    tokio::time::sleep(Duration::from_secs(5)).await;
    eprintln!("Waiting for input...");
    wait_for_element(&page, "#userInput", 20).await?;
    eprintln!("Ready.");
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
        let port = free_port();
        let mut edge = launch_edge(port)?;
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
        let (browser, mut handler) = Browser::connect(&ws_url).await?;
        let handle = tokio::spawn(async move {
            while let Some(h) = handler.next().await {
                if let Err(e) = h {
                    eprintln!("Handler: {e}");
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
    eprintln!("Waiting for response...");
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if ai_message_count(page).await.unwrap_or(0) >= n {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("Timed out waiting for response");
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
            eprintln!("Stable {stable}/5 ({} chars)", text.len());
            if stable >= 5 {
                return Ok(text);
            }
        } else if !text.is_empty() {
            eprintln!("Generating... ({} chars)", text.len());
            stable = 0;
            last = text;
        }

        if tokio::time::Instant::now() >= deadline {
            if !last.is_empty() {
                return Ok(last);
            }
            anyhow::bail!("Timed out waiting for response text");
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

async fn run_agent(
    session: &mut CopilotSession,
    root: &std::path::Path,
    user_task: &str,
) -> anyhow::Result<()> {
    let mut prompt = user_task.to_string();

    for turn in 0..MAX_TURNS {
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
            eprintln!("コマンドが取得できませんでした");
            break;
        }

        let (exec_results, messages) = execute(root, &commands);
        tool_results.extend(exec_results);

        for msg in &messages {
            println!("\n[AI] {msg}");
        }

        if tool_results.is_empty() {
            break;
        }

        for r in &tool_results {
            println!("[{}] {}", r.label, r.output);
        }

        if turn + 1 == MAX_TURNS {
            eprintln!("最大ターン数 ({MAX_TURNS}) に達しました。タスクを中断します。");
            break;
        }

        prompt = format_tool_results(&tool_results);
    }

    Ok(())
}

// ─── main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use std::io::{self, BufRead, Write};

    // CLI 引数でプロジェクトディレクトリを受け取る（省略時はカレントディレクトリ）
    let root_arg = std::env::args().nth(1);
    let root = match root_arg {
        Some(ref p) => std::path::PathBuf::from(p),
        None => std::env::current_dir()?,
    }
    .canonicalize()?;

    eprintln!("プロジェクトルート: {}", root.display());

    let mut session = CopilotSession::start().await?;
    eprintln!("Copilot に接続しました。");

    eprintln!("システムプロンプト送信中...");
    session.send_raw(&build_system_prompt(&root)).await?;
    eprintln!("準備完了。");

    println!(
        "ToyClaudeCode へようこそ。[{}] のタスクを入力してください（終了: exit）",
        root.display()
    );

    let stdin = io::stdin();
    loop {
        print!("\n> ");
        io::stdout().flush()?;

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                eprintln!("入力エラー: {e}");
                break;
            }
        }

        let task = line.trim();
        if task.is_empty() {
            continue;
        }
        if task == "exit" || task == "quit" {
            break;
        }

        if let Err(e) = run_agent(&mut session, &root, task).await {
            eprintln!("エラー: {e}");
        }
    }

    println!("終了します");
    drop(session);
    Ok(())
}
