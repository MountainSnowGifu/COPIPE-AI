use std::time::Duration;

// ─── スクロール ───────────────────────────────────────────────────────────────

pub(super) async fn scroll_to_selector(page: &chromiumoxide::Page, selector: &str) {
    let js = format!(
        r#"(function() {{
            const el = document.querySelector({sel});
            if (el) el.scrollIntoView({{behavior: 'smooth', block: 'center'}});
        }})()"#,
        sel = serde_json::to_string(selector).unwrap_or_default()
    );
    page.evaluate_expression(&js).await.ok();
}

pub(super) async fn scroll_to_nth_ai_message(page: &chromiumoxide::Page, n: usize) {
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

// ─── DOM 読み取り ─────────────────────────────────────────────────────────────

pub(crate) async fn ai_message_count(page: &chromiumoxide::Page) -> anyhow::Result<usize> {
    let n = tokio::time::timeout(
        Duration::from_secs(8),
        page.evaluate_expression(
            r#"document.querySelectorAll('[data-testid="ai-message"]').length"#,
        ),
    )
    .await
    .map_err(|_| anyhow::anyhow!("ai_message_count タイムアウト (8s)"))?
    ?
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

pub(crate) async fn read_nth_ai_text(page: &chromiumoxide::Page, n: usize) -> String {
    let js = format!(
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
    );
    let raw = tokio::time::timeout(Duration::from_secs(8), page.evaluate_expression(&js))
        .await
        .ok()
        .and_then(|r| r.ok())
        .and_then(|r| r.value().and_then(|v| v.as_str().map(|s| s.to_string())))
        .unwrap_or_default();

    extract_ai_text(&raw)
}

pub(crate) async fn get_codeblocks_from_dom(page: &chromiumoxide::Page, n: usize) -> Vec<String> {
    let _ = tokio::time::timeout(Duration::from_secs(3), scroll_to_nth_ai_message(page, n)).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let js = format!(
        r#"
            (() => {{
                const msgs = document.querySelectorAll('[data-testid="ai-message"]');
                if (msgs.length < {n}) return '';
                const el = msgs[{n} - 1];
                const blocks = [...el.querySelectorAll('pre > code')];
                return blocks.map(b => b.innerText.trim()).filter(t => t).join('\x00');
            }})()
        "#
    );
    let raw = tokio::time::timeout(Duration::from_secs(8), page.evaluate_expression(&js))
        .await
        .ok()
        .and_then(|r| r.ok())
        .and_then(|r| r.value().and_then(|v| v.as_str().map(|s| s.to_string())))
        .unwrap_or_default();

    if raw.is_empty() {
        return vec![];
    }
    raw.split('\x00')
        .map(|s| s.trim().to_string())
        .filter(|s| {
            let t = s.trim_start();
            t.starts_with('{') || t.starts_with('[')
        })
        .collect()
}

pub(super) fn looks_like_bot_challenge(text: &str) -> bool {
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
