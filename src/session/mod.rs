mod browser;
use browser::{free_port, launch_edge};
use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams;
use futures::StreamExt;
use std::process::Child;
use std::time::Duration;

/// ページ読み込み前に注入する BOT 検出回避スクリプト
const ANTI_BOT_JS: &str = r#"
// webdriver フラグを削除
Object.defineProperty(navigator, 'webdriver', {get: () => undefined});

// プラグイン一覧（空だと bot 判定されやすい）
Object.defineProperty(navigator, 'plugins', {
    get: () => {
        const p = [
            {name:'PDF Viewer',          filename:'internal-pdf-viewer', description:'Portable Document Format'},
            {name:'Chrome PDF Viewer',   filename:'internal-pdf-viewer', description:''},
            {name:'Chromium PDF Viewer', filename:'internal-pdf-viewer', description:''},
            {name:'Microsoft Edge PDF Viewer', filename:'internal-pdf-viewer', description:''},
            {name:'WebKit built-in PDF', filename:'internal-pdf-viewer', description:''},
        ];
        Object.setPrototypeOf(p, PluginArray.prototype);
        return p;
    }
});

// 言語設定（実際の Edge に合わせる）
Object.defineProperty(navigator, 'languages', {
    get: () => ['ja-JP', 'ja', 'en-US', 'en']
});

// window.chrome が存在しないと bot 判定される
if (!window.chrome) {
    window.chrome = {
        app: { isInstalled: false },
        runtime: {}
    };
}

// Permissions API: notifications の照会を自然な状態で返す
try {
    const _origQuery = navigator.permissions.query.bind(navigator.permissions);
    navigator.permissions.query = params =>
        params.name === 'notifications'
            ? Promise.resolve({ state: 'prompt', onchange: null })
            : _origQuery(params);
} catch (_) {}
"#;

/// base_ms ± spread_ms/2 のランダムな待機時間を返す（疑似乱数）
fn jitter(base_ms: u64, spread_ms: u64) -> Duration {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0) as u64;
    Duration::from_millis(base_ms + seed % spread_ms.max(1))
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

// ─── ページユーティリティ ─────────────────────────────────────────────────────

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
    page.execute(AddScriptToEvaluateOnNewDocumentParams::new(ANTI_BOT_JS))
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

pub(crate) async fn ai_message_count(page: &chromiumoxide::Page) -> anyhow::Result<usize> {
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

pub(crate) async fn get_codeblocks_from_dom(page: &chromiumoxide::Page, n: usize) -> Vec<String> {
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
    // JSON らしいブロック（{ or [ 始まり）をすべて返す。
    // 構文エラーのある JSON も parse_blocks に渡して詳細なエラーを出させる。
    raw.split('\x00')
        .map(|s| s.trim().to_string())
        .filter(|s| {
            let t = s.trim_start();
            t.starts_with('{') || t.starts_with('[')
        })
        .collect()
}

async fn wait_for_ai_message_count(
    page: &chromiumoxide::Page,
    n: usize,
    timeout_secs: u64,
) -> anyhow::Result<()> {
    use std::io::Write as _;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let start = tokio::time::Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if ai_message_count(page).await.unwrap_or(0) >= n {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            eprintln!();
            anyhow::bail!("応答の開始がタイムアウトしました");
        }
        let secs = start.elapsed().as_secs();
        eprint!("\r応答を待機中... ({secs}秒)          ");
        std::io::stderr().flush().ok();
    }
}

async fn wait_for_stable_text(
    page: &chromiumoxide::Page,
    n: usize,
    timeout_secs: u64,
) -> anyhow::Result<String> {
    use std::io::Write as _;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let mut last = String::new();
    let mut stable = 0u64;

    loop {
        tokio::time::sleep(Duration::from_millis(600)).await;
        let text = read_nth_ai_text(page, n).await;

        if !text.is_empty() && text == last {
            stable += 1;
            eprint!("\r安定確認中 {stable}/3 ({} 文字)          ", text.len());
            std::io::stderr().flush().ok();
            if stable >= 3 {
                eprintln!();
                return Ok(text);
            }
        } else if !text.is_empty() {
            eprint!("\r生成中... ({} 文字)          ", text.len());
            std::io::stderr().flush().ok();
            stable = 0;
            last = text;
        }

        if tokio::time::Instant::now() >= deadline {
            eprintln!();
            if !last.is_empty() {
                return Ok(last);
            }
            anyhow::bail!("応答テキストの取得がタイムアウトしました");
        }
    }
}

// ─── CopilotSession ──────────────────────────────────────────────────────────

pub struct CopilotSession {
    pub(crate) page: chromiumoxide::Page,
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
    pub async fn start() -> anyhow::Result<Self> {
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

    pub async fn send_raw(&mut self, prompt: &str) -> anyhow::Result<()> {
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
            if total > 1 {
                eprintln!("送信 {part}/{total} ({}文字)", msg.len());
            }
            self.send_raw_single(&msg).await?;
        }
        Ok(())
    }

    async fn send_raw_single(&mut self, prompt: &str) -> anyhow::Result<()> {
        let page = &self.page;
        let baseline = ai_message_count(page).await?;

        wait_for_element(page, "#userInput", 10).await?;

        // マウス移動 + クリックでフォーカス（JS focus() より自然な操作に見せる）
        page.evaluate_expression(r#"
            (function() {
                const el = document.querySelector('#userInput');
                if (!el) return;
                const r = el.getBoundingClientRect();
                const x = r.left + r.width  * 0.4 + Math.random() * r.width  * 0.2;
                const y = r.top  + r.height * 0.4 + Math.random() * r.height * 0.2;
                const mo = {bubbles: true, cancelable: true, clientX: x, clientY: y, button: 0};
                el.dispatchEvent(new MouseEvent('mousemove', mo));
                el.dispatchEvent(new MouseEvent('mousedown', mo));
                el.dispatchEvent(new MouseEvent('mouseup',   mo));
                el.dispatchEvent(new MouseEvent('click',     mo));
                el.focus();
            })()
        "#).await?;
        tokio::time::sleep(jitter(250, 150)).await;

        // テキストを React の value setter 経由でセット
        let js_str = serde_json::to_string(prompt)?;
        page.evaluate_expression(&format!(r#"
            (function() {{
                const el = document.querySelector('#userInput');
                if (!el) return 'not found';
                const setter = Object.getOwnPropertyDescriptor(
                    window.HTMLTextAreaElement.prototype, 'value'
                ).set;
                setter.call(el, {js_str});
                el.dispatchEvent(new Event('input',  {{bubbles: true}}));
                el.dispatchEvent(new Event('change', {{bubbles: true}}));
                return 'ok';
            }})()
        "#)).await?;
        tokio::time::sleep(jitter(450, 250)).await;

        // Enter キー（keyCode / which / charCode を含む完全なイベント列）
        page.evaluate_expression(r#"
            (function() {
                const el = document.querySelector('#userInput');
                if (!el) return;
                const ko = {key:'Enter', code:'Enter', keyCode:13, which:13, charCode:13,
                            bubbles:true, cancelable:true};
                el.dispatchEvent(new KeyboardEvent('keydown',  ko));
                el.dispatchEvent(new KeyboardEvent('keypress', ko));
                el.dispatchEvent(new KeyboardEvent('keyup',    ko));
            })()
        "#).await?;
        tokio::time::sleep(jitter(200, 150)).await;

        let target = baseline + 1;
        wait_for_ai_message_count(page, target, 90).await?;
        wait_for_stable_text(page, target, 90).await?;
        Ok(())
    }
}
