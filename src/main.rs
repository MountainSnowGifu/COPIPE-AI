mod agent;
mod color;
mod command;
mod executor;
mod session;

use agent::{build_system_prompt, run_agent};
use color::{use_unicode, BOLD, CYAN_BOLD, DIM, GREEN_BOLD, RED_BOLD, RESET, YELLOW};
use executor::LOG_DIR;
use session::CopilotSession;

fn print_help() {
    println!("{BOLD}操作方法{RESET}");
    println!("  タスクを日本語で入力して Enter");
    println!("  行末に \\ を付けると次の行に続けられます");
    println!("  タスク実行中は {BOLD}Ctrl+C{RESET} でキャンセル");
    println!("  終了: {BOLD}exit{RESET} / {BOLD}quit{RESET} / {BOLD}Ctrl+D{RESET}");
    println!();
    println!("{BOLD}コマンド{RESET}");
    println!("  {BOLD}:h{RESET}          このヘルプを表示");
    println!("  {BOLD}:v{RESET}          verbose モードを切替（ツール出力を詳しく表示）");
    println!("  {BOLD}:y{RESET}          確認スキップモードをトグル（常時 on/off）");
    println!("  タスク末尾 {BOLD}:y{RESET}  そのタスクだけ確認スキップ  例: レビューして:y");
    println!("  {DIM}※ :y トグルとタスク末尾 :y は独立した機能です{RESET}");
    println!();
    println!("{BOLD}動作要件{RESET}");
    println!("  ブラウザ: Microsoft Edge または Google Chrome が必要");
    println!("  {DIM}COPIPE_BROWSER_PATH 環境変数でブラウザパスを上書き可能{RESET}");
    println!("  Copilot へのログイン済みセッションが必要（初回は手動ログイン）");
    println!();
    println!("{DIM}タスク例:{RESET}");
    println!("{DIM}  src ディレクトリの構成を調べてください{RESET}");
    println!("{DIM}  main.rs を読んで TODO を一覧にしてください{RESET}");
    println!("{DIM}  cargo build して失敗したら修正してください{RESET}");
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
        print_help();
        return Ok(());
    }

    let root = match root_dir {
        Some(ref p) => std::path::PathBuf::from(p),
        None => std::env::current_dir()?,
    }
    .canonicalize()?;

    // 起動時にログを初期化（シンボリックリンク経由のルート外書き込みを防ぐ）
    let log_dir = root.join(LOG_DIR);
    // .copipe_logs ディレクトリ自体が symlink の場合は起動を拒否
    if log_dir.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        anyhow::bail!(
            "{} はシンボリックリンクです。untrusted リポジトリによるログ外部書き込みを防ぐため起動を中止します。",
            log_dir.display()
        );
    }
    std::fs::create_dir_all(&log_dir).ok();
    for name in &["ai_log", "cmd_log", "browser_log"] {
        let log_path = log_dir.join(name);
        if log_path.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
            eprintln!("警告: {} はシンボリックリンクのため初期化をスキップしました", log_path.display());
            continue;
        }
        std::fs::write(&log_path, "").ok();
    }

    println!("{DIM}ブラウザを起動中...{RESET}");
    let mut session = CopilotSession::start().await?;
    session.log_dir = Some(log_dir.clone());
    println!("{GREEN_BOLD}✓{RESET} Copilot に接続しました");

    println!("{DIM}初期化中...{RESET}");
    session.send_raw(&build_system_prompt(&root)).await?;
    println!("{GREEN_BOLD}✓{RESET} 準備完了\n");

    // バナー（Unicode 利用可能ならボックス描画文字、そうでなければ ASCII）
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
    println!("{DIM}ヘルプは :h  終了は exit または Ctrl+D{RESET}");

    let mut rl = rustyline::DefaultEditor::new()?;
    let history_path = std::env::var("HOME")
        .ok()
        .map(|h| std::path::PathBuf::from(h).join(".copipe_ai_history"));
    if let Some(ref p) = history_path {
        rl.load_history(p).ok();
    }

    'repl: loop {
        // ── 入力フェーズ（行末 \ でマルチライン継続） ──────────────────
        let base_prompt = match (auto_confirm, verbose) {
            (true,  true)  => "\n[y,v]> ",
            (true,  false) => "\n[y]> ",
            (false, true)  => "\n[v]> ",
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
                    // Ctrl+C: 入力中なら内容クリア、空ならヒント
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
            ":h" => { print_help(); continue 'repl; }
            ":v" => {
                verbose = !verbose;
                println!("verbose: {BOLD}{}{RESET}", if verbose { "on" } else { "off" });
                continue 'repl;
            }
            ":y" => {
                auto_confirm = !auto_confirm;
                println!("確認スキップ: {BOLD}{}{RESET}", if auto_confirm { "on" } else { "off" });
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
            let confirmed = 'confirm: loop {
                // Enter（空入力）= Yes、n/N = No
                match rl.readline("実行しますか? [Y/n] ") {
                    Ok(ans) => match ans.trim() {
                        "" | "y" | "Y" => break 'confirm Some(true),
                        "n" | "N"      => break 'confirm Some(false),
                        other => println!("{DIM}「{other}」は無効です。Enter で Yes、n で No{RESET}"),
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
                    Ok(false) => {} // agent/mod.rs 内で詳細メッセージ出力済み
                    Err(e)    => println!("\n{RED_BOLD}エラー: {e}{RESET}"),
                }
            }
            _ = tokio::signal::ctrl_c() => {
                // Copilot ブラウザ側の生成も停止してから戻る
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
