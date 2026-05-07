mod browser;
mod command;
use browser::{free_port, launch_edge};
use command::parse_commands;

use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams;
use futures::StreamExt;
use std::process::Child;
use std::time::Duration;

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

async fn get_codeblocks_from_dom(page: &chromiumoxide::Page, n: usize) -> Vec<String> {
    let raw = page
        .evaluate_expression(&format!(
            r#"
            (() => {{
                const msgs = document.querySelectorAll('[data-testid="ai-message"]');
                if (msgs.length < {n}) return '';
                const el = msgs[{n} - 1];
                // pre > code のみ（インラインコードは除外）
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
        // JSON として妥当なブロックのみ採用
        .filter(|s| serde_json::from_str::<serde_json::Value>(s).is_ok())
        .collect()
}

fn extract_ai_text(raw: &str) -> String {
    let text = raw.trim();
    // 先頭の "Copilot の発言" プレフィックスを除去
    let text = text
        .strip_prefix("Copilot の発言\n")
        .or_else(|| text.strip_prefix("Copilot の発言"))
        .unwrap_or(text);
    // 末尾の UI テキスト（ページ内編集ボタン、引用リンクなど）を切り落とす
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

struct CopilotSession {
    page: chromiumoxide::Page,
    edge: Child,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for CopilotSession {
    fn drop(&mut self) {
        self.handle.abort();
        self.edge.kill().ok();
        self.edge.wait().ok(); // zombie 化防止
    }
}

async fn focus_input(page: &chromiumoxide::Page) -> anyhow::Result<()> {
    wait_for_element(page, "#userInput", 10).await?;
    page.evaluate_expression("document.querySelector('#userInput').focus()")
        .await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    Ok(())
}

async fn set_input_value(page: &chromiumoxide::Page, prompt: &str) -> anyhow::Result<()> {
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
    Ok(())
}

async fn submit_input(page: &chromiumoxide::Page) -> anyhow::Result<()> {
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
    Ok(())
}

impl CopilotSession {
    async fn start() -> anyhow::Result<Self> {
        let port = free_port();
        let mut edge = launch_edge(port)?;

        // Browser::connect 以降の失敗でも edge を確実に回収するクロージャ
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
                // edge の kill は呼び出し元で行う
                let _ = edge;
                Err(e)
            }
        }
    }

    async fn send(&mut self, prompt: &str) -> anyhow::Result<String> {
        let page = &self.page;
        let baseline = ai_message_count(page).await?;

        focus_input(page).await?;
        set_input_value(page, prompt).await?;
        submit_input(page).await?;

        wait_for_nth_response(page, baseline + 1, 90).await
    }
}

async fn wait_for_nth_response(
    page: &chromiumoxide::Page,
    n: usize,
    timeout_secs: u64,
) -> anyhow::Result<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    wait_for_ai_message_count(page, n, deadline).await?;
    wait_for_stable_ai_text(page, n, deadline).await
}

async fn wait_for_ai_message_count(
    page: &chromiumoxide::Page,
    n: usize,
    deadline: tokio::time::Instant,
) -> anyhow::Result<()> {
    eprintln!("Waiting for response to start...");
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let count = ai_message_count(page).await.unwrap_or(0);
        if count >= n {
            eprintln!("Response detected.");
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("Timed out waiting for response to start");
        }
    }
}

async fn wait_for_stable_ai_text(
    page: &chromiumoxide::Page,
    n: usize,
    deadline: tokio::time::Instant,
) -> anyhow::Result<String> {
    eprintln!("Waiting for response to finish...");
    let mut last_text = String::new();
    let mut stable_secs = 0u64;

    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let text = read_nth_ai_text(page, n).await;

        if !text.is_empty() && text == last_text {
            stable_secs += 1;
            eprintln!("Stable {stable_secs}/10 ({} chars)", text.len());
            if stable_secs >= 10 {
                eprintln!("Response finished.");
                return Ok(text);
            }
        } else if !text.is_empty() {
            eprintln!("Generating... ({} chars)", text.len());
            stable_secs = 0;
            last_text = text;
        }

        if tokio::time::Instant::now() >= deadline {
            if !last_text.is_empty() {
                return Ok(last_text);
            }
            anyhow::bail!("Timed out waiting for response text");
        }
    }
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

async fn ai_message_count(page: &chromiumoxide::Page) -> anyhow::Result<usize> {
    let n = page
        .evaluate_expression(r#"document.querySelectorAll('[data-testid="ai-message"]').length"#)
        .await?
        .value()
        .and_then(|v| v.as_f64())
        .ok_or_else(|| anyhow::anyhow!("ai-message 数の取得に失敗"))?;
    Ok(n as usize)
}

fn print_blocks(blocks: &[String]) {
    for block in blocks {
        match parse_commands(block) {
            Ok(cmds) => {
                for cmd in &cmds {
                    println!("{cmd:#?}");
                }
            }
            Err(e) => {
                // デシリアライズ失敗時は生 JSON を表示
                eprintln!("デシリアライズ失敗 ({e}): {block}");
            }
        }
    }
    println!();
}

async fn send_with_json_retry(session: &mut CopilotSession, prompt: &str) -> anyhow::Result<()> {
    session.send(prompt).await?;
    let n = ai_message_count(&session.page).await?;
    let blocks = get_codeblocks_from_dom(&session.page, n).await;
    if !blocks.is_empty() {
        print_blocks(&blocks);
        return Ok(());
    }

    eprintln!("コードブロックなし → JSON で返すよう要求します");
    session
        .send("返答をコードブロック付きの JSON 形式で出力してください。")
        .await?;
    let n2 = ai_message_count(&session.page).await?;
    let blocks2 = get_codeblocks_from_dom(&session.page, n2).await;
    if !blocks2.is_empty() {
        print_blocks(&blocks2);
    } else {
        eprintln!("コードブロックが取得できませんでした");
        println!();
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use std::io::{self, BufRead, Write};

    let mut session = CopilotSession::start().await?;
    eprintln!("Copilot に接続しました。");

    // p.txt が存在すれば最初の質問として送る
    let initial_prompt = std::fs::read_to_string("p.txt")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    if let Some(prompt) = initial_prompt {
        eprintln!("p.txt を送信中...");
        if let Err(e) = send_with_json_retry(&mut session, &prompt).await {
            eprintln!("エラー: {e}");
        }
    }

    println!("質問を入力してください（終了: Ctrl+D または 'exit'）");

    let stdin = io::stdin();
    loop {
        print!("> ");
        io::stdout().flush()?;

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break, // EOF (Ctrl+D)
            Ok(_) => {}
            Err(e) => {
                eprintln!("入力エラー: {e}");
                break;
            }
        }

        let prompt = line.trim();
        if prompt.is_empty() {
            continue;
        }
        if prompt == "exit" || prompt == "quit" {
            break;
        }

        if let Err(e) = send_with_json_retry(&mut session, prompt).await {
            eprintln!("エラー: {e}");
        }
    }

    println!("終了します");
    drop(session); // Drop が kill + wait を実行
    Ok(())
}
