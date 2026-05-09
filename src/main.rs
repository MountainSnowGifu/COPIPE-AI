mod agent;
mod color;
mod command;
mod executor;
mod session;

use agent::{build_system_prompt, run_agent};
use color::{BOLD, CYAN_BOLD, DIM, GREEN_BOLD, RED_BOLD, RESET, YELLOW};
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
    println!("  {BOLD}:h{RESET}   このヘルプを表示");
    println!("  {BOLD}:v{RESET}   verbose モードを切替（ツール出力を詳しく表示）");
    println!("  {BOLD}:y{RESET}   確認スキップモードを切替（毎回の Y/n を省略）");
    println!("  タスク末尾に {BOLD}:y{RESET} で1回だけ確認スキップ  例: コードレビューして:y");
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

    // バナー（全角混在を避けるため ASCII ボックスで固定幅）
    println!("{CYAN_BOLD}+--------------------------------------------------+{RESET}");
    println!("{CYAN_BOLD}|{RESET}         {BOLD}COPIPE-AI  - AI Dev Assistant{RESET}          {CYAN_BOLD}|{RESET}");
    println!("{CYAN_BOLD}+--------------------------------------------------+{RESET}");
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
                    Ok(true)  => println!("\n{GREEN_BOLD}✓ タスク完了{RESET}"),
                    Ok(false) => println!("\n{YELLOW}最大ターン数に達しました。タスクを再入力すると続きから作業できます。{RESET}"),
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
