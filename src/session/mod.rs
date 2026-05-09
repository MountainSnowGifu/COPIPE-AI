mod browser;
mod dom;
mod page;
mod watchdog;

use browser::{free_port, launch_edge};
use chromiumoxide::browser::Browser;
use dom::{scroll_to_nth_ai_message, scroll_to_selector};
use futures::StreamExt;
use page::{get_ws_url, prepare_copilot_page, wait_for_element};
use std::process::Child;
use std::time::Duration;
use watchdog::{wait_for_ai_message_count, wait_for_stable_text};

pub(crate) use dom::{ai_message_count, get_codeblocks_from_dom, page_diagnostic};

// ─── ユーティリティ ───────────────────────────────────────────────────────────

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
                if let Err(_) = h {
                    // CDP切断エラーは終了時の正常シーケンスでも発生するため黙殺
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
        tokio::time::sleep(jitter(600, 400)).await;

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
        tokio::time::sleep(jitter(500, 400)).await;

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
        tokio::time::sleep(jitter(800, 600)).await;

        // Enter キー送信
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

        // 入力欄がまだ空でなければ送信ボタンをフォールバッククリック（二重送信防止）
        let target = baseline + 1;
        let input_still_has_text = page
            .evaluate_expression(r#"(document.querySelector('#userInput')?.value?.length ?? 0) > 0"#)
            .await
            .ok()
            .and_then(|r| r.value().and_then(|v| v.as_bool()))
            .unwrap_or(false);
        if input_still_has_text && ai_message_count(page).await.unwrap_or(0) < target {
            let click_result = page.evaluate_expression(r#"
                (function() {
                    const selectors = [
                        'button[aria-label*="Send"]', 'button[aria-label*="送信"]',
                        '[data-testid*="send"]', '[data-testid*="Send"]',
                        'button[type="submit"]', 'form button:last-of-type',
                    ];
                    for (const sel of selectors) {
                        const btn = document.querySelector(sel);
                        if (btn && !btn.disabled) { btn.click(); return 'clicked:' + sel; }
                    }
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
            // フォールバック詳細はファイルのみ（端末には出さない）
            if let Ok(log_dir) = std::env::current_dir().map(|d| d.join(".copipe_logs")) {
                use std::io::Write as IoWrite;
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log_dir.join("browser_log")) {
                    let _ = writeln!(f, "[送信フォールバック] {click_result}\n---");
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        wait_for_ai_message_count(page, target, 90).await?;
        wait_for_stable_text(page, target, 90).await?;
        scroll_to_nth_ai_message(page, target).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        Ok(())
    }

    /// Copilot の生成を停止する（Ctrl+C キャンセル時に呼ぶ）。
    /// 停止ボタンが見つからない場合は Escape キーを送信してフォールバック。
    pub async fn stop_generation(&self) {
        let result = self.page.evaluate_expression(r#"
            (function() {
                // 生成停止ボタンを探してクリック
                const selectors = [
                    '[aria-label*="Stop"]', '[aria-label*="停止"]',
                    '[data-testid*="stop"]', '[data-testid*="Stop"]',
                    'button[title*="Stop"]', 'button[title*="停止"]',
                ];
                for (const sel of selectors) {
                    const btn = document.querySelector(sel);
                    if (btn && !btn.disabled) {
                        btn.click();
                        return 'stopped:' + sel;
                    }
                }
                // フォールバック: Escape キー
                document.dispatchEvent(new KeyboardEvent('keydown', {
                    key: 'Escape', code: 'Escape', keyCode: 27, bubbles: true
                }));
                return 'escape_sent';
            })()
        "#).await;
        if let Ok(r) = result {
            if let Some(v) = r.value() {
                eprintln!("[停止] {}", v.as_str().unwrap_or(""));
            }
        }
    }
}
