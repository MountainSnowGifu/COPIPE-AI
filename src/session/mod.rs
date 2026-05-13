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

// 隨渉隨渉隨渉 郢晢ｽｦ郢晢ｽｼ郢昴・縺・ｹ晢ｽｪ郢昴・縺・隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉

/// base_ms ・ゑｽｱ spread_ms/2 邵ｺ・ｮ郢晢ｽｩ郢晢ｽｳ郢敖郢晢｣ｰ邵ｺ・ｪ陟輔・・ｩ貊灘・鬮｢阮呻ｽ帝恆譁絶・
fn jitter(base_ms: u64, spread_ms: u64) -> Duration {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    Duration::from_millis(base_ms + seed % spread_ms.max(1))
}

// 隨渉隨渉隨渉 郢晏干ﾎ溽ｹ晢ｽｳ郢晏干繝ｨ陋ｻ繝ｻ迚｡ 隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉

const PROMPT_CHUNK_SIZE: usize = 7_000;

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

async fn input_has_text(page: &chromiumoxide::Page) -> bool {
    tokio::time::timeout(
        Duration::from_secs(5),
        page.evaluate_expression(include_str!("js/input_has_text.js")),
    )
    .await
    .ok()
    .and_then(|r| r.ok())
    .and_then(|r| r.value().and_then(|v| v.as_bool()))
    .unwrap_or(false)
}

async fn send_button(page: &chromiumoxide::Page) -> Option<String> {
    tokio::time::timeout(
        Duration::from_secs(10),
        page.evaluate_expression(include_str!("js/send_button.js")),
    )
    .await
    .ok()
    .and_then(|r| r.ok())
    .and_then(|r| r.value().and_then(|v| v.as_str().map(|s| s.to_string())))
}

async fn send_enter(page: &chromiumoxide::Page) -> anyhow::Result<String> {
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        page.evaluate_expression(include_str!("js/send_enter.js")),
    )
    .await
    .map_err(|_| anyhow::anyhow!("send_enter timeout"))??;

    Ok(result
        .value()
        .and_then(|v| v.as_str())
        .unwrap_or("enter")
        .to_string())
}

async fn wait_for_send_acceptance(
    page: &chromiumoxide::Page,
    target: usize,
    timeout: Duration,
) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if ai_message_count(page).await.unwrap_or(0) >= target {
            return true;
        }
        if !input_has_text(page).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(350)).await;
    }
    false
}

// 隨渉隨渉隨渉 CopilotSession 隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉隨渉

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
        println!("[1/3] Edge ブラウザを起動中...");
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
                    // CDP陋ｻ繝ｻ螯咏ｹｧ・ｨ郢晢ｽｩ郢晢ｽｼ邵ｺ・ｯ驍ｨ繧・ｽｺ繝ｻ蜃ｾ邵ｺ・ｮ雎・ｽ｣陝ｶ・ｸ郢ｧ・ｷ郢晢ｽｼ郢ｧ・ｱ郢晢ｽｳ郢ｧ・ｹ邵ｺ・ｧ郢ｧ繧牙験騾墓ｺ倪・郢ｧ荵昶螺郢ｧ繝ｻ・ｻ蜻趣ｽｮ・ｺ
                    break;
                }
            }
        });
        println!("[3/3] ページを準備中...");
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
                format!("({part}/{total}) More chunks follow. Do not answer yet.`n{chunk}")
            } else {
                format!("({part}/{total}) All chunks sent. Continue processing.`n{chunk}")
            };
            if total > 1 {
                eprintln!("send {part}/{total} ({} chars)", msg.len());
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

        // Slightly scroll before input so the page is in a natural state.
        {
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            if seed % 2 == 0 {
                let dy = 40 + (seed % 80); // 40邵ｲ繝ｻ19px 闕ｳ荵昶・郢ｧ・ｹ郢ｧ・ｯ郢晢ｽｭ郢晢ｽｼ郢晢ｽｫ
                let js = format!("window.scrollBy({{top: {dy}, behavior: 'smooth'}});");
                tokio::time::timeout(Duration::from_secs(3), page.evaluate_expression(&js))
                    .await
                    .ok();
                tokio::time::sleep(jitter(400, 300)).await;
            }
        }

        scroll_to_selector(page, INPUT_SELECTOR).await;
        tokio::time::sleep(jitter(700, 500)).await;

        tokio::time::timeout(
            Duration::from_secs(10),
            page.evaluate_expression(include_str!("js/move_and_click.js")),
        )
        .await
        .map_err(|_| anyhow::anyhow!("move_and_click timeout"))??;
        tokio::time::sleep(jitter(600, 400)).await;

        // Show progress while inserting text.
        {
            use std::io::Write as _;
            eprint!("\r  Copilot へ送信中...                        ");
            std::io::stderr().flush().ok();
        }
        // Focus the input before CDP InsertText.
        tokio::time::timeout(
            Duration::from_secs(5),
            page.evaluate_expression(
                r#"(function() {
                    const el = window.__copipeFindInput
                        ? window.__copipeFindInput()
                        : document.querySelector('#userInput, textarea, [contenteditable="true"][role="textbox"], [role="textbox"][contenteditable="true"]');
                    if (el) { el.focus(); return true; }
                    return false;
                })()"#,
            ),
        )
        .await
        .ok();
        tokio::time::sleep(jitter(150, 100)).await;

        // 遶ｭ・ｰ CDP Input.insertText 遯ｶ繝ｻOS 郢晢ｽｬ郢晏生ﾎ晉ｸｺ・ｮ郢ｧ・ｭ郢晢ｽｼ陷茨ｽ･陷牙ｸ吮・陷ｷ蠕個ｧ郢昜ｻ｣縺帷ｹｧ蟶敖螢ｹ・狗ｸｺ貅假ｽ∬ｭ崢郢ｧ繧翫・霎滂ｽｶ
        {
            use chromiumoxide::cdp::browser_protocol::input::InsertTextParams;
            let cdp_ok = tokio::time::timeout(
                Duration::from_secs(30),
                page.execute(InsertTextParams::new(prompt)),
            )
            .await
            .ok()
            .and_then(|r| r.ok())
            .is_some();

            if cdp_ok {
                // Notify React-style listeners after CDP text insertion.
                tokio::time::timeout(
                    Duration::from_secs(5),
                    page.evaluate_expression(
                        r#"(function() {
                            const el = window.__copipeFindInput
                                ? window.__copipeFindInput()
                                : document.querySelector('#userInput, textarea, [contenteditable="true"][role="textbox"], [role="textbox"][contenteditable="true"]');
                            if (!el) return;
                            el.dispatchEvent(new Event('input',  { bubbles: true, composed: true }));
                            el.dispatchEvent(new Event('change', { bubbles: true, composed: true }));
                        })()"#,
                    ),
                )
                .await
                .ok();
            } else {
                // Fallback through React's native value setter.
                let js_str = serde_json::to_string(prompt)?;
                tokio::time::timeout(
                    Duration::from_secs(15),
                    page.evaluate_expression(&format!(
                        "({})({})",
                        include_str!("js/react_set_value.js"),
                        js_str
                    )),
                )
                .await
                .map_err(|_| anyhow::anyhow!("react_set_value timeout"))??;
            }
        }
        tokio::time::sleep(jitter(900, 700)).await;

        // 鬨ｾ竏ｽ・ｿ・｡: 鬨ｾ竏ｽ・ｿ・｡郢晄㈱縺｡郢晢ｽｳ郢ｧ・ｯ郢晢ｽｪ郢昴・縺醍ｹｧ雋樞煤陷亥現・邵ｲ竏ｬ・ｦ荵昶命邵ｺ荵晢ｽ臥ｸｺ・ｪ邵ｺ繝ｻ・ｰ・ｴ陷ｷ蛹ｻ繝ｻ Enter 郢ｧ・ｭ郢晢ｽｼ邵ｺ・ｫ郢晁ｼ斐°郢晢ｽｼ郢晢ｽｫ郢晁・繝｣郢ｧ・ｯ
        let target = baseline + 1;
        let mut send_attempts = Vec::new();
        let mut accepted = false;

        for attempt in 1..=4 {
            let detail = if attempt == 2 {
                format!("enter:{}", send_enter(page).await?)
            } else if let Some(detail) = send_button(page).await {
                format!("button:{detail}")
            } else {
                format!("enter:{}", send_enter(page).await?)
            };
            send_attempts.push(format!("#{attempt} {detail}"));

            tokio::time::sleep(jitter(650, 350)).await;
            if wait_for_send_acceptance(page, target, Duration::from_secs(4)).await {
                accepted = true;
                break;
            }

            dismiss_signin_later_safe(page).await;
            scroll_to_selector(page, INPUT_SELECTOR).await;
            tokio::time::sleep(jitter(350, 250)).await;
        }

        let send_log_msg = if accepted {
            format!("[send:accepted] {}\n---", send_attempts.join(" -> "))
        } else {
            format!("[send:unconfirmed] {}\n---", send_attempts.join(" -> "))
        };

        // Keep the send method in the browser log.
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
        dismiss_signin_later_safe(page).await;
        wait_for_ai_message_count(page, target, 120, log_dir_ref).await?;
        dismiss_signin_later_safe(page).await;
        wait_for_stable_text(page, target, 180, log_dir_ref).await?;
        dismiss_signin_later_safe(page).await;
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            scroll_to_nth_ai_message(page, target),
        )
        .await;
        tokio::time::sleep(jitter(1_500, 2_000)).await;
        Ok(())
    }

    /// Stop Copilot generation when Ctrl+C is pressed.
    pub async fn stop_generation(&self) {
        let result = self
            .page
            .evaluate_expression(
                r#"
            (function() {
                // 騾墓ｻ薙・陋帶㊧・ｭ・｢郢晄㈱縺｡郢晢ｽｳ郢ｧ蜻育粟邵ｺ蜉ｱ窶ｻ郢ｧ・ｯ郢晢ｽｪ郢昴・縺・                const selectors = [
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
                // 郢晁ｼ斐°郢晢ｽｼ郢晢ｽｫ郢晁・繝｣郢ｧ・ｯ: Escape 郢ｧ・ｭ郢晢ｽｼ
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

#[cfg(test)]
mod tests {
    #[test]
    fn react_set_value_script_can_be_wrapped_as_callable_expression() {
        let script = include_str!("js/react_set_value.js");
        let arg = serde_json::to_string("hello; world").unwrap();
        let wrapped = format!("({script})({arg})");

        assert!(!wrapped.contains("});)("));
        assert!(wrapped.starts_with("((function"));
        assert!(wrapped.contains(")(\"hello; world\")"));
    }
}
