mod agent;
mod color;
mod command;
mod executor;
mod session;

use agent::{build_system_prompt, run_agent};
use color::{BOLD, DIM, RESET};
use executor::LOG_DIR;
use session::CopilotSession;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use rustyline::error::ReadlineError;

    // CLI 引数パース: copipe-ai [--verbose|-v] [-y] [プロジェクトディレクトリ]
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut verbose = args.iter().any(|a| a == "--verbose" || a == "-v");
    let mut auto_confirm = args.iter().any(|a| a == "-y" || a == "--yes");
    let root_dir = args.iter().find(|a| !a.starts_with('-')).cloned();

    let root = match root_dir {
        Some(ref p) => std::path::PathBuf::from(p),
        None => std::env::current_dir()?,
    }
    .canonicalize()?;

    eprintln!("プロジェクトルート: {}", root.display());

    // 起動時にログを初期化
    let log_dir = root.join(LOG_DIR);
    std::fs::create_dir_all(&log_dir).ok();
    for name in &["ai_log", "cmd_log", "browser_log"] {
        std::fs::write(log_dir.join(name), "").ok();
    }

    let mut session = CopilotSession::start().await?;
    eprintln!("Copilot に接続しました。");

    eprintln!("システムプロンプト送信中...");
    session.send_raw(&build_system_prompt(&root)).await?;
    eprintln!("準備完了。");

    println!("{BOLD}╔══════════════════════════════════════════════════╗{RESET}");
    println!("{BOLD}║             COPIPE-AI へようこそ                 ║{RESET}");
    println!("{BOLD}╚══════════════════════════════════════════════════╝{RESET}");
    println!("プロジェクト: {BOLD}{}{RESET}", root.display());
    println!();
    println!("{BOLD}操作方法{RESET}");
    println!("  タスクを日本語で入力して Enter");
    println!("  行末に \\ を付けると次の行に続けられます");
    println!("  タスク実行中は {BOLD}Ctrl+C{RESET} でキャンセル");
    println!("  終了: {BOLD}exit{RESET} / {BOLD}quit{RESET} / {BOLD}Ctrl+D{RESET}");
    println!("  verbose 切替: {BOLD}:v{RESET}  (現在: {})", if verbose { "on" } else { "off" });
    println!("  確認スキップ切替: {BOLD}:y{RESET}  (現在: {})", if auto_confirm { "on" } else { "off" });
    println!("  タスク末尾に {BOLD}:y{RESET} で1回だけ確認スキップ  例: コードレビューして:y");
    println!();
    println!("{DIM}タスク例:{RESET}");
    println!("{DIM}  src ディレクトリの構成を調べてください{RESET}");
    println!("{DIM}  main.rs を読んで TODO を一覧にしてください{RESET}");
    println!("{DIM}  cargo build して失敗したら修正してください{RESET}");

    let mut rl = rustyline::DefaultEditor::new()?;
    let history_path = std::env::var("HOME")
        .ok()
        .map(|h| std::path::PathBuf::from(h).join(".copipe_ai_history"));
    if let Some(ref p) = history_path {
        rl.load_history(p).ok();
    }

    'repl: loop {
        // ── 入力フェーズ（行末 \ でマルチライン継続） ──────────────────
        let mut task = String::new();
        loop {
            let prompt = if task.is_empty() { "\n> " } else { "... " };
            match rl.readline(prompt) {
                Ok(line) => {
                    rl.add_history_entry(line.as_str()).ok();
                    if line.ends_with('\\') {
                        task.push_str(&line[..line.len() - 1]);
                        task.push('\n');
                    } else {
                        task.push_str(&line);
                        break;
                    }
                }
                Err(ReadlineError::Interrupted) => {
                    if task.is_empty() {
                        println!("(Ctrl+D で終了)");
                    } else {
                        task.clear();
                        println!("入力をクリアしました");
                    }
                    continue 'repl;
                }
                Err(ReadlineError::Eof) => break 'repl,
                Err(e) => {
                    eprintln!("入力エラー: {e}");
                    break 'repl;
                }
            }
        }

        let task = task.trim().to_string();
        if task.is_empty() {
            continue;
        }
        if task == "exit" || task == "quit" {
            break;
        }
        if task == ":v" || task == "verbose" {
            verbose = !verbose;
            println!("verbose: {}", if verbose { "on" } else { "off" });
            continue 'repl;
        }
        if task == ":y" {
            auto_confirm = !auto_confirm;
            println!("確認スキップ: {}", if auto_confirm { "on" } else { "off" });
            continue 'repl;
        }

        // タスク末尾の :y で1回だけ確認スキップ
        let (task, skip_confirm) = if task.ends_with(":y") {
            (task[..task.len() - 2].trim().to_string(), true)
        } else {
            (task, false)
        };

        // ── 確認ステップ ──────────────────────────────────────────────
        println!("┌─ タスク ─────────────────────────────────────────");
        for line in task.lines() {
            println!("│ {line}");
        }
        println!("└──────────────────────────────────────────────────");

        if !auto_confirm && !skip_confirm {
            // y / Y / Enter のみ実行。それ以外は再入力を促す。
            let confirmed = 'confirm: loop {
                match rl.readline("実行しますか? [Y/n] ") {
                    Ok(ans) => match ans.trim() {
                        "" | "y" | "Y" => break 'confirm Some(true),
                        "n" | "N"      => break 'confirm Some(false),
                        other => println!("「{other}」は無効です。y か n を半角英字で入力してください"),
                    },
                    Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => {
                        break 'confirm Some(false);
                    }
                    Err(e) => {
                        eprintln!("入力エラー: {e}");
                        break 'confirm None;
                    }
                }
            };
            match confirmed {
                Some(true) => {}
                Some(false) => {
                    println!("キャンセルしました");
                    continue 'repl;
                }
                None => break 'repl,
            }
        }

        // ── 実行フェーズ（Ctrl+C でキャンセル） ───────────────────────
        tokio::select! {
            result = run_agent(&mut session, &root, &task, verbose) => {
                match result {
                    Ok(()) => println!("\n── タスク完了 ─────────────────────────────────────────"),
                    Err(e) => println!("エラー: {e}"),
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!("\nCtrl+C: タスクをキャンセルしました");
            }
        }
    }

    if let Some(ref p) = history_path {
        rl.save_history(p).ok();
    }
    println!("終了します");
    drop(session);
    Ok(())
}
