use std::fmt;
use std::sync::OnceLock;

static ENABLED: OnceLock<bool> = OnceLock::new();

fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        std::env::var("NO_COLOR").is_err() && std::env::var("TERM").as_deref() != Ok("dumb")
    })
}

/// Unicode シンボル・ボックス文字を使用できるかどうか
pub fn use_unicode() -> bool {
    // Windows Terminal (WT_SESSION) は Unicode をフルサポート
    if std::env::var("WT_SESSION").is_ok() {
        return true;
    }
    // ConEmu / Cmder の Unicode サポート
    if std::env::var("ConEmuANSI").as_deref() == Ok("ON") {
        return true;
    }
    if cfg!(target_os = "windows") {
        // 標準 cmd.exe ではボックス描画文字が文字化けする場合があるため無効
        return false;
    }
    // LC_ALL/LANG が UTF-8 系か、TERM が明示的にサポートしている場合のみ true
    let ok = |v: &str| v.contains("UTF") || v.contains("utf");
    std::env::var("LC_ALL").map(|v| ok(&v)).unwrap_or(false)
        || std::env::var("LANG").map(|v| ok(&v)).unwrap_or(false)
        || std::env::var("TERM")
            .map(|v| v == "xterm-256color" || v.contains("xterm"))
            .unwrap_or(false)
        || cfg!(target_os = "macos") // macOS は UTF-8 が標準
}

/// シンボルの ASCII / Unicode 切替ヘルパー
#[allow(dead_code)]
pub fn sym(unicode: &'static str, ascii: &'static str) -> &'static str {
    if use_unicode() { unicode } else { ascii }
}

/// ANSI カラーコードを `{FOO}` 書式で使えるラッパー
pub struct Ansi(&'static str);

impl fmt::Display for Ansi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if enabled() {
            f.write_str(self.0)
        } else {
            Ok(())
        }
    }
}

pub static RESET: Ansi = Ansi("\x1b[0m");
pub static BOLD: Ansi = Ansi("\x1b[1m");
pub static DIM: Ansi = Ansi("\x1b[2m");
pub static GREEN: Ansi = Ansi("\x1b[32m");
pub static GREEN_BOLD: Ansi = Ansi("\x1b[1;32m");
pub static YELLOW: Ansi = Ansi("\x1b[33m");
pub static RED: Ansi = Ansi("\x1b[31m");
pub static CYAN_BOLD: Ansi = Ansi("\x1b[1;36m");
pub static RED_BOLD: Ansi = Ansi("\x1b[1;31m");
