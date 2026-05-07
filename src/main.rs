use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams;
use futures::StreamExt;
use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::Duration;

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("空きポートが見つかりません")
        .local_addr()
        .unwrap()
        .port()
}

fn launch_edge(port: u16) -> Child {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let profile_dir = format!("{home}/.config/microsoft-edge");
    let _ = std::fs::remove_file(format!("{profile_dir}/SingletonLock"));
    let _ = std::fs::remove_file(format!("{profile_dir}/SingletonSocket"));
    let _ = std::fs::remove_file(format!("{profile_dir}/SingletonCookie"));

    Command::new("/usr/bin/microsoft-edge")
        .arg(format!("--remote-debugging-port={port}"))
        .arg("--no-sandbox")
        .arg("--disable-dev-shm-usage")
        .arg("--disable-blink-features=AutomationControlled")
        .arg(format!("--user-data-dir={profile_dir}"))
        .env("DISPLAY", ":0")
        .env("WAYLAND_DISPLAY", "wayland-0")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("Edge の起動に失敗しました")
}

async fn get_ws_url(port: u16) -> anyhow::Result<String> {
    let url = format!("http://localhost:{port}/json/version");
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

/// 最後の ai-message 内の <pre><code> からコードブロックを取得
async fn get_codeblocks_from_dom(page: &chromiumoxide::Page, n: usize) -> Vec<String> {
    let raw = page
        .evaluate_expression(&format!(r#"
            (() => {{
                const msgs = document.querySelectorAll('[data-testid="ai-message"]');
                if (msgs.length < {n}) return '';
                const el = msgs[{n} - 1];
                const blocks = [...el.querySelectorAll('pre code, code')];
                return blocks.map(b => b.innerText.trim()).filter(t => t).join('\x00');
            }})()
        "#))
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
    let cutoffs = ["\nページ内で編集します", "\nFluentU", "\nhanabira", "\n参照:"];
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

impl CopilotSession {
    async fn start() -> anyhow::Result<Self> {
        let port = free_port();
        // 起動直後に失敗しても kill+wait できるよう先に保持
        let mut edge = launch_edge(port);
        let ws_url = match get_ws_url(port).await {
            Ok(u) => u,
            Err(e) => {
                edge.kill().ok();
                edge.wait().ok();
                return Err(e);
            }
        };

        let (browser, mut handler) = Browser::connect(&ws_url).await?;
        let handle = tokio::spawn(async move {
            while let Some(h) = handler.next().await {
                if let Err(e) = h {
                    eprintln!("Handler: {e}");
                    break;
                }
            }
        });

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

        eprintln!("Copilot を読み込み中...");
        page.goto("https://copilot.microsoft.com").await?;
        tokio::time::sleep(Duration::from_secs(5)).await;

        eprintln!("入力欄を待機中...");
        wait_for_element(&page, "#userInput", 20).await?;
        eprintln!("準備完了");

        Ok(Self { page, edge, handle })
    }

    async fn send(&mut self, prompt: &str) -> anyhow::Result<String> {
        let page = &self.page;

        // 送信前の ai-message 数を baseline として記録
        let baseline = page
            .evaluate_expression(
                r#"document.querySelectorAll('[data-testid="ai-message"]').length"#,
            )
            .await
            .ok()
            .and_then(|r| r.value().and_then(|v| v.as_f64()))
            .unwrap_or(0.0) as usize;

        // JS で focus
        wait_for_element(page, "#userInput", 10).await?;
        page.evaluate_expression("document.querySelector('#userInput').focus()").await?;
        tokio::time::sleep(Duration::from_millis(300)).await;

        // serde_json で安全な JS 文字列リテラルに変換
        let js_str = serde_json::to_string(prompt)?;
        page.evaluate_expression(&format!(r#"
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
        "#))
        .await?;
        tokio::time::sleep(Duration::from_millis(500)).await;

        // 送信
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

        // baseline + 1 番目の応答を待つ
        let expected = baseline + 1;
        let response = wait_for_nth_response(page, expected, 90).await?;
        Ok(response)
    }
}

async fn wait_for_nth_response(
    page: &chromiumoxide::Page,
    n: usize,
    timeout_secs: u64,
) -> anyhow::Result<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);

    // n番目の ai-message が出現するまで待つ
    eprintln!("応答開始を待機中...");
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let count = page
            .evaluate_expression(r#"document.querySelectorAll('[data-testid="ai-message"]').length"#)
            .await
            .ok()
            .and_then(|r| r.value().and_then(|v| v.as_f64()))
            .unwrap_or(0.0) as usize;
        if count >= n {
            eprintln!("応答開始を検出");
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("応答開始タイムアウト");
        }
    }

    // テキストが5秒安定するまで待つ
    eprintln!("生成完了を待機中...");
    let mut last_text = String::new();
    let mut stable_secs = 0u64;

    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;

        // n番目のメッセージを取得
        let raw = page
            .evaluate_expression(&format!(r#"
                (() => {{
                    const msgs = document.querySelectorAll('[data-testid="ai-message"]');
                    if (msgs.length < {n}) return '';
                    const el = msgs[{n} - 1];
                    const clone = el.cloneNode(true);
                    ['[data-testid="message-item-reactions"]', 'button', 'cite', '.supcontainer']
                        .forEach(sel => clone.querySelectorAll(sel).forEach(e => e.remove()));
                    return clone.innerText.trim();
                }})()
            "#))
            .await
            .ok()
            .and_then(|r| r.value().and_then(|v| v.as_str().map(|s| s.to_string())))
            .unwrap_or_default();

        let text = extract_ai_text(&raw);

        if !text.is_empty() && text == last_text {
            stable_secs += 1;
            eprintln!("安定中 {stable_secs}/10 ({} 文字)", text.len());
            if stable_secs >= 10 {
                eprintln!("生成完了");
                return Ok(text);
            }
        } else if !text.is_empty() {
            eprintln!("生成中... ({} 文字)", text.len());
            stable_secs = 0;
            last_text = text;
        }

        if tokio::time::Instant::now() >= deadline {
            if !last_text.is_empty() {
                return Ok(last_text);
            }
            anyhow::bail!("応答タイムアウト");
        }
    }
}

async fn ai_message_count(page: &chromiumoxide::Page) -> usize {
    page.evaluate_expression(
        r#"document.querySelectorAll('[data-testid="ai-message"]').length"#,
    )
    .await
    .ok()
    .and_then(|r| r.value().and_then(|v| v.as_f64()))
    .unwrap_or(0.0) as usize
}

async fn send_with_json_retry(session: &mut CopilotSession, prompt: &str) -> anyhow::Result<()> {
    session.send(prompt).await?;
    let n = ai_message_count(&session.page).await;
    let blocks = get_codeblocks_from_dom(&session.page, n).await;
    if !blocks.is_empty() {
        for block in &blocks {
            println!("{block}");
        }
        println!();
        return Ok(());
    }

    eprintln!("コードブロックなし → JSON で返すよう要求します");
    session.send("返答をコードブロック付きの JSON 形式で出力してください。").await?;
    let n2 = ai_message_count(&session.page).await;
    let blocks2 = get_codeblocks_from_dom(&session.page, n2).await;
    if !blocks2.is_empty() {
        for block in &blocks2 {
            println!("{block}");
        }
    } else {
        eprintln!("コードブロックが取得できませんでした");
    }
    println!();
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use std::io::{self, BufRead, Write};

    let mut session = CopilotSession::start().await?;
    eprintln!("Copilot に接続しました。");

    // p.txt が存在すれば最初の質問として送る
    let initial_prompt = std::fs::read_to_string("p.txt").ok()
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
