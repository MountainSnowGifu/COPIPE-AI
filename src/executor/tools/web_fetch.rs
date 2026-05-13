use crate::executor::ToolResult;
use std::net::IpAddr;

const MAX_CHARS: usize = 20_000;
const TIMEOUT_SECS: u64 = 15;

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()        // 127.0.0.0/8
                || v4.is_private()  // 10/8, 172.16/12, 192.168/16
                || v4.is_link_local() // 169.254/16 (クラウドメタデータを含む)
                || v4.is_unspecified()
                || (o[0] == 100 && o[1] >= 64 && o[1] <= 127) // 100.64/10 CG-NAT
        }
        IpAddr::V6(v6) => {
            let seg = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || (seg[0] & 0xfe00) == 0xfc00 // fc00::/7 unique-local
                || (seg[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
        }
    }
}

/// ホストをDNS解決してプライベートIPへのアクセスを拒否する（SSRF防止）
async fn check_ssrf(url_str: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url_str).map_err(|e| format!("URL解析失敗: {e}"))?;
    let host = parsed.host_str().unwrap_or("");
    if host.is_empty() {
        return Err("URLにホストがありません".to_string());
    }
    // IPv6 ブラケットを除去して IP リテラルかどうか確認
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        if is_private_ip(ip) {
            return Err(format!(
                "Permission denied: プライベートIP '{ip}' へのアクセスは禁止です（SSRF防止）"
            ));
        }
        return Ok(());
    }
    // ホスト名 → DNS 解決してすべての IP を確認
    let port = parsed.port_or_known_default().unwrap_or(80);
    let addrs = tokio::net::lookup_host(format!("{host}:{port}"))
        .await
        .map_err(|e| format!("DNS解決失敗 ('{host}'): {e}"))?;
    for addr in addrs {
        let ip = addr.ip();
        if is_private_ip(ip) {
            return Err(format!(
                "Permission denied: '{host}' がプライベートIP ({ip}) に解決されます（SSRF防止）"
            ));
        }
    }
    Ok(())
}

/// URL からコンテンツを取得してテキストとして返す
///
/// HTML は簡易パースでテキスト抽出（タグ除去）。
/// JSON / プレーンテキストはそのまま返す。
/// 最大 20,000 文字に切り詰める。
pub async fn handle(url: &str, _selector: &Option<String>) -> ToolResult {
    let label = format!("WebFetch({url})");

    // URL の基本検証
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return ToolResult::new(
            label,
            "ERROR: URL は http:// または https:// で始まる必要があります。",
        );
    }

    // SSRF 防止: プライベートIP・ループバック・リンクローカルへのアクセスを拒否
    if let Err(e) = check_ssrf(url).await {
        return ToolResult::new(label, format!("ERROR: {e}"));
    }

    // リダイレクトを無効化し、手動で追跡することで各リダイレクト先に SSRF チェックを適用する。
    // reqwest の自動 follow はリダイレクト先のホスト名を DNS 解決しないため不十分。
    let client = match reqwest::ClientBuilder::new()
        .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
        .user_agent("Mozilla/5.0 (compatible; COPIPE-AI/1.0)")
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(c) => c,
        Err(e) => return ToolResult::new(label, format!("ERROR: HTTPクライアント作成失敗: {e}")),
    };

    // 手動リダイレクトループ: 各 Location に check_ssrf を適用
    const MAX_REDIRECTS: usize = 5;
    let mut current_url = url.to_string();
    let mut redirect_count = 0usize;
    let resp = loop {
        let r = match client.get(&current_url).send().await {
            Err(e) => return ToolResult::new(label, format!("ERROR: リクエスト失敗: {e}")),
            Ok(r) => r,
        };
        if r.status().is_redirection() {
            if redirect_count >= MAX_REDIRECTS {
                return ToolResult::new(
                    label,
                    format!("ERROR: リダイレクト回数が上限 ({MAX_REDIRECTS}) を超えました"),
                );
            }
            let location = r
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let location = match location {
                Some(l) => l,
                None => {
                    return ToolResult::new(
                        label,
                        format!("ERROR: リダイレクト先 (Location ヘッダ) が不明です"),
                    );
                }
            };
            // 相対 URL を絶対 URL に解決
            let next_url = match reqwest::Url::parse(&location) {
                Ok(u) => u.to_string(),
                Err(_) => {
                    match reqwest::Url::parse(&current_url).and_then(|base| base.join(&location)) {
                        Ok(u) => u.to_string(),
                        Err(e) => {
                            return ToolResult::new(
                                label,
                                format!("ERROR: リダイレクト URL の解決に失敗: {e}"),
                            );
                        }
                    }
                }
            };
            // リダイレクト先にも SSRF チェック（ホスト名の DNS 解決を含む）
            if let Err(e) = check_ssrf(&next_url).await {
                return ToolResult::new(
                    label,
                    format!("ERROR: リダイレクト先がブロックされました: {e}"),
                );
            }
            current_url = next_url;
            redirect_count += 1;
            continue;
        }
        break r;
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
            if lower[i..].starts_with("<script") {
                in_script = true;
            }
            if lower[i..].starts_with("</script>") {
                in_script = false;
            }
            if lower[i..].starts_with("<style") {
                in_style = true;
            }
            if lower[i..].starts_with("</style>") {
                in_style = false;
            }
        }

        if in_script || in_style {
            if ch == '>' {
                in_script = in_script && !lower[i..].starts_with("</script>");
            }
            continue;
        }

        match ch {
            '<' => {
                in_tag = true;
            }
            '>' => {
                in_tag = false;
                // ブロック要素の後には改行を挿入
                out.push('\n');
                prev_ws = true;
            }
            _ if in_tag => {}
            '\n' | '\r' | '\t' => {
                if !prev_ws {
                    out.push(' ');
                    prev_ws = true;
                }
            }
            ' ' => {
                if !prev_ws {
                    out.push(' ');
                    prev_ws = true;
                }
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
