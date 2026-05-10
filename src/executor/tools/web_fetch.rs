use crate::executor::ToolResult;

const MAX_CHARS: usize = 20_000;
const TIMEOUT_SECS: u64 = 15;

/// URL からコンテンツを取得してテキストとして返す
///
/// HTML は簡易パースでテキスト抽出（タグ除去）。
/// JSON / プレーンテキストはそのまま返す。
/// 最大 20,000 文字に切り詰める。
pub async fn handle(url: &str, _selector: &Option<String>) -> ToolResult {
    let label = format!("WebFetch({url})");

    // URL の基本検証
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return ToolResult::new(label, "ERROR: URL は http:// または https:// で始まる必要があります。");
    }

    let client = match reqwest::ClientBuilder::new()
        .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
        .user_agent("Mozilla/5.0 (compatible; COPIPE-AI/1.0)")
        .build()
    {
        Ok(c) => c,
        Err(e) => return ToolResult::new(label, format!("ERROR: HTTPクライアント作成失敗: {e}")),
    };

    let resp = match client.get(url).send().await {
        Err(e) => return ToolResult::new(label, format!("ERROR: リクエスト失敗: {e}")),
        Ok(r) => r,
    };

    let status = resp.status();
    if !status.is_success() {
        return ToolResult::new(label, format!("ERROR: HTTP {status}"));
    }

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();

    let body = match resp.text().await {
        Err(e) => return ToolResult::new(label, format!("ERROR: 本文取得失敗: {e}")),
        Ok(s) => s,
    };

    // HTML はタグを除去してテキスト抽出
    let text = if content_type.contains("html") {
        strip_html(&body)
    } else {
        body
    };

    // サイズ制限
    let (result, truncated) = if text.chars().count() > MAX_CHARS {
        let t: String = text.chars().take(MAX_CHARS).collect();
        (t, true)
    } else {
        (text, false)
    };

    let mut output = format!("URL: {url}\n\n{result}");
    if truncated {
        output.push_str(&format!("\n\n[{MAX_CHARS} 文字以降を省略]"));
    }

    ToolResult::new(label, output)
}

/// HTML からテキストを抽出（簡易実装）
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut prev_ws = false;

    let lower = html.to_lowercase();

    let mut chars = html.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        // <script> / <style> ブロックをスキップ
        if !in_tag {
            if lower[i..].starts_with("<script") { in_script = true; }
            if lower[i..].starts_with("</script>") { in_script = false; }
            if lower[i..].starts_with("<style") { in_style = true; }
            if lower[i..].starts_with("</style>") { in_style = false; }
        }

        if in_script || in_style {
            if ch == '>' { in_script = in_script && !lower[i..].starts_with("</script>"); }
            continue;
        }

        match ch {
            '<' => { in_tag = true; }
            '>' => {
                in_tag = false;
                // ブロック要素の後には改行を挿入
                out.push('\n');
                prev_ws = true;
            }
            _ if in_tag => {}
            '\n' | '\r' | '\t' => {
                if !prev_ws { out.push(' '); prev_ws = true; }
            }
            ' ' => {
                if !prev_ws { out.push(' '); prev_ws = true; }
            }
            _ => {
                // HTML エンティティの簡易デコード
                out.push(ch);
                prev_ws = false;
            }
        }
    }

    // HTML エンティティの後処理
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        // 連続する改行を2行までに抑制
        .split('\n')
        .fold(String::new(), |mut acc, line| {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                acc.push_str(trimmed);
                acc.push('\n');
            }
            acc
        })
}

// ─── テスト ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_html_basic() {
        let html = "<h1>Hello</h1><p>World</p>";
        let text = strip_html(html);
        assert!(text.contains("Hello"));
        assert!(text.contains("World"));
        assert!(!text.contains('<'));
    }

    #[test]
    fn test_strip_html_entities() {
        let html = "<p>A &amp; B &lt;C&gt;</p>";
        let text = strip_html(html);
        assert!(text.contains("A & B <C>"));
    }

    #[test]
    fn test_strip_html_script_removed() {
        let html = "<p>visible</p><script>alert('xss')</script>";
        let text = strip_html(html);
        assert!(text.contains("visible"));
        assert!(!text.contains("alert"));
    }
}
