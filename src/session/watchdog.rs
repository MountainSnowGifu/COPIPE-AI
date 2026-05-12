use super::dom::{ai_message_count, read_nth_ai_text, scroll_to_nth_ai_message};
use std::path::Path;
use std::time::Duration;

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const SPINNER_ASCII: &[&str] = &["-", "\\", "|", "/"];

fn spin(tick: u64) -> &'static str {
    if crate::color::use_unicode() {
        SPINNER[(tick as usize) % SPINNER.len()]
    } else {
        SPINNER_ASCII[(tick as usize) % SPINNER_ASCII.len()]
    }
}

/// 人間らしいアイドル動作（マウス移動 + 確率的スクロール）を発火する（bot 検知回避）
async fn idle_mouse_wiggle(page: &chromiumoxide::Page) {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);

    // マウス移動（小幅なベジェ軌跡）
    let x = 200.0 + (seed % 800) as f64;
    let y = 150.0 + ((seed / 800) % 400) as f64;
    let dx = ((seed % 30) as f64) - 15.0;
    let dy = (((seed / 30) % 30) as f64) - 15.0;

    // 約30%の確率でスクロール動作を混ぜる（読み進めている自然な動作）
    let do_scroll = (seed / 1000) % 10 < 3;
    let scroll_dy = if (seed / 100) % 2 == 0 {
        // ゆっくり下にスクロール（30〜80px）
        (30 + (seed % 50)) as i64
    } else {
        // 少し戻るスクロール（10〜30px）
        -((10 + (seed % 20)) as i64)
    };

    let scroll_js = if do_scroll {
        format!(
            "window.scrollBy({{top: {scroll_dy}, behavior: 'smooth'}});"
        )
    } else {
        String::new()
    };

    let js = format!(
        r#"document.dispatchEvent(new MouseEvent('mousemove', {{
            bubbles: true, clientX: {x}, clientY: {y}
        }}));
        document.dispatchEvent(new MouseEvent('mousemove', {{
            bubbles: true, clientX: {}, clientY: {}
        }}));
        {scroll_js}"#,
        x + dx,
        y + dy
    );
    tokio::time::timeout(Duration::from_secs(5), page.evaluate_expression(&js))
        .await
        .ok();
}

fn write_diag_log(log_dir: Option<&Path>, msg: &str) {
    if let Some(dir) = log_dir {
        let path = dir.join("browser_log");
        if path
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            return;
        }
        // Tokio ランタイムをブロックしないようファイル書き込みを別スレッドで実行
        let msg = format!("{msg}\n---");
        tokio::task::spawn_blocking(move || {
            use std::io::Write as IoWrite;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                let _ = writeln!(f, "{msg}");
            }
        });
    }
}

pub(super) async fn detect_copilot_block(page: &chromiumoxide::Page) -> Option<String> {
    let js = r#"
    (() => {
        const inp = window.__copipeFindInput ? window.__copipeFindInput() : document.querySelector('#userInput, textarea, [contenteditable="true"][role="textbox"], [role="textbox"][contenteditable="true"]');
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
    tokio::time::timeout(Duration::from_secs(8), page.evaluate_expression(js))
        .await
        .ok()
        .and_then(|r| r.ok())
        .and_then(|r| r.value().cloned())
        .and_then(|v| {
            if v.is_null() {
                None
            } else {
                v.as_str().map(|s| s.to_string())
            }
        })
}

pub(super) async fn wait_for_ai_message_count(
    page: &chromiumoxide::Page,
    n: usize,
    timeout_secs: u64,
    log_dir: Option<&Path>,
) -> anyhow::Result<()> {
    use std::io::Write as _;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let start = tokio::time::Instant::now();
    let mut check_block_at = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut wiggle_at = tokio::time::Instant::now() + Duration::from_secs(7);
    let mut tick: u64 = 0;
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        tick += 1;
        if ai_message_count(page).await.unwrap_or(0) >= n {
            return Ok(());
        }
        // 7秒ごとにアイドルマウス動作（bot 検知回避）
        if tokio::time::Instant::now() >= wiggle_at {
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            wiggle_at = tokio::time::Instant::now() + Duration::from_secs(5 + seed % 8); // 5〜12秒ごと
            idle_mouse_wiggle(page).await;
        }
        if tokio::time::Instant::now() >= deadline {
            eprintln!();
            anyhow::bail!(
                "Copilot が応答しませんでした。しばらく待ってから同じタスクを再入力してください"
            );
        }
        if tokio::time::Instant::now() >= check_block_at {
            check_block_at = tokio::time::Instant::now() + Duration::from_secs(10);
            // 診断情報は root ベースのログファイルのみ（端末には出さない）
            let diag_js = r#"
                (function() {
                    const inp = window.__copipeFindInput ? window.__copipeFindInput() : document.querySelector('#userInput, textarea, [contenteditable="true"][role="textbox"], [role="textbox"][contenteditable="true"]');
                    const allBtns = [...document.querySelectorAll('button')].map(b => ({
                        aria: b.getAttribute('aria-label') || null,
                        testid: b.getAttribute('data-testid') || null,
                        disabled: b.disabled,
                        text: b.textContent.trim().slice(0, 20) || null,
                    }));
                    return JSON.stringify({
                        input_exists: !!inp,
                        input_disabled: inp ? inp.disabled : null,
                        input_value_len: inp ? (('value' in inp ? inp.value : (inp.innerText || inp.textContent || '')).length) : 0,
                        ai_msg_count: document.querySelectorAll('[data-testid="ai-message"]').length,
                        all_btns: allBtns,
                    });
                })()
            "#;
            let input_state = tokio::time::timeout(
                Duration::from_secs(8),
                page.evaluate_expression(diag_js),
            )
            .await
            .ok()
            .and_then(|r| r.ok())
            .and_then(|r| r.value().and_then(|v| v.as_str().map(|s| s.to_string())))
            .unwrap_or_else(|| "{}".to_string());
            write_diag_log(log_dir, &format!("[診断] {input_state}"));
            if let Some(reason) = detect_copilot_block(page).await {
                anyhow::bail!(
                    "応答が停止しました（{reason}）。同じタスクを再入力してください"
                );
            }
        }
        let secs = start.elapsed().as_secs();
        // \r で上書きする進捗表示（診断 eprintln! と競合しないよう stderr flush）
        eprint!("\r  {} 応答待機中 {secs}s          ", spin(tick));
        std::io::stderr().flush().ok();
    }
}

pub(super) async fn wait_for_stable_text(
    page: &chromiumoxide::Page,
    n: usize,
    timeout_secs: u64,
    _log_dir: Option<&Path>,
) -> anyhow::Result<String> {
    use std::io::Write as _;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let mut last = String::new();
    let mut stable = 0u64;
    let mut check_block_at = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut wiggle_at = tokio::time::Instant::now() + Duration::from_secs(10);

    const POLL_MS: u64 = 1_000;
    const STABLE_NEEDED: u64 = 7; // 1000ms × 7 = 7秒安定を要求
    const SETTLE_MS: u64 = 3_000; // 安定確認後の追加バッファ → 合計 ~10秒
    let mut tick: u64 = 0;

    loop {
        tokio::time::sleep(Duration::from_millis(POLL_MS)).await;
        tick += 1;
        let text = read_nth_ai_text(page, n).await;

        if !text.is_empty() && text == last {
            stable += 1;
            eprint!(
                "\r  {} 受信確認中 ({}/{STABLE_NEEDED})          ",
                spin(tick),
                stable
            );
            std::io::stderr().flush().ok();
            if stable >= STABLE_NEEDED {
                tokio::time::sleep(Duration::from_millis(SETTLE_MS)).await;
                eprintln!();
                return Ok(text);
            }
        } else if !text.is_empty() {
            eprint!("\r  {} 生成中... {} 文字          ", spin(tick), text.len());
            std::io::stderr().flush().ok();
            stable = 0;
            last = text;
            let _ = tokio::time::timeout(Duration::from_secs(3), scroll_to_nth_ai_message(page, n))
                .await;
        }

        // アイドルマウス動作（生成中も自然な操作感を維持）
        if tokio::time::Instant::now() >= wiggle_at {
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            wiggle_at = tokio::time::Instant::now() + Duration::from_secs(8 + seed % 10);
            idle_mouse_wiggle(page).await;
        }

        if tokio::time::Instant::now() >= deadline {
            eprintln!();
            if !last.is_empty() {
                return Ok(last);
            }
            anyhow::bail!("Copilot の応答が途中で止まりました。同じタスクを再入力してください");
        }

        if tokio::time::Instant::now() >= check_block_at {
            check_block_at = tokio::time::Instant::now() + Duration::from_secs(15);
            if let Some(reason) = detect_copilot_block(page).await {
                eprintln!();
                if !last.is_empty() {
                    // 部分レスポンスで続行するが、ユーザーに通知を返す
                    // 呼び出し元が tool_results に混ぜて表示できるよう、テキストに警告を付加
                    let warned = format!(
                        "{last}\n\n⚠ Copilot がレート制限/ブロックを報告しました（{reason}）。応答が途中の可能性があります。"
                    );
                    return Ok(warned);
                }
                anyhow::bail!(
                    "Copilot との接続が切れました（{reason}）。同じタスクを再入力してください"
                );
            }
        }
    }
}
