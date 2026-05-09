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
    pub(crate) log_dir: Option<std::path::PathBuf>,
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
            Ok((page, handle)) => Ok(Self { page, edge, handle, log_dir: None }),
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
        tokio::time::sleep(jitter(700, 500)).await;

        // Bézier 曲線軌跡でマウスを入力欄に移動してクリック
        page.evaluate_expression(r#"
            (function() {
                const el = document.querySelector('#userInput');
                if (!el) return;
                const r = el.getBoundingClientRect();
                const tx = r.left + r.width  * (0.35 + Math.random() * 0.3);
                const ty = r.top  + r.height * (0.35 + Math.random() * 0.3);
                // 現在のマウス位置の推定（画面中央付近 + ノイズ）
                const sx = window.innerWidth  * (0.3 + Math.random() * 0.4);
                const sy = window.innerHeight * (0.3 + Math.random() * 0.4);
                // Bézier 制御点（軌跡に自然な弧を作る）
                const cx = sx + (tx - sx) * (0.3 + Math.random() * 0.4) + (Math.random() - 0.5) * 120;
                const cy = sy + (ty - sy) * (0.3 + Math.random() * 0.4) + (Math.random() - 0.5) * 80;
                const steps = 12 + Math.floor(Math.random() * 8);
                for (let i = 0; i <= steps; i++) {
                    const t = i / steps;
                    const u = 1 - t;
                    const mx = u*u*sx + 2*u*t*cx + t*t*tx;
                    const my = u*u*sy + 2*u*t*cy + t*t*ty;
                    el.dispatchEvent(new MouseEvent('mousemove', {bubbles:true, clientX:mx, clientY:my}));
                }
                const mo = {bubbles:true, cancelable:true, clientX:tx, clientY:ty, button:0};
                el.dispatchEvent(new MouseEvent('mousedown', mo));
                el.dispatchEvent(new MouseEvent('mouseup',   mo));
                el.dispatchEvent(new MouseEvent('click',     mo));
                el.focus();
            })()
        "#).await?;
        tokio::time::sleep(jitter(600, 400)).await;

        // テキスト入力: React setter を主軸にしつつ追加イベントで React state を確実に更新
        let js_str = serde_json::to_string(prompt)?;
        page.evaluate_expression(&format!(r#"
            (function() {{
                const el = document.querySelector('#userInput');
                if (!el) return 'not found';
                // React の native value setter で値をセット（React state が確実に更新される）
                const nativeSetter = Object.getOwnPropertyDescriptor(
                    window.HTMLTextAreaElement.prototype, 'value'
                ).set;
                nativeSetter.call(el, {js_str});
                // React が検知するイベントを順に発火
                el.dispatchEvent(new Event('input',  {{bubbles: true, composed: true}}));
                el.dispatchEvent(new Event('change', {{bubbles: true, composed: true}}));
                // InputEvent も発火（より詳細な変更通知）
                try {{
                    el.dispatchEvent(new InputEvent('input', {{
                        bubbles: true, composed: true,
                        inputType: 'insertText',
                        data: 'x'
                    }}));
                }} catch(_) {{}}
                return 'react_setter_ok';
            }})()
        "#)).await?;
        tokio::time::sleep(jitter(900, 700)).await;

        // Enter キー（Shift/Alt/Ctrl なし、より自然なイベントオブジェクト）
        page.evaluate_expression(r#"
            (function() {
                const el = document.querySelector('#userInput');
                if (!el) return;
                const base = {
                    key:'Enter', code:'Enter', keyCode:13, which:13, charCode:0,
                    bubbles:true, cancelable:true, composed:true,
                    shiftKey:false, altKey:false, ctrlKey:false, metaKey:false
                };
                el.dispatchEvent(new KeyboardEvent('keydown',  base));
                el.dispatchEvent(new KeyboardEvent('keypress', {...base, charCode:13}));
                el.dispatchEvent(new KeyboardEvent('keyup',    base));
            })()
        "#).await?;
        tokio::time::sleep(jitter(400, 250)).await;

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
                    // 優先セレクター（aria-label / testid）
                    const selectors = [
                        'button[aria-label*="Send"]', 'button[aria-label*="送信"]',
                        'button[aria-label*="メッセージ"]', 'button[aria-label*="message"]',
                        'button[aria-label*="submit"]', 'button[aria-label*="Submit"]',
                        '[data-testid*="send"]', '[data-testid*="Send"]',
                        '[data-testid*="submit"]', '[data-testid*="Submit"]',
                        'button[type="submit"]',
                    ];
                    for (const sel of selectors) {
                        const btn = document.querySelector(sel);
                        if (btn && !btn.disabled) { btn.click(); return 'clicked:' + sel; }
                    }
                    // 入力欄の近くにある有効ボタンを最大3階層上まで探す
                    const inp = document.querySelector('#userInput');
                    if (inp) {
                        let el = inp.parentElement;
                        for (let depth = 0; el && depth < 5; depth++, el = el.parentElement) {
                            const btns = [...el.querySelectorAll('button:not([disabled])')];
                            // 入力欄より右/下にあるボタンを優先
                            const inpRect = inp.getBoundingClientRect();
                            for (const btn of btns) {
                                const r = btn.getBoundingClientRect();
                                if (r.left >= inpRect.right - 10 || r.top >= inpRect.bottom - 10) {
                                    btn.click();
                                    return 'clicked:nearby@' + depth + ':' + (btn.getAttribute('aria-label') || btn.getAttribute('data-testid') || btn.className.slice(0,30) || 'unknown');
                                }
                            }
                        }
                    }
                    // 最終手段: form.requestSubmit() または form.submit()
                    const form = document.querySelector('#userInput')?.closest('form');
                    if (form) {
                        try { form.requestSubmit(); return 'form_requestSubmit'; } catch(_) {}
                        try { form.submit(); return 'form_submit'; } catch(_) {}
                    }
                    return 'no button found';
                })()
            "#).await.ok()
                .and_then(|r| r.value().and_then(|v| v.as_str().map(|s| s.to_string())))
                .unwrap_or_default();
            // フォールバック詳細はログファイルのみ（端末には出さない）
            if let Some(ref ld) = self.log_dir {
                let log_path = ld.join("browser_log");
                if !log_path.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                    use std::io::Write as IoWrite;
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log_path) {
                        let _ = writeln!(f, "[送信フォールバック] {click_result}\n---");
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        let log_dir_ref = self.log_dir.as_deref();
        wait_for_ai_message_count(page, target, 90, log_dir_ref).await?;
        wait_for_stable_text(page, target, 90, log_dir_ref).await?;
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
