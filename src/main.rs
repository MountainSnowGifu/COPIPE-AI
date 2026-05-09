mod agent;
mod color;
mod command;
mod executor;
mod session;

use agent::{build_system_prompt, run_agent};
use color::{BOLD, CYAN_BOLD, DIM, GREEN_BOLD, RED_BOLD, RESET, YELLOW};
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

    // 起動時にログを初期化
    let log_dir = root.join(LOG_DIR);
    std::fs::create_dir_all(&log_dir).ok();
    for name in &["ai_log", "cmd_log", "browser_log"] {
        std::fs::write(log_dir.join(name), "").ok();
    }

    println!("{DIM}ブラウザを起動中...{RESET}");
    let mut session = CopilotSession::start().await?;
    println!("{GREEN_BOLD}✓{RESET} Copilot に接続しました");

    println!("{DIM}初期化中...{RESET}");
    session.send_raw(&build_system_prompt(&root)).await?;
    println!("{GREEN_BOLD}✓{RESET} 準備完了\n");

    println!("{CYAN_BOLD}╔══════════════════════════════════════════════════╗{RESET}");
    println!("{CYAN_BOLD}║{RESET}          {BOLD}COPIPE-AI へようこそ{RESET}                   {CYAN_BOLD}║{RESET}");
    println!("{CYAN_BOLD}╚══════════════════════════════════════════════════╝{RESET}");
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
                        println!("{DIM}(Ctrl+D で終了){RESET}");
                    } else {
                        task.clear();
                        println!("{DIM}入力をクリアしました{RESET}");
                    }
                    continue 'repl;
                }
                Err(ReadlineError::Eof) => break 'repl,
                Err(e) => {
                    println!("{RED_BOLD}入力エラー: {e}{RESET}");
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
            println!("verbose: {BOLD}{}{RESET}", if verbose { "on" } else { "off" });
            continue 'repl;
        }
        if task == ":y" {
            auto_confirm = !auto_confirm;
            println!("確認スキップ: {BOLD}{}{RESET}", if auto_confirm { "on" } else { "off" });
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
            let confirmed = 'confirm: loop {
                match rl.readline("実行しますか? [Y/n] ") {
                    Ok(ans) => match ans.trim() {
                        "" | "y" | "Y" => break 'confirm Some(true),
                        "n" | "N"      => break 'confirm Some(false),
                        other => println!("「{other}」は無効です。y か n を入力してください"),
                    },
                    Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => {
                        break 'confirm Some(false);
                    }
                    Err(e) => {
                        println!("{RED_BOLD}入力エラー: {e}{RESET}");
                        break 'confirm None;
                    }
                }
            };
            match confirmed {
                Some(true) => {}
                Some(false) => {
                    println!("{DIM}キャンセルしました{RESET}");
                    continue 'repl;
                }
                None => break 'repl,
            }
        }

        // ── 実行フェーズ（Ctrl+C でキャンセル） ───────────────────────
        tokio::select! {
            result = run_agent(&mut session, &root, &task, verbose) => {
                match result {
                    Ok(()) => println!("\n{GREEN_BOLD}✓ タスク完了{RESET}"),
                    Err(e) => println!("\n{RED_BOLD}エラー: {e}{RESET}"),
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!("\n{YELLOW}キャンセルしました{RESET}");
            }
        }
    }

    if let Some(ref p) = history_path {
        rl.save_history(p).ok();
    }
    println!("{DIM}終了します{RESET}");
    drop(session);
    Ok(())
}
