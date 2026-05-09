use super::dom::{looks_like_bot_challenge, page_diagnostic};
use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::emulation::{
    SetAutomationOverrideParams, SetDeviceMetricsOverrideParams,
    SetHardwareConcurrencyOverrideParams, SetLocaleOverrideParams, SetTimezoneOverrideParams,
    SetUserAgentOverrideParams, UserAgentBrandVersion, UserAgentMetadata,
};
use chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams;
use std::time::Duration;

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36 Edg/124.0.0.0";
const ACCEPT_LANGUAGE: &str = "ja-JP,ja;q=0.9,en-US;q=0.8,en;q=0.7";

const ANTI_BOT_JS: &str = r#"
Object.defineProperty(navigator, 'webdriver', {get: () => undefined});

for (const key of ['cdc_adoQpoasnfa76pfcZLmcfl_Array', 'cdc_adoQpoasnfa76pfcZLmcfl_Promise', 'cdc_adoQpoasnfa76pfcZLmcfl_Symbol']) {
    try { delete window[key]; } catch (_) {}
}

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

Object.defineProperty(navigator, 'languages', {
    get: () => ['ja-JP', 'ja', 'en-US', 'en']
});

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
    Object.defineProperty(window, 'outerWidth',  {get: () => 1280});
    Object.defineProperty(window, 'outerHeight', {get: () => 800});
    Object.defineProperty(window, 'screenX',     {get: () => 100});
    Object.defineProperty(window, 'screenY',     {get: () => 50});
    Object.defineProperty(window, 'screenLeft',  {get: () => 100});
    Object.defineProperty(window, 'screenTop',   {get: () => 50});
} catch (_) {}

try { document.hasFocus = () => true; } catch(_) {}

try {
    ['domAutomation', 'domAutomationController',
     '__webdriver_script_fn', '__webdriver_script_func',
     '__selenium_unwrapped', '__fxdriver_unwrapped',
     '_phantom', 'callPhantom', '__nightmare',
     '__puppeteer_evaluation_script__',
    ].forEach(k => { try { delete window[k]; } catch(_) {} });
} catch(_) {}

try {
    if (!navigator.getBattery) {
        navigator.getBattery = () => Promise.resolve({
            charging: true, chargingTime: 0, dischargingTime: Infinity, level: 1,
            addEventListener: function() {}, removeEventListener: function() {},
        });
    }
} catch (_) {}

window.chrome = {
    app: {
        isInstalled: false,
        InstallState: {DISABLED:'disabled', INSTALLED:'installed', NOT_INSTALLED:'not_installed'},
        RunningState: {CANNOT_RUN:'cannot_run', READY_TO_RUN:'ready_to_run', RUNNING:'running'},
        getDetails: function() {}, getIsInstalled: function() {},
        installState: function() {}, runningState: function() {},
    },
    runtime: {
        OnInstalledReason: {CHROME_UPDATE:'chrome_update',INSTALL:'install',SHARED_MODULE_UPDATE:'shared_module_update',UPDATE:'update'},
        OnRestartRequiredReason: {APP_UPDATE:'app_update',OS_UPDATE:'os_update',PERIODIC:'periodic'},
        PlatformArch: {ARM:'arm',ARM64:'arm64',MIPS:'mips',MIPS64:'mips64',X86_32:'x86-32',X86_64:'x86-64'},
        PlatformNaclArch: {ARM:'arm',MIPS:'mips',MIPS64:'mips64',X86_32:'x86-32',X86_64:'x86-64'},
        PlatformOs: {ANDROID:'android',CROS:'cros',LINUX:'linux',MAC:'mac',OPENBSD:'openbsd',WIN:'win'},
        RequestUpdateCheckStatus: {NO_UPDATE:'no_update',THROTTLED:'throttled',UPDATE_AVAILABLE:'update_available'},
        connect: function() {}, sendMessage: function() {}, id: undefined,
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
            wasFetchedViaSpdy: true, wasNpnNegotiated: true,
            npnNegotiatedProtocol: 'h2', wasAlternateProtocolAvailable: false,
            connectionInfo: 'h2',
        };
    },
};

try {
    const _origQuery = navigator.permissions.query.bind(navigator.permissions);
    navigator.permissions.query = params =>
        params.name === 'notifications'
            ? Promise.resolve({ state: 'prompt', onchange: null })
            : _origQuery(params);
} catch (_) {}

try {
    const _toDataURL = HTMLCanvasElement.prototype.toDataURL;
    HTMLCanvasElement.prototype.toDataURL = function(type, ...args) {
        const ctx = this.getContext('2d');
        if (ctx) {
            const d = ctx.getImageData(0, 0, this.width || 1, this.height || 1);
            d.data[0] ^= 1;
            ctx.putImageData(d, 0, 0);
        }
        return _toDataURL.call(this, type, ...args);
    };
} catch(_) {}

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

// CDP の Function.prototype.toString() 偽装除去
// CDP は関数を wrap するため toString() で "native code" でない実装が露出する場合がある
try {
    const _toString = Function.prototype.toString;
    Function.prototype.toString = function() {
        if (this === Function.prototype.toString) return 'function toString() { [native code] }';
        return _toString.call(this);
    };
} catch(_) {}

// Notification.permission を denied に（実際のブラウザと同様）
try {
    if (window.Notification) {
        Object.defineProperty(Notification, 'permission', { get: () => 'denied' });
    }
} catch(_) {}

// performance.now() にわずかなノイズを乗せてフィンガープリントを揺らす
try {
    const _perfNow = Performance.prototype.now;
    Performance.prototype.now = function() {
        return _perfNow.call(this) + (Math.random() * 0.1);
    };
} catch(_) {}

// CDP が追加する runtime 関連プロパティを削除
try {
    for (const key of Object.getOwnPropertyNames(window)) {
        if (key.startsWith('cdc_') || key.startsWith('__cdc_') || key.startsWith('chrome_')) {
            try { delete window[key]; } catch(_) {}
        }
    }
} catch(_) {}

// devicePixelRatio を自然な値に設定
try {
    Object.defineProperty(window, 'devicePixelRatio', { get: () => 1.25 });
} catch(_) {}
"#;

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

pub(super) async fn get_ws_url(port: u16) -> anyhow::Result<String> {
    use std::io::Write as _;
    let url = format!("http://127.0.0.1:{port}/json/version");
    for i in 0..30 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let secs = (i + 1) / 2;
        print!("\r  ブラウザ接続中... {secs}s          ");
        std::io::stdout().flush().ok();
        if let Ok(resp) = reqwest::get(&url).await {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(ws) = json["webSocketDebuggerUrl"].as_str() {
                    print!("\r                                      \r");
                    std::io::stdout().flush().ok();
                    return Ok(ws.to_string());
                }
            }
        }
    }
    println!();
    anyhow::bail!("Edge の CDP に接続できませんでした (port {port})")
}

pub(super) async fn wait_for_element(
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

pub(super) async fn prepare_copilot_page(browser: &Browser) -> anyhow::Result<chromiumoxide::Page> {
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
    page.execute(SetLocaleOverrideParams::builder().locale("ja_JP").build()).await.ok();
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
    page.execute(SetAutomationOverrideParams::new(false)).await.ok();
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
    page.execute(AddScriptToEvaluateOnNewDocumentParams::new(ANTI_BOT_JS)).await?;
    use std::io::Write as _;
    page.goto("https://copilot.microsoft.com").await?;
    for s in 1..=5u64 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        print!("\r  ページ初期化中... {s}/5s          ");
        std::io::stdout().flush().ok();
    }
    print!("\r                                   \r");
    std::io::stdout().flush().ok();

    // 入力欄を待機。見つからない場合は BOT チャレンジを確認してユーザーに回復を促す
    if let Err(_) = wait_for_element(&page, "#userInput", 20).await {
        let diag = page_diagnostic(&page).await;
        if looks_like_bot_challenge(&diag) {
            println!();
            println!("⚠ ログイン/チャレンジ画面が検出されました。");
            println!("  ブラウザでログインまたは確認を完了してから Enter を押してください...");
            println!("  （120秒後に自動タイムアウトします）");
            std::io::stdout().flush().ok();
            // spawn_blocking + timeout で stdin を非同期安全に待機
            let wait_result = tokio::time::timeout(
                Duration::from_secs(120),
                tokio::task::spawn_blocking(|| {
                    let mut buf = String::new();
                    std::io::stdin().read_line(&mut buf).ok();
                }),
            ).await;
            if wait_result.is_err() {
                anyhow::bail!("ログイン待機がタイムアウトしました（120秒）。再実行してください");
            }
            // 再待機（最大60秒）
            wait_for_element(&page, "#userInput", 60).await
                .map_err(|_| anyhow::anyhow!("ログイン後も入力欄が見つかりませんでした。ブラウザを確認してください"))?;
        } else {
            anyhow::bail!("Copilot の入力欄が見つかりません。ブラウザを確認してください");
        }
    }
    Ok(page)
}
