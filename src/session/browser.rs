use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

pub fn free_port() -> anyhow::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| anyhow::anyhow!("空きポートが見つかりません: {}", e))?;
    let port = listener
        .local_addr()
        .map_err(|e| anyhow::anyhow!("local_addr の取得に失敗しました: {}", e))?
        .port();
    Ok(port)
}

fn browser_candidates() -> Vec<PathBuf> {
    if let Ok(path) = std::env::var("COPIPE_BROWSER_PATH") {
        let path = path.trim().trim_matches('"');
        if !path.is_empty() {
            return vec![PathBuf::from(path)];
        }
    }

    if cfg!(target_os = "windows") {
        let mut candidates = vec![PathBuf::from(
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        )];
        for var in ["ProgramFiles", "ProgramFiles(x86)", "LocalAppData"] {
            if let Ok(base) = std::env::var(var) {
                candidates.push(PathBuf::from(&base).join("Microsoft/Edge/Application/msedge.exe"));
                candidates.push(PathBuf::from(&base).join("Google/Chrome/Application/chrome.exe"));
            }
        }
        candidates
    } else {
        vec![
            PathBuf::from("/usr/bin/microsoft-edge"),
            PathBuf::from("/usr/bin/microsoft-edge-stable"),
            PathBuf::from("/usr/bin/google-chrome"),
            PathBuf::from("/usr/bin/chromium"),
        ]
    }
}

fn browser_profile_dir() -> PathBuf {
    let base = if cfg!(target_os = "windows") {
        crate::paths::local_data_dir()
    } else {
        crate::paths::home_dir()
            .map(|home| home.join(".config"))
            .unwrap_or_else(|| std::env::temp_dir().join("copipe-ai"))
    };

    base.join("copipe-ai-browser-profile")
}

fn cleanup_singleton_files(profile_dir: &Path) {
    for file in ["SingletonLock", "SingletonSocket", "SingletonCookie"] {
        let _ = std::fs::remove_file(profile_dir.join(file));
    }
}

fn build_browser_command(candidate: &Path, port: u16, profile_dir: &Path) -> Command {
    let mut command = Command::new(candidate);
    command
        .arg(format!("--remote-debugging-port={port}"))
        .arg("--disable-blink-features=AutomationControlled")
        .arg(format!("--user-data-dir={}", profile_dir.display()))
        .arg("--lang=ja-JP")
        // 通常ブラウザに見せるための追加フラグ
        .arg("--disable-infobars")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-notifications")
        .arg("--disable-popup-blocking")
        .arg("--password-store=basic")
        .arg("--use-mock-keychain")
        .arg("--window-size=1280,800")
        .arg("--window-position=100,50")
        .arg("--force-device-scale-factor=1")
        .arg("--disable-features=IsolateOrigins,site-per-process")
        // マウス/ポインターが存在する通常デスクトップ環境として通知
        .arg("--blink-settings=primaryHoverType=2,availableHoverTypes=2,primaryPointerType=4,availablePointerTypes=4")
        // タブがバックグラウンドでもスロットリングされないようにする
        .arg("--disable-backgrounding-occluded-windows")
        .arg("--disable-renderer-backgrounding")
        .arg("--disable-background-timer-throttling")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    if !cfg!(target_os = "windows") {
        command
            .arg("--no-sandbox")
            .arg("--disable-dev-shm-usage")
            .env("DISPLAY", ":0")
            .env("WAYLAND_DISPLAY", "wayland-0")
            .env("LANG", "ja_JP.UTF-8")
            .env("LC_ALL", "ja_JP.UTF-8");
    }

    command
}

pub fn launch_edge(port: u16) -> anyhow::Result<Child> {
    let profile_dir = browser_profile_dir();
    std::fs::create_dir_all(&profile_dir)?;
    cleanup_singleton_files(&profile_dir);

    let mut tried = Vec::new();
    for candidate in browser_candidates() {
        let candidate_exists = candidate
            .is_absolute()
            .then(|| candidate.exists())
            .unwrap_or(true);
        if !candidate_exists {
            tried.push(format!("{} (missing)", candidate.display()));
            continue;
        }

        match build_browser_command(&candidate, port, &profile_dir).spawn() {
            Ok(child) => return Ok(child),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                tried.push(format!("{} (not found)", candidate.display()));
            }
            Err(err) => {
                return Err(anyhow::anyhow!(
                    "ブラウザの起動に失敗しました: {} ({err})",
                    candidate.display()
                ));
            }
        }
    }

    Err(anyhow::anyhow!(
        "起動可能な Chromium 系ブラウザが見つかりません。COPIPE_BROWSER_PATH を設定するか、Microsoft Edge / Google Chrome をインストールしてください。候補: {}",
        tried.join(", ")
    ))
}
