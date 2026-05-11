mod browser;
mod dom;
mod input;
mod page;
mod watchdog;

use browser::{free_port, launch_edge};
use chromiumoxide::browser::Browser;
use dom::{scroll_to_nth_ai_message, scroll_to_selector};
use futures::StreamExt;
use input::INPUT_SELECTOR;
use page::{dismiss_signin_later_safe, get_ws_url, prepare_copilot_page, wait_for_input};
use std::process::Child;
use std::time::Duration;
use watchdog::{wait_for_ai_message_count, wait_for_stable_text};

pub(crate) use dom::{
    ai_message_count, get_codeblocks_from_dom, page_diagnostic, read_nth_ai_text,
};

// ─── ユーティリティ ───────────────────────────────────────────────────────────

/// base_ms ± spread_ms/2 のランダムな待機時間を返す
fn jitter(base_ms: u64, spread_ms: u64) -> Duration {
    // subsec_nanos は同ミリ秒内で同値になる問題があるため as_nanos() 全体を使う
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    Duration::from_millis(base_ms + seed % spread_ms.max(1))
}

// ─── プロンプト分割 ───────────────────────────────────────────────────────────

const PROMPT_CHUNK_SIZE: usize = 30_000;

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
        println!("[1/3] ブラウザを起動中...");
        let port = free_port()?;
        let mut edge = launch_edge(port)?;
        let result = Self::init(port, &mut edge).await;
        match result {
            Ok((page, handle)) => Ok(Self {
                page,
                edge,
                handle,
                log_dir: None,
            }),
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
        println!("[2/3] Copilot に接続中...");
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
        println!("[3/3] セッションを初期化中...");
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

        dismiss_signin_later_safe(page).await;
        wait_for_input(page, 10).await?;
        scroll_to_selector(page, INPUT_SELECTOR).await;
        tokio::time::sleep(jitter(700, 500)).await;

        // Bézier 曲線軌跡でマウスを入力欄に移動してクリック
        page.evaluate_expression(include_str!("js/move_and_click.js"))
            .await?;
        tokio::time::sleep(jitter(600, 400)).await;

        // テキスト入力: React setter を主軸にしつつ追加イベントで React state を確実に更新
        let js_str = serde_json::to_string(prompt)?;
        page.evaluate_expression(&format!(
            "({})({})",
            include_str!("js/react_set_value.js"),
            js_str
        ))
        .await?;
        tokio::time::sleep(jitter(900, 700)).await;

        // 送信: 送信ボタンクリックを優先し、見つからない場合は Enter キーにフォールバック
        let target = baseline + 1;
        let btn_result = page
            .evaluate_expression(include_str!("js/send_button.js"))
            .await
            .ok()
            .and_then(|r| r.value().and_then(|v| v.as_str().map(|s| s.to_string())));

        let send_log_msg = if let Some(ref detail) = btn_result {
            // ボタンクリック成功
            tokio::time::sleep(jitter(400, 250)).await;
            format!("[送信:ボタン] {detail}\n---")
        } else {
            // ボタンが見つからない場合は Enter キーで試みる
            page.evaluate_expression(include_str!("js/send_enter.js"))
                .await?;
            tokio::time::sleep(jitter(400, 250)).await;

            // 入力欄にテキストが残っていれば最終手段としてボタン再試行
            let still_text = page
                .evaluate_expression(
                    r#"(function() {
                        const inp = window.__copipeFindInput ? window.__copipeFindInput() : document.querySelector('#userInput, textarea, [contenteditable="true"][role="textbox"], [role="textbox"][contenteditable="true"]');
                        if (!inp) return false;
                        if ('value' in inp) return (inp.value || '').length > 0;
                        return (inp.innerText || inp.textContent || '').length > 0;
                    })()"#,
                )
                .await
                .ok()
                .and_then(|r| r.value().and_then(|v| v.as_bool()))
                .unwrap_or(false);
            if still_text && ai_message_count(page).await.unwrap_or(0) < target {
                page.evaluate_expression(include_str!("js/send_button.js"))
                    .await
                    .ok();
                tokio::time::sleep(Duration::from_millis(500)).await;
                "[送信フォールバック:ボタン再試行]\n---".to_string()
            } else {
                "[送信:Enter]\n---".to_string()
            }
        };

        // 送信方法をログファイルに記録（端末には出さない）
        if let Some(ref ld) = self.log_dir {
            let log_path = ld.join("browser_log");
            if !log_path
                .symlink_metadata()
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
            {
                tokio::task::spawn_blocking(move || {
                    use std::io::Write as IoWrite;
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&log_path)
                    {
                        let _ = writeln!(f, "{send_log_msg}");
                    }
                });
            }
        }

        let log_dir_ref = self.log_dir.as_deref();
        wait_for_ai_message_count(page, target, 90, log_dir_ref).await?;
        wait_for_stable_text(page, target, 90, log_dir_ref).await?;
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            scroll_to_nth_ai_message(page, target),
        )
        .await;
        // 応答受信後の「読み返し」自然遅延（bot 検知回避）
        tokio::time::sleep(jitter(1_500, 2_000)).await;
        Ok(())
    }

    /// Copilot の生成を停止する（Ctrl+C キャンセル時に呼ぶ）。
    /// 停止ボタンが見つからない場合は Escape キーを送信してフォールバック。
    pub async fn stop_generation(&self) {
        let result = self
            .page
            .evaluate_expression(
                r#"
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
        "#,
            )
            .await;
        if let Ok(r) = result {
            if let Some(v) = r.value() {
                eprintln!("[停止] {}", v.as_str().unwrap_or(""));
            }
        }
    }
}
