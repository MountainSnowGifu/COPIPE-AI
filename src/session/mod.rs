mod browser;
use browser::{free_port, launch_edge};
use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::emulation::{
    SetAutomationOverrideParams, SetDeviceMetricsOverrideParams,
    SetHardwareConcurrencyOverrideParams, SetLocaleOverrideParams, SetTimezoneOverrideParams,
    SetUserAgentOverrideParams, UserAgentBrandVersion, UserAgentMetadata,
};
use chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams;
use futures::StreamExt;
use std::process::Child;
use std::time::Duration;

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36 Edg/124.0.0.0";
const ACCEPT_LANGUAGE: &str = "ja-JP,ja;q=0.9,en-US;q=0.8,en;q=0.7";

/// ページ読み込み前に注入する BOT 検出回避スクリプト
const ANTI_BOT_JS: &str = r#"
// webdriver フラグを削除
Object.defineProperty(navigator, 'webdriver', {get: () => undefined});

// 自動化で露出しやすい Chrome Driver の痕跡を隠す
for (const key of ['cdc_adoQpoasnfa76pfcZLmcfl_Array', 'cdc_adoQpoasnfa76pfcZLmcfl_Promise', 'cdc_adoQpoasnfa76pfcZLmcfl_Symbol']) {
    try { delete window[key]; } catch (_) {}
}

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

// ハードウェア情報（ヘッドレスだと 0 や undefined になりがち）
Object.defineProperty(navigator, 'hardwareConcurrency', {get: () => 8});
Object.defineProperty(navigator, 'deviceMemory',        {get: () => 8});
Object.defineProperty(navigator, 'platform',            {get: () => 'Win32'});
Object.defineProperty(navigator, 'vendor',              {get: () => 'Google Inc.'});
Object.defineProperty(navigator, 'maxTouchPoints',      {get: () => 0});

try {
    Object.defineProperty(navigator, 'connection', {
        get: () => ({effectiveType: '4g', rtt: 50, downlink: 10, saveData: false})
    });
} catch (_) {}

try {
    Object.defineProperty(screen, 'availTop', {get: () => 0});
    Object.defineProperty(screen, 'availLeft', {get: () => 0});
    Object.defineProperty(screen, 'width',       {get: () => 1920});
    Object.defineProperty(screen, 'height',      {get: () => 1080});
    Object.defineProperty(screen, 'availWidth',  {get: () => 1920});
    Object.defineProperty(screen, 'availHeight', {get: () => 1040});
    Object.defineProperty(screen, 'colorDepth',  {get: () => 24});
    Object.defineProperty(screen, 'pixelDepth',  {get: () => 24});
    Object.defineProperty(window, 'outerWidth', {get: () => 1280});
    Object.defineProperty(window, 'outerHeight', {get: () => 800});
    Object.defineProperty(window, 'screenX',    {get: () => 100});
    Object.defineProperty(window, 'screenY',    {get: () => 50});
    Object.defineProperty(window, 'screenLeft', {get: () => 100});
    Object.defineProperty(window, 'screenTop',  {get: () => 50});
} catch (_) {}

// フォーカスが外れていると bot 判定されることがある
try { document.hasFocus = () => true; } catch(_) {}

// automation 特有の DOM 変数・Selenium/Puppeteer の痕跡を削除
try {
    ['domAutomation', 'domAutomationController',
     '__webdriver_script_fn', '__webdriver_script_func',
     '__selenium_unwrapped', '__fxdriver_unwrapped',
     '_phantom', 'callPhantom', '__nightmare',
     '__puppeteer_evaluation_script__',
    ].forEach(k => { try { delete window[k]; } catch(_) {} });
} catch(_) {}

// Battery API がない環境との差分を埋める
try {
    if (!navigator.getBattery) {
        navigator.getBattery = () => Promise.resolve({
            charging: true,
            chargingTime: 0,
            dischargingTime: Infinity,
            level: 1,
            addEventListener: function() {},
            removeEventListener: function() {},
        });
    }
} catch (_) {}

// window.chrome を完全な形で設定
window.chrome = {
    app: {
        isInstalled: false,
        InstallState: {DISABLED:'disabled', INSTALLED:'installed', NOT_INSTALLED:'not_installed'},
        RunningState: {CANNOT_RUN:'cannot_run', READY_TO_RUN:'ready_to_run', RUNNING:'running'},
        getDetails:    function() {},
        getIsInstalled:function() {},
        installState:  function() {},
        runningState:  function() {},
    },
    runtime: {
        OnInstalledReason: {CHROME_UPDATE:'chrome_update',INSTALL:'install',SHARED_MODULE_UPDATE:'shared_module_update',UPDATE:'update'},
        OnRestartRequiredReason: {APP_UPDATE:'app_update',OS_UPDATE:'os_update',PERIODIC:'periodic'},
        PlatformArch: {ARM:'arm',ARM64:'arm64',MIPS:'mips',MIPS64:'mips64',X86_32:'x86-32',X86_64:'x86-64'},
        PlatformNaclArch: {ARM:'arm',MIPS:'mips',MIPS64:'mips64',X86_32:'x86-32',X86_64:'x86-64'},
        PlatformOs: {ANDROID:'android',CROS:'cros',LINUX:'linux',MAC:'mac',OPENBSD:'openbsd',WIN:'win'},
        RequestUpdateCheckStatus: {NO_UPDATE:'no_update',THROTTLED:'throttled',UPDATE_AVAILABLE:'update_available'},
        connect:          function() {},
        sendMessage:      function() {},
        id: undefined,
    },
    csi: function() {
        return {startE: Date.now(), onloadT: Date.now(), pageT: Math.random() * 1000 + 500, tran: 15};
    },
    loadTimes: function() {
        const t = Date.now() / 1000;
        return {
            requestTime: t - Math.random() * 0.5 - 0.1,
            startLoadTime: t - Math.random() * 0.4,
            commitLoadTime: t - Math.random() * 0.3,
            finishDocumentLoadTime: t - Math.random() * 0.1,
            finishLoadTime: t,
            firstPaintTime: t - Math.random() * 0.05,
            firstPaintAfterLoadTime: 0,
            navigationType: 'Other',
            wasFetchedViaSpdy: true,
            wasNpnNegotiated: true,
            npnNegotiatedProtocol: 'h2',
            wasAlternateProtocolAvailable: false,
            connectionInfo: 'h2',
        };
    },
};

// Permissions API: notifications の照会を自然な状態で返す
try {
    const _origQuery = navigator.permissions.query.bind(navigator.permissions);
    navigator.permissions.query = params =>
        params.name === 'notifications'
            ? Promise.resolve({ state: 'prompt', onchange: null })
            : _origQuery(params);
} catch (_) {}

// Canvas フィンガープリントに微小ノイズを乗せる
try {
    const _toDataURL = HTMLCanvasElement.prototype.toDataURL;
    HTMLCanvasElement.prototype.toDataURL = function(type, ...args) {
        const ctx = this.getContext('2d');
        if (ctx) {
            const d = ctx.getImageData(0, 0, this.width || 1, this.height || 1);
            d.data[0] ^= 1; // 1ビットだけ変化させる（視覚的に無影響）
            ctx.putImageData(d, 0, 0);
        }
        return _toDataURL.call(this, type, ...args);
    };
} catch(_) {}

// WebGL ベンダー・レンダラーを一般的な値に偽装
try {
    const _getParam = WebGLRenderingContext.prototype.getParameter;
    WebGLRenderingContext.prototype.getParameter = function(p) {
        if (p === 37445) return 'Intel Inc.';
        if (p === 37446) return 'Intel Iris OpenGL Engine';
        return _getParam.call(this, p);
    };
    const _getParam2 = WebGL2RenderingContext.prototype.getParameter;
    WebGL2RenderingContext.prototype.getParameter = function(p) {
        if (p === 37445) return 'Intel Inc.';
        if (p === 37446) return 'Intel Iris OpenGL Engine';
        return _getParam2.call(this, p);
    };
} catch(_) {}
"#;

/// base_ms ± spread_ms/2 のランダムな待機時間を返す（疑似乱数）
fn jitter(base_ms: u64, spread_ms: u64) -> Duration {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0) as u64;
    Duration::from_millis(base_ms + seed % spread_ms.max(1))
}

fn user_agent_metadata() -> anyhow::Result<UserAgentMetadata> {
    Ok(UserAgentMetadata::builder()
        .brands([
            UserAgentBrandVersion::new("Chromium", "124"),
            UserAgentBrandVersion::new("Microsoft Edge", "124"),
            UserAgentBrandVersion::new("Not-A.Brand", "99"),
        ])
        .full_version_lists([
            UserAgentBrandVersion::new("Chromium", "124.0.0.0"),
            UserAgentBrandVersion::new("Microsoft Edge", "124.0.0.0"),
            UserAgentBrandVersion::new("Not-A.Brand", "99.0.0.0"),
        ])
        .platform("Windows")
        .platform_version("10.0.0")
        .architecture("x86")
        .model("")
        .mobile(false)
        .bitness("64")
        .wow64(false)
        .build()
        .map_err(|e| anyhow::anyhow!(e))?)
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

async fn scroll_to_selector(page: &chromiumoxide::Page, selector: &str) {
    let js = format!(
        r#"(function() {{
            const el = document.querySelector({sel});
            if (el) el.scrollIntoView({{behavior: 'smooth', block: 'center'}});
        }})()"#,
        sel = serde_json::to_string(selector).unwrap_or_default()
    );
    page.evaluate_expression(&js).await.ok();
}

async fn scroll_to_nth_ai_message(page: &chromiumoxide::Page, n: usize) {
    let js = format!(
        r#"(function() {{
            const msgs = document.querySelectorAll('[data-testid="ai-message"]');
            if (msgs.length >= {n}) {{
                msgs[{n} - 1].scrollIntoView({{behavior: 'smooth', block: 'start'}});
            }}
        }})()"#
    );
    page.evaluate_expression(&js).await.ok();
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
        SetUserAgentOverrideParams::builder()
            .user_agent(USER_AGENT)
            .accept_language(ACCEPT_LANGUAGE)
            .platform("Win32")
            .user_agent_metadata(user_agent_metadata()?)
            .build()
            .map_err(|e| anyhow::anyhow!(e))?,
    )
    .await?;
    page.execute(
        SetLocaleOverrideParams::builder()
            .locale("ja_JP")
            .build(),
    )
    .await
    .ok();
    page.execute(
        SetTimezoneOverrideParams::builder()
            .timezone_id("Asia/Tokyo")
            .build()
            .map_err(|e| anyhow::anyhow!(e))?,
    )
    .await
    .ok();
    page.execute(
        SetHardwareConcurrencyOverrideParams::builder()
            .hardware_concurrency(8i64)
            .build()
            .map_err(|e| anyhow::anyhow!(e))?,
    )
    .await
    .ok();
    page.execute(SetAutomationOverrideParams::new(false))
        .await
        .ok();
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
    if let Err(e) = wait_for_element(&page, "#userInput", 20).await {
        let diag = page_diagnostic(&page).await;
        if looks_like_bot_challenge(&diag) {
            anyhow::bail!(
                "Copilot の入力欄が見つかりません。BOT対策/ログイン/チャレンジ画面の可能性があります。ブラウザで手動確認後に再実行してください。\n診断: {diag}"
            );
        }
        return Err(e);
    }
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
    scroll_to_nth_ai_message(page, n).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
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

fn looks_like_bot_challenge(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "verify you are human",
        "checking your browser",
        "unusual activity",
        "captcha",
        "robot",
        "sign in",
        "サインイン",
        "ログイン",
        "本人確認",
        "人間であること",
        "通常と異なる",
        "access denied",
        "blocked",
        "403",
        "security check",
        "challenge",
        "automated",
        "bot detection",
        "アクセスが拒否",
        "セキュリティチェック",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

pub(crate) async fn page_diagnostic(page: &chromiumoxide::Page) -> String {
    page.evaluate_expression(
        r#"
        (() => {
            const text = document.body ? document.body.innerText : '';
            return JSON.stringify({
                url: location.href,
                title: document.title,
                text: text.replace(/\s+/g, ' ').slice(0, 1200)
            });
        })()
    "#,
    )
    .await
    .ok()
    .and_then(|r| r.value().and_then(|v| v.as_str().map(|s| s.to_string())))
    .unwrap_or_else(|| "{\"error\":\"page diagnostic unavailable\"}".to_string())
}

/// Copilot がエラー状態になっていないか確認する。
/// 入力欄の無効化やよくあるエラーテキストを検知してエラー文字列を返す。
async fn detect_copilot_block(page: &chromiumoxide::Page) -> Option<String> {
    let js = r#"
    (() => {
        // 入力欄が無効化されている
        const inp = document.querySelector('#userInput');
        if (!inp) return 'input_missing';
        if (inp.disabled || inp.getAttribute('aria-disabled') === 'true') return 'input_disabled';

        // ページ全体のテキストからエラーキーワードを検索（日英共通）
        const body = document.body ? document.body.innerText : '';
        const patterns = [
            '新しいトピック', 'new topic', 'something went wrong',
            '制限に達しました', 'limit reached', '応答を生成できません',
            'Unable to generate', 'conversation is too long', '会話が長すぎます',
            'エラーが発生しました', 'server error', '503', '429',
        ];
        for (const p of patterns) {
            if (body.toLowerCase().includes(p.toLowerCase())) return 'block:' + p;
        }
        return null;
    })()
    "#;
    let result = page
        .evaluate_expression(js)
        .await
        .ok()
        .and_then(|r| r.value().cloned())
        .and_then(|v| if v.is_null() { None } else { v.as_str().map(|s| s.to_string()) });
    result
}

async fn wait_for_ai_message_count(
    page: &chromiumoxide::Page,
    n: usize,
    timeout_secs: u64,
) -> anyhow::Result<()> {
    use std::io::Write as _;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let start = tokio::time::Instant::now();
    let mut check_block_at = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if ai_message_count(page).await.unwrap_or(0) >= n {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            eprintln!();
            anyhow::bail!("応答の開始がタイムアウトしました");
        }
        // 10秒ごとに Copilot のブロック状態を確認
        if tokio::time::Instant::now() >= check_block_at {
            check_block_at = tokio::time::Instant::now() + Duration::from_secs(10);
            // ページの入力欄・ボタン状態を詳細に取得してログへ
            let input_state = page.evaluate_expression(r#"
                (function() {
                    const inp = document.querySelector('#userInput');
                    // 全ボタンをスキャンして属性・位置を記録
                    const allBtns = [...document.querySelectorAll('button')].map(b => ({
                        tag: b.tagName,
                        id: b.id || null,
                        aria: b.getAttribute('aria-label') || null,
                        testid: b.getAttribute('data-testid') || null,
                        type: b.type || null,
                        disabled: b.disabled,
                        text: b.textContent.trim().slice(0, 20) || null,
                    }));
                    return JSON.stringify({
                        input_exists: !!inp,
                        input_disabled: inp ? inp.disabled : null,
                        input_value_len: inp ? inp.value.length : 0,
                        ai_msg_count: document.querySelectorAll('[data-testid="ai-message"]').length,
                        all_btns: allBtns,
                    });
                })()
            "#).await.ok()
                .and_then(|r| r.value().and_then(|v| v.as_str().map(|s| s.to_string())))
                .unwrap_or_else(|| "{}".to_string());
            eprintln!("\n[診断] {input_state}");
            // browser_log にも診断を書き込む
            if let Ok(log_dir) = std::env::current_dir().map(|d| d.join(".copipe_logs")) {
                use std::io::Write as IoWrite;
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log_dir.join("browser_log")) {
                    let _ = writeln!(f, "[診断] {input_state}\n---");
                }
            }
            if let Some(reason) = detect_copilot_block(page).await {
                eprintln!();
                anyhow::bail!("Copilot が応答を停止しました: {reason}");
            }
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
    let mut check_block_at = tokio::time::Instant::now() + Duration::from_secs(15);

    loop {
        tokio::time::sleep(Duration::from_millis(600)).await;
        let text = read_nth_ai_text(page, n).await;

        if !text.is_empty() && text == last {
            stable += 1;
            eprint!("\r安定確認中 {stable}/3 ({} 文字)          ", text.len());
            std::io::stderr().flush().ok();
            if stable >= 3 {
                // コードブロックのレンダリング完了を待つ追加バッファ
                tokio::time::sleep(Duration::from_millis(800)).await;
                eprintln!();
                return Ok(text);
            }
        } else if !text.is_empty() {
            eprint!("\r生成中... ({} 文字)          ", text.len());
            std::io::stderr().flush().ok();
            stable = 0;
            last = text;
            scroll_to_nth_ai_message(page, n).await;
        }

        if tokio::time::Instant::now() >= deadline {
            eprintln!();
            if !last.is_empty() {
                return Ok(last);
            }
            anyhow::bail!("応答テキストの取得がタイムアウトしました");
        }

        // 15秒ごとにブロック状態を確認（生成中は頻度を下げる）
        if tokio::time::Instant::now() >= check_block_at {
            check_block_at = tokio::time::Instant::now() + Duration::from_secs(15);
            if let Some(reason) = detect_copilot_block(page).await {
                eprintln!();
                if !last.is_empty() {
                    eprintln!("警告: Copilot ブロック検知 ({reason}) - 取得済みテキストで続行");
                    return Ok(last);
                }
                anyhow::bail!("Copilot が応答を停止しました: {reason}");
            }
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
        scroll_to_selector(page, "#userInput").await;
        tokio::time::sleep(jitter(600, 400)).await; // 600〜1000ms

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
        tokio::time::sleep(jitter(500, 400)).await; // 500〜900ms

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
        tokio::time::sleep(jitter(800, 600)).await; // 800〜1400ms

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
        tokio::time::sleep(jitter(300, 200)).await;

        // Enter で送信されなかった場合の送信ボタンフォールバック
        let target = baseline + 1;
        if ai_message_count(page).await.unwrap_or(0) < target {
            let click_result = page.evaluate_expression(r#"
                (function() {
                    // 優先セレクター
                    const selectors = [
                        'button[aria-label*="Send"]',
                        'button[aria-label*="送信"]',
                        '[data-testid*="send"]',
                        '[data-testid*="Send"]',
                        'button[type="submit"]',
                        'form button:last-of-type',
                    ];
                    for (const sel of selectors) {
                        const btn = document.querySelector(sel);
                        if (btn && !btn.disabled) {
                            btn.click();
                            return 'clicked:' + sel;
                        }
                    }
                    // 入力欄の隣にある有効ボタンを総当たり
                    const inp = document.querySelector('#userInput');
                    if (inp) {
                        const area = inp.closest('form, [role="form"], div');
                        if (area) {
                            for (const btn of area.querySelectorAll('button:not([disabled])')) {
                                btn.click();
                                return 'clicked:nearby:' + (btn.getAttribute('aria-label') || btn.getAttribute('data-testid') || 'unknown');
                            }
                        }
                    }
                    return 'no button found';
                })()
            "#).await.ok()
                .and_then(|r| r.value().and_then(|v| v.as_str().map(|s| s.to_string())))
                .unwrap_or_default();
            eprintln!("[送信フォールバック] {click_result}");
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        wait_for_ai_message_count(page, target, 90).await?;
        wait_for_stable_text(page, target, 90).await?;
        scroll_to_nth_ai_message(page, target).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        Ok(())
    }
}
