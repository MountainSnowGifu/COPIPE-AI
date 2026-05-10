mod agent;
mod color;
mod command;
mod executor;
mod paths;
mod session;

use agent::{SessionStore, build_system_prompt, run_agent};
use color::{BOLD, CYAN_BOLD, DIM, GREEN_BOLD, RED_BOLD, RESET, YELLOW, use_unicode};
use executor::CheckpointManager;
use executor::LOG_DIR;
use session::CopilotSession;

// #6: セクション分けされたヘルプ + #9: Ctrl+C 明記
fn print_help(verbose: bool, auto_confirm: bool, debug: bool) {
    let v_state = if verbose { "ON " } else { "OFF" };
    let y_state = if auto_confirm { "ON " } else { "OFF" };
    let d_state = if debug { "ON " } else { "OFF" };

    println!("{BOLD}── コマンド ─────────────────────────────────{RESET}");
    println!("  {BOLD}:h{RESET}       このヘルプを表示");
    println!("  {BOLD}:v{RESET}       verboseモード切替      (現在: {BOLD}{v_state}{RESET})");
    println!(
        "  {BOLD}:d{RESET}       デバッグログ切替       (現在: {BOLD}{d_state}{RESET})  → .copipe_logs/debug_log"
    );
    println!("  {BOLD}:y{RESET}       自動確認モード切替     (現在: {BOLD}{y_state}{RESET})");
    println!("  {BOLD}:undo{RESET}    直前のファイル変更を元に戻す");
    println!("  {BOLD}:undo list{RESET} チェックポイント一覧を表示");
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
    // デバッグログはデフォルト ON（--no-debug で無効化）
    let mut debug = !args.iter().any(|a| a == "--no-debug");
    let root_dir = args.iter().find(|a| !a.starts_with('-')).cloned();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help(verbose, auto_confirm, debug);
        return Ok(());
    }

    let root = match root_dir {
        Some(ref p) => std::path::PathBuf::from(p),
        None => std::env::current_dir()?,
    };
    let root = canonicalize_clean(&root)?;

    // 起動時にログを初期化（シンボリックリンク経由のルート外書き込みを防ぐ）
    let log_dir = root.join(LOG_DIR);
    if log_dir
        .symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
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
        if log_path
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            anyhow::bail!(
                "{} はシンボリックリンクです。外部ファイルへのログ書き込みを防ぐため起動を中止します。",
                log_path.display()
            );
        }
        std::fs::write(&log_path, "").ok();
    }

    // チェックポイントマネージャとセッションストアを初期化
    let mut checkpoints = CheckpointManager::new(&root);
    let session_store = SessionStore::new(&root);

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
    println!(
        "{CYAN_BOLD}{v}{RESET}         {BOLD}COPIPE-AI  - AI Dev Assistant{RESET}          {CYAN_BOLD}{v}{RESET}"
    );
    println!("{CYAN_BOLD}{bl}{line}{br}{RESET}");
    println!("プロジェクト: {BOLD}{}{RESET}", root.display());
    {
        let v_state = if verbose {
            format!("{BOLD}ON{RESET}")
        } else {
            format!("{DIM}OFF{RESET}")
        };
        let y_state = if auto_confirm {
            format!("{BOLD}ON{RESET}")
        } else {
            format!("{DIM}OFF{RESET}")
        };
        let d_state = if debug {
            format!("{BOLD}ON{RESET}")
        } else {
            format!("{DIM}OFF{RESET}")
        };
        println!(
            "モード: 詳細ログ={v_state}  自動確認={y_state}  デバッグ={d_state}  {DIM}(切替: :v / :y / :d){RESET}"
        );
        if debug {
            println!(
                "{YELLOW}デバッグモード: .copipe_logs/debug_log にプロンプト・タイミング・状態を記録します{RESET}"
            );
        }
    }
    println!("{DIM}ヘルプは :h  Ctrl+C でキャンセル  終了は exit または Ctrl+D{RESET}");

    // #8: 初回起動時のみログディレクトリを案内
    if is_first_run {
        println!(
            "{DIM}ログ出力先: {} (ai_log, cmd_log, browser_log){RESET}",
            log_dir.display()
        );
    }

    // 前回セッションが残っていれば案内（ただし即座に復元はしない — タスク入力時に判断）
    if session_store.exists() {
        if let Some(ref prev) = session_store.load() {
            println!(
                "{YELLOW}前回の未完了セッションがあります: 「{}」（{}ターン完了済み / {}）{RESET}",
                prev.user_task, prev.turn_count, prev.saved_at
            );
            println!(
                "{DIM}同じタスクを入力すると続きから再開します。別のタスクを入力すると新規開始します。{RESET}"
            );
        }
    }

    let mut rl = rustyline::DefaultEditor::new()?;
    let history_path = paths::home_dir().map(|h| h.join(".copipe_ai_history"));
    if let Some(ref p) = history_path {
        rl.load_history(p).ok();
    }

    'repl: loop {
        // ── 入力フェーズ（行末 \ でマルチライン継続） ──────────────────
        // #7: Worktree 使用中かチェックしてプロンプトに反映
        let in_worktree = executor::tools::worktree::load_state(&root).is_some();
        let base_prompt: &str = &{
            let mut tags: Vec<&str> = Vec::new();
            if in_worktree {
                tags.push("worktree");
            }
            if auto_confirm {
                tags.push("自動確認");
            }
            if verbose {
                tags.push("詳細");
            }
            if tags.is_empty() {
                "\n> ".to_string()
            } else {
                format!("\n[{}]> ", tags.join(","))
            }
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

        let task = clean_task_input(&task);
        if task.is_empty() {
            continue;
        }
        match task.as_str() {
            "exit" | "quit" => break,
            ":h" => {
                print_help(verbose, auto_confirm, debug);
                continue 'repl;
            }
            ":v" => {
                verbose = !verbose;
                println!(
                    "詳細ログ: {BOLD}{}{RESET}",
                    if verbose { "ON" } else { "OFF" }
                );
                continue 'repl;
            }
            ":y" => {
                auto_confirm = !auto_confirm;
                println!(
                    "自動確認: {BOLD}{}{RESET}",
                    if auto_confirm { "ON" } else { "OFF" }
                );
                continue 'repl;
            }
            ":d" | ":debug" => {
                debug = !debug;
                println!(
                    "デバッグ: {BOLD}{}{RESET}",
                    if debug {
                        "ON (.copipe_logs/debug_log へ記録)"
                    } else {
                        "OFF"
                    }
                );
                continue 'repl;
            }
            ":undo list" => {
                let list = checkpoints.list();
                if list.is_empty() {
                    println!("{DIM}チェックポイントはありません{RESET}");
                } else {
                    println!(
                        "{BOLD}── チェックポイント（新しい順、:undo / :undo 0 / :undo 1 ...）──{RESET}"
                    );
                    for (i, (path, op)) in list.iter().enumerate() {
                        println!("  {DIM}[{i}]{RESET} ↩ {path} ({op})");
                    }
                }
                continue 'repl;
            }
            cmd if cmd == ":undo" || cmd.starts_with(":undo ") => {
                // :undo → 最新1件、:undo N → N番目を復元
                let idx: Option<usize> = cmd
                    .strip_prefix(":undo ")
                    .and_then(|s| s.trim().parse().ok());
                let result = if let Some(n) = idx {
                    checkpoints.undo_at(n)
                } else {
                    checkpoints.undo()
                };
                match result {
                    Ok(Some((path, op))) => {
                        println!("{GREEN_BOLD}✓{RESET} 復元しました: {path} ({op})")
                    }
                    Ok(None) => println!("{YELLOW}チェックポイントがありません{RESET}"),
                    Err(e) => println!("{RED_BOLD}復元失敗: {e}{RESET}"),
                }
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
                        "n" | "N" => break 'confirm Some(false),
                        other => {
                            println!("{DIM}「{other}」は無効です。Enter で実行、n で中止{RESET}")
                        }
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
        // 前回セッションがあり、タスクが同じなら自動復元
        let resume = session_store.load().filter(|s| s.user_task == task);
        if resume.is_some() {
            println!("{DIM}前回の続きから再開します...{RESET}");
        }
        tokio::select! {
            result = run_agent(&mut session, &root, &task, verbose, auto_confirm, debug, &mut checkpoints, &session_store, resume) => {
                match result {
                    Ok(true)  => println!("\n{GREEN_BOLD}✓ タスク完了{RESET}"),
                    // #2: MaxTurns 時の確認メッセージ（runner.rs の詳細メッセージの後に簡潔に）
                    Ok(false) => println!("{DIM}（同じタスクを再入力すると続きから再開します）{RESET}"),
                    Err(e)    => {
                        // #3: エラー時にタスク文字列を表示して再入力を楽にする
                        println!("\n{RED_BOLD}エラー: {e}{RESET}");
                        println!("{DIM}タスク: {task}{RESET}");
                    }
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

fn canonicalize_clean(path: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let canonical = path.canonicalize()?;
    // Windows の canonicalize は \\?\ プレフィックス（拡張パス）を返す場合があるので除去
    #[cfg(target_os = "windows")]
    {
        let s = canonical.to_string_lossy();
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            return Ok(std::path::PathBuf::from(stripped));
        }
    }
    Ok(canonical)
}

fn clean_task_input(input: &str) -> String {
    input
        .trim()
        .trim_start_matches("\u{1b}[200~")
        .trim_end_matches("\u{1b}[201~")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_task_input_removes_bracketed_paste_markers() {
        assert_eq!(
            clean_task_input("\u{1b}[200~debug_log.rs をレビューして\u{1b}[201~"),
            "debug_log.rs をレビューして"
        );
    }
}
