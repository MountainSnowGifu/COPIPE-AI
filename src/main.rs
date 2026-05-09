mod agent;
mod color;
mod command;
mod executor;
mod session;

use agent::{build_system_prompt, run_agent};
use color::{use_unicode, BOLD, CYAN_BOLD, DIM, GREEN_BOLD, RED_BOLD, RESET, YELLOW};
use executor::LOG_DIR;
use session::CopilotSession;

// #6: セクション分けされたヘルプ + #9: Ctrl+C 明記
fn print_help(verbose: bool, auto_confirm: bool) {
    let v_state = if verbose      { "ON " } else { "OFF" };
    let y_state = if auto_confirm { "ON " } else { "OFF" };

    println!("{BOLD}── コマンド ─────────────────────────────────{RESET}");
    println!("  {BOLD}:h{RESET}       このヘルプを表示");
    println!("  {BOLD}:v{RESET}       verboseモード切替      (現在: {BOLD}{v_state}{RESET})");
    println!("  {BOLD}:y{RESET}       自動確認モード切替     (現在: {BOLD}{y_state}{RESET})");
    println!("  {BOLD}exit{RESET}     終了  (Ctrl+D でも可)");
    println!();
    println!("{BOLD}── タスクの書き方 ───────────────────────────{RESET}");
    println!("  日本語でタスクを入力して Enter");
    println!("  行末に {BOLD}\\{RESET} で複数行入力");
    println!("  タスク末尾に {BOLD}:y{RESET} で確認を1回スキップ  例: レビューして:y");
    println!();
    println!("{BOLD}── キー操作 ─────────────────────────────────{RESET}");
    println!("  {BOLD}Enter{RESET}    タスク確認プロンプトで実行");
    println!("  {BOLD}Ctrl+C{RESET}   実行中のタスクをキャンセル（生成も停止）");
    println!("  {BOLD}Ctrl+D{RESET}   終了");
    println!();
    println!("{BOLD}── 動作要件 ─────────────────────────────────{RESET}");
    println!("  ブラウザ: Microsoft Edge または Google Chrome が必要");
    println!("  {DIM}COPIPE_BROWSER_PATH 環境変数でパスを上書き可能{RESET}");
    println!("  Copilot へのログイン済みセッションが必要（初回は手動ログイン）");
    println!();
    println!("{DIM}タスク例:{RESET}");
    println!("{DIM}  src の構成を調べてください{RESET}");
    println!("{DIM}  main.rs を読んで TODO を一覧にしてください{RESET}");
    println!("{DIM}  cargo check して失敗したら修正してください{RESET}");
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use rustyline::error::ReadlineError;

    // CLI 引数パース: copipe-ai [--verbose|-v] [-y] [プロジェクトディレクトリ]
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut verbose = args.iter().any(|a| a == "--verbose" || a == "-v");
    let mut auto_confirm = args.iter().any(|a| a == "-y" || a == "--yes");
    let root_dir = args.iter().find(|a| !a.starts_with('-')).cloned();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help(verbose, auto_confirm);
        return Ok(());
    }

    let root = match root_dir {
        Some(ref p) => std::path::PathBuf::from(p),
        None => std::env::current_dir()?,
    }
    .canonicalize()?;

    // 起動時にログを初期化（シンボリックリンク経由のルート外書き込みを防ぐ）
    let log_dir = root.join(LOG_DIR);
    if log_dir.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        anyhow::bail!(
            "{} はシンボリックリンクです。untrusted リポジトリによるログ外部書き込みを防ぐため起動を中止します。",
            log_dir.display()
        );
    }
    std::fs::create_dir_all(&log_dir).ok();
    // #8: ログディレクトリの初回案内（ディレクトリが新規作成された場合）
    let is_first_run = !log_dir.join("ai_log").exists();
    for name in &["ai_log", "cmd_log", "browser_log"] {
        let log_path = log_dir.join(name);
        if log_path.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
            anyhow::bail!(
                "{} はシンボリックリンクです。外部ファイルへのログ書き込みを防ぐため起動を中止します。",
                log_path.display()
            );
        }
        std::fs::write(&log_path, "").ok();
    }

    // #1: 進捗は session/mod.rs の start() 内で出力
    let mut session = CopilotSession::start().await?;
    session.log_dir = Some(log_dir.clone());

    println!("{DIM}システムプロンプトを送信中...{RESET}");
    session.send_raw(&build_system_prompt(&root)).await?;
    println!("{GREEN_BOLD}✓{RESET} 準備完了\n");

    // #2: バナーにモード状態を表示
    let (tl, tr, bl, br, h, v) = if use_unicode() {
        ("╔", "╗", "╚", "╝", "═", "║")
    } else {
        ("+", "+", "+", "+", "-", "|")
    };
    let line = h.repeat(50);
    println!("{CYAN_BOLD}{tl}{line}{tr}{RESET}");
    println!("{CYAN_BOLD}{v}{RESET}         {BOLD}COPIPE-AI  - AI Dev Assistant{RESET}          {CYAN_BOLD}{v}{RESET}");
    println!("{CYAN_BOLD}{bl}{line}{br}{RESET}");
    println!("プロジェクト: {BOLD}{}{RESET}", root.display());
    {
        let v_state = if verbose      { format!("{BOLD}ON{RESET}")  } else { format!("{DIM}OFF{RESET}") };
        let y_state = if auto_confirm { format!("{BOLD}ON{RESET}")  } else { format!("{DIM}OFF{RESET}") };
        println!("モード: 詳細ログ={v_state}  自動確認={y_state}  {DIM}(切替: :v / :y){RESET}");
    }
    println!("{DIM}ヘルプは :h  Ctrl+C でキャンセル  終了は exit または Ctrl+D{RESET}");

    // #8: 初回起動時のみログディレクトリを案内
    if is_first_run {
        println!("{DIM}ログ出力先: {} (ai_log, cmd_log, browser_log){RESET}", log_dir.display());
    }

    let mut rl = rustyline::DefaultEditor::new()?;
    let history_path = std::env::var("HOME")
        .ok()
        .map(|h| std::path::PathBuf::from(h).join(".copipe_ai_history"));
    if let Some(ref p) = history_path {
        rl.load_history(p).ok();
    }

    'repl: loop {
        // ── 入力フェーズ（行末 \ でマルチライン継続） ──────────────────
        // #2: プロンプトにモード状態を表示
        let base_prompt = match (auto_confirm, verbose) {
            (true,  true)  => "\n[自動確認,詳細]> ",
            (true,  false) => "\n[自動確認]> ",
            (false, true)  => "\n[詳細]> ",
            (false, false) => "\n> ",
        };
        let mut task = String::new();
        loop {
            let prompt_str = if task.is_empty() { base_prompt } else { "... " };
            match rl.readline(prompt_str) {
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
                    if !task.is_empty() {
                        task.clear();
                        println!("{DIM}入力をクリアしました{RESET}");
                    } else {
                        println!("{DIM}(Ctrl+D で終了){RESET}");
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
        match task.as_str() {
            "exit" | "quit" => break,
            ":h" => { print_help(verbose, auto_confirm); continue 'repl; }
            ":v" => {
                verbose = !verbose;
                println!("詳細ログ: {BOLD}{}{RESET}", if verbose { "ON" } else { "OFF" });
                continue 'repl;
            }
            ":y" => {
                auto_confirm = !auto_confirm;
                println!("自動確認: {BOLD}{}{RESET}", if auto_confirm { "ON" } else { "OFF" });
                continue 'repl;
            }
            _ => {}
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
            // #3: Enter=実行 / n=中止 を明示
            let confirmed = 'confirm: loop {
                match rl.readline("実行しますか? [Enter=実行 / n=中止] ") {
                    Ok(ans) => match ans.trim() {
                        "" | "y" | "Y" => break 'confirm Some(true),
                        "n" | "N"      => break 'confirm Some(false),
                        other => println!("{DIM}「{other}」は無効です。Enter で実行、n で中止{RESET}"),
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
                    Ok(true)  => println!("\n{GREEN_BOLD}✓ タスク完了{RESET}"),
                    Ok(false) => {}
                    Err(e)    => println!("\n{RED_BOLD}エラー: {e}{RESET}"),
                }
            }
            _ = tokio::signal::ctrl_c() => {
                session.stop_generation().await;
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
