use super::dom::{ai_message_count, read_nth_ai_text, scroll_to_nth_ai_message};
use std::time::Duration;

pub(super) async fn detect_copilot_block(page: &chromiumoxide::Page) -> Option<String> {
    let js = r#"
    (() => {
        const inp = document.querySelector('#userInput');
        if (!inp) return 'input_missing';
        if (inp.disabled || inp.getAttribute('aria-disabled') === 'true') return 'input_disabled';

        const errorEl = document.querySelector('[role="alert"], [data-testid*="error"], .error-message');
        const body = errorEl ? errorEl.innerText : '';
        const patterns = [
            'something went wrong',
            '制限に達しました', 'limit reached', '応答を生成できません',
            'Unable to generate', 'conversation is too long', '会話が長すぎます',
            'server error',
        ];
        for (const p of patterns) {
            if (body.toLowerCase().includes(p.toLowerCase())) return 'block:' + p;
        }
        return null;
    })()
    "#;
    page.evaluate_expression(js)
        .await
        .ok()
        .and_then(|r| r.value().cloned())
        .and_then(|v| if v.is_null() { None } else { v.as_str().map(|s| s.to_string()) })
}

pub(super) async fn wait_for_ai_message_count(
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
        if tokio::time::Instant::now() >= check_block_at {
            check_block_at = tokio::time::Instant::now() + Duration::from_secs(10);
            let input_state = page.evaluate_expression(r#"
                (function() {
                    const inp = document.querySelector('#userInput');
                    const allBtns = [...document.querySelectorAll('button')].map(b => ({
                        id: b.id || null,
                        aria: b.getAttribute('aria-label') || null,
                        testid: b.getAttribute('data-testid') || null,
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

pub(super) async fn wait_for_stable_text(
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
