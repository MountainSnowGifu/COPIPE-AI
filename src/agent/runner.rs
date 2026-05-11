use super::context::{build_context_header, summarize_for_display};
use super::debug_log::DebugLogger;
use super::log::{write_ai_log, write_browser_log};
use super::parser::{SCHEMA_HINT, parse_blocks};
use super::rate_limiter::RateLimiter;
use super::session_store::{SessionData, SessionStore};
use super::task::{
    is_deferring_development_message, is_menu_selection_without_context, is_non_actionable_ack,
    is_placeholder_review_message, is_read_status_question, is_short_open_ended_development_task,
    looks_like_project_path, missing_referenced_project_paths,
    should_short_circuit_non_actionable_task, should_short_circuit_read_status_task,
    task_mentions_explicit_filename, task_requires_development_action, task_requires_file_update,
    task_requires_review_output,
};
use crate::color::{BOLD, CYAN_BOLD, DIM, GREEN, RED, RED_BOLD, RESET, YELLOW};
use crate::command::AiCommand;
use crate::command::TodoStatus;
use crate::executor::errors::is_error_output;
use crate::executor::pre_hooks::is_destructive_tool;
use crate::executor::tools::{todo_write, worktree};
use crate::executor::{CheckpointManager, ToolResult, execute, format_tool_results};
use crate::session::{CopilotSession, ai_message_count, get_codeblocks_from_dom, read_nth_ai_text};
use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const MAX_TURNS: u32 = 30;

// ─── Continuation パターン（tool-use-flow.md §13）────────────────────────────
//
// 各ターンの終わりに「次に何をするか」を TurnOutcome として明示する。
// ループは match outcome { ... } の一本道になる。

/// ツール種別に基づいてコマンドリストをフィルタリングする（llm-prompts.md §4）
///
/// - 読み取り専用ツールは auto_confirm の値に関わらず常に実行許可
/// - 破壊的ツールは auto_confirm=false のとき実行前にインライン確認を表示
/// - ユーザーが "n" を入力した場合はそのコマンドをスキップする
async fn filter_by_permission(
    commands: Vec<crate::command::AiCommand>,
    auto_confirm: bool,
) -> Vec<crate::command::AiCommand> {
    use crate::command::AiCommand;
    use std::io::Write as _;

    if auto_confirm {
        return commands; // auto_confirm=true なら全コマンドを通す
    }

    // 破壊的操作があるか確認
    let destructive_summary: Vec<(usize, String)> = commands
        .iter()
        .enumerate()
        .filter_map(|(i, cmd)| {
            let (tool, label) = match cmd {
                AiCommand::File { path, .. } => ("write_file", format!("WriteFile({path})")),
                AiCommand::Edit { path, .. } => ("edit", format!("Edit({path})")),
                AiCommand::MultiEdit { path, .. } => ("multi_edit", format!("MultiEdit({path})")),
                AiCommand::Patch { path, .. } => ("patch", format!("Patch({path})")),
                AiCommand::DeleteFile { path } => ("delete_file", format!("DeleteFile({path})")),
                AiCommand::Cmd { name, cmd, .. } => {
                    ("cmd", format!("Cmd({name}: {})", cmd.join(" ")))
                }
                AiCommand::EnterWorktree => ("enter_worktree", "EnterWorktree".to_string()),
                AiCommand::ExitWorktree { action, .. } => {
                    ("exit_worktree", format!("ExitWorktree(action={action})"))
                }
                _ => return None,
            };
            if is_destructive_tool(tool) {
                Some((i, label))
            } else {
                None
            }
        })
        .collect();

    if destructive_summary.is_empty() {
        return commands; // 読み取り専用のみ → 確認なしで通す
    }

    // 破壊的操作があれば一覧表示して確認
    println!();
    println!("  \x1b[33m⚠ 変更操作が含まれます:\x1b[0m");
    for (_, label) in &destructive_summary {
        println!("    \x1b[2m→ {label}\x1b[0m");
    }
    print!("  続けますか? \x1b[1m[Enter=実行 / n=スキップ]\x1b[0m ");
    std::io::stdout().flush().ok();

    let answer = tokio::task::spawn_blocking(|| {
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf).ok();
        buf.trim().to_lowercase()
    })
    .await
    .unwrap_or_default();

    println!();

    if answer == "n" || answer == "no" {
        // 破壊的コマンドをスキップして読み取り系のみ残す
        let skip_indices: std::collections::HashSet<usize> =
            destructive_summary.iter().map(|(i, _)| *i).collect();
        println!("  \x1b[2m破壊的操作をスキップしました（読み取り操作は継続）\x1b[0m");
        commands
            .into_iter()
            .enumerate()
            .filter(|(i, _)| !skip_indices.contains(i))
            .map(|(_, cmd)| cmd)
            .collect()
    } else {
        commands // Enter または y → 全コマンドを実行
    }
}

/// 1ターンの処理結果を表す明示的な状態
#[derive(Debug)]
enum TurnOutcome {
    /// AI が bot コマンドを返した → 正常完了（tool-use-flow.md の stop_reason: end_turn 相当）
    Done,
    /// ツール実行結果を持って次ターンへ継続（continuation: true 相当）
    Continue { prompt: String },
    /// AI がテキストのみ返した → JSON コマンドを改めて要求
    NudgeForJson { prompt: String },
    /// 応答からコマンドを取得できなかった → ユーザーへ通知して終了
    #[allow(dead_code)]
    NoCommands,
    /// 最大ターン数に達した
    MaxTurns,
}

/// ターン終了時の状態を判定して TurnOutcome を返す
fn determine_outcome(
    user_task: &str,
    bot_message: Option<&str>,
    is_done: bool,
    only_txt: bool,
    tool_results: &[ToolResult],
    turn: u32,
    ctx: &str,
    consecutive_txt: u32,
    read_files: &HashSet<PathBuf>,
    root: &Path,
    has_successful_file_update: bool,
    consecutive_read_file: u32,
    consecutive_ask_user: u32,
    consecutive_edit_fail: u32,
    last_failed_edit_path: &str,
) -> TurnOutcome {
    if is_done && let Some(message) = bot_message {
        let missing_paths = missing_referenced_project_paths(message, root);
        if !missing_paths.is_empty() {
            return TurnOutcome::NudgeForJson {
                prompt: format!(
                    "{ctx}\n\n\
                    [⚠ 最終回答に、現在の作業ディレクトリに存在しないパスが含まれています]\n\
                    存在しないパス: {}\n\
                    ログや過去文脈のファイル名を現在の事実として扱わないでください。\
                    `glob` / `grep` / `read_file` の実行結果で実在を確認したファイルだけを根拠にして、\
                    必要なら調査をやり直してください。\n\
                    ```json\n{{\"type\":\"glob\",\"pattern\":\"src/**/*.rs\"}}\n```",
                    missing_paths.join(", ")
                ),
            };
        }
    }

    if is_done && task_requires_review_output(user_task) {
        if bot_message
            .map(is_placeholder_review_message)
            .unwrap_or(true)
        {
            return TurnOutcome::NudgeForJson {
                prompt: format!(
                    "{ctx}\n\n\
                    レビュー本文がまだ出ていません。段取り説明や「次に返します」では完了にしないでください。\
                    今すぐ `bot` の message に、具体的な指摘・根拠・改善案を含むレビュー本文を省略せず入れてください。\n\
                    ```json\n{{\"type\":\"bot\",\"message\":\"（ここにレビュー本文。『次に返します』は禁止）\"}}\n```"
                ),
            };
        }
    }

    if is_done
        && task_requires_development_action(user_task)
        && !has_successful_file_update
        && bot_message
            .map(is_deferring_development_message)
            .unwrap_or(false)
    {
        return TurnOutcome::NudgeForJson {
            prompt: format!(
                "{ctx}\n\n\
                [⚠ 開発タスクを質問だけで終了しようとしています]\n\
                この依頼は実装・修正・改善系のタスクです。方向性が完全に指定されていなくても、\
                現在の作業ディレクトリに実在するファイルを根拠に、最小で保守的な改善を1つ選んで進めてください。\
                ログ内のファイル名や crate 名は現在の事実として扱わないでください。\
                必要なら `glob` / `grep` で対象を絞り、実装後に `cargo check` してください。\n\
                ```json\n{{\"type\":\"glob\",\"pattern\":\"src/**/*.rs\"}}\n```"
            ),
        };
    }

    // ask_user 連打（2回以上）検知 → 手元の情報で進むよう誘導
    if consecutive_ask_user >= 2 && !is_done {
        return TurnOutcome::NudgeForJson {
            prompt: format!(
                "{ctx}\n\n\
                [⚠ ask_user を連続で送っています ({consecutive_ask_user}回目)]\
                \nユーザーの回答が短い・曖昧であっても、再度 ask_user で詳細を聞き返さないでください。\
                \n手元の情報で最善の判断をして作業を進めてください。\
                \n判断できないなら `bot` で現状と次のステップ候補を提示してください。\
                \n```json\
                \n{{\"type\": \"bot\", \"message\": \"（現状と選択肢）\"}}\
                \n```"
            ),
        };
    }

    // edit / multi_edit の連続失敗（2回以上）検知 → 再読み込みを強制
    if consecutive_edit_fail >= 2 && !is_done && !last_failed_edit_path.is_empty() {
        return TurnOutcome::NudgeForJson {
            prompt: format!(
                "{ctx}\n\n\
                [⚠ `{last_failed_edit_path}` への edit が {consecutive_edit_fail} 回連続で失敗しています]\n\
                old_string が現在のファイル内容と一致していません。\n\
                ファイルは既に変更されているか、old_string に余分な空白・改行が含まれている可能性があります。\n\
                既に変更済みなら `bot` で完了を報告してください。\n\
                まだ必要なら改めて read_file でファイルの実際の内容を確認し、old_string を正確に合わせてください。\n\
                ```json\n{{\"type\":\"read_file\",\"path\":\"{last_failed_edit_path}\"}}\n```"
            ),
        };
    }

    // 書き込み成功後に read_file で再確認するだけの無駄ループ検知
    // file/edit で書き込んだ直後に read_file のみ返してきた場合 → bot への誘導
    if has_successful_file_update
        && task_requires_file_update(user_task)
        && !is_done
        && !tool_results.is_empty()
        && tool_results
            .iter()
            .all(|r| r.label.starts_with("ReadFile("))
    {
        return TurnOutcome::NudgeForJson {
            prompt: format!(
                "{ctx}\n\n\
                [ファイルの保存は既に成功しています]\n\
                書き込み済みのファイルを再度 read_file する必要はありません。\n\
                `bot` で完了を報告してください。\n\
                ```json\n{{\"type\":\"bot\",\"message\":\"完了しました。\"}}\
```"
            ),
        };
    }

    // read_file 連打（3件以上）で grep/bot への誘導
    // context.rs のソフト警告（recent_reads >= 3）と閾値を合わせ、
    // ソフト警告を無視してもう1ファイル読んだ時点ですぐハードnudgeを返す
    if consecutive_read_file >= 3 && !is_done {
        return TurnOutcome::NudgeForJson {
            prompt: format!(
                "{ctx}\n\n\
                [⚠ 連続 read_file が {consecutive_read_file} 件に達しました]\n\
                これ以上ファイルを順番に読み続けるのは非効率です。\n\
                次のいずれかを実行してください:\n\
                1. 今読んだファイルの情報で十分なら `bot` でレビュー内容を返してください\n\
                2. 特定の関数・変数を探すなら `grep` を使ってください\n\
                3. 大きなファイルの続きを読む場合は offset_lines を指定して同一ターンにまとめてください\n\
                ```json\n\
                {{\"type\": \"grep\", \"pattern\": \"キーワード\", \"path\": \"src\"}}\n\
                ```"
            ),
        };
    }
    if is_done && task_requires_file_update(user_task) && !has_successful_file_update {
        let file_hint = read_file_hint(read_files, root);
        return TurnOutcome::NudgeForJson {
            prompt: format!(
                "{ctx}\n\n{file_hint}\n\
                この依頼はファイル内容の書き換え・翻訳タスクです。まだ file / edit / multi_edit / patch による保存が成功していません。\
                bot で完了報告せず、読み込み済みファイルの本文を変換して `file` コマンドで同じ path に保存してください。\n\
                ```json\n{{\"type\":\"file\",\"path\":\"対象ファイル\",\"content\":\"（変換後の全文）\"}}\n```"
            ),
        };
    }

    if is_done {
        return TurnOutcome::Done;
    }

    if only_txt {
        let file_hint = read_file_hint(read_files, root);
        let action_hint = if task_requires_file_update(user_task) && !has_successful_file_update {
            "\nこの依頼はファイル内容の書き換え・翻訳タスクです。通常テキストで回答せず、変換後の全文を `file` コマンドで保存してください：\
            \n```json\n{\"type\": \"file\", \"path\": \"対象ファイル\", \"content\": \"（変換後の全文）\"}\n```"
        } else if consecutive_txt >= 2 {
            "\n今すぐ `bot` コマンドで回答を出力してください。省略せず完全な内容を含めること。\
            \n```json\n{\"type\": \"bot\", \"message\": \"（完全な回答をここに）\"}\n```"
        } else {
            "\n**必ず ```json コードブロックで** 次のアクションを記述してください。\
            \nタスクが完了なら:\n```json\n{\"type\": \"bot\", \"message\": \"（完全な回答）\"}\n```\
            \nまだ作業がある場合は read_file / grep / edit 等のコマンドをJSONで返してください。\
            \nプレーンテキストのみの返答は受け付けられません。"
        };
        return TurnOutcome::NudgeForJson {
            prompt: format!("{ctx}\n\n{file_hint}{action_hint}"),
        };
    }

    if tool_results.is_empty() {
        return TurnOutcome::Done;
    }

    if turn + 1 >= MAX_TURNS {
        return TurnOutcome::MaxTurns;
    }

    // コンパイラの help: 提案がある場合は追加調査なしで直接修正するよう促す
    let compiler_hint = if tool_results.iter().any(|r| {
        r.label.starts_with("Cmd(")
            && crate::executor::errors::is_error_output(&r.output)
            && r.output.contains("help:")
            && (r.output.contains("error[E") || r.output.contains("error:"))
    }) {
        "\n\n[コンパイラが修正提案を示しています]\n\
        上記エラー出力の `help:` 行を参考に、追加の read_file や grep を挟まず、\
        今すぐ `edit` または `multi_edit` でファイルを直接修正してください。"
    } else {
        ""
    };

    let recovery_hint = recovery_hint_for_tool_results(tool_results);

    // 通常の継続：ツール結果を次のプロンプトに組み込む
    TurnOutcome::Continue {
        // system_prompt.md §「動的コンテキストの形式」に準拠したセクション構造
        prompt: format!(
            "{ctx}\n\n## Tool results\n{}{}{}",
            format_tool_results(tool_results),
            compiler_hint,
            recovery_hint
        ),
    }
}

fn recovery_hint_for_tool_results(tool_results: &[ToolResult]) -> String {
    let missing_read_paths: Vec<String> = tool_results
        .iter()
        .filter(|r| r.label.starts_with("ReadFile(") && r.output.contains("ファイルが存在しません"))
        .filter_map(|r| label_inner(&r.label).map(str::to_string))
        .collect();

    let missing_path_hint = if missing_read_paths.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n[存在しないファイルを読もうとしました]\n\
            存在しないパス: {}\n\
            そのファイルの内容や crate 構造を推測して回答しないでください。\
            `glob` / `list_dir` / `grep` で現在の作業ディレクトリに実在するファイルを確認し、\
            実在するファイルだけを根拠に次のアクションを決めてください。",
            missing_read_paths.join(", ")
        )
    };

    let blocked_cargo_hint = if tool_results.iter().any(|r| {
        r.label.starts_with("Cmd(")
            && r.output.contains("cargo test")
            && r.output.contains("任意コードを実行")
    }) {
        "\n\n[cargo test は実行できません]\n\
        テストのコンパイル確認が目的なら、次は `cmd` で `[\"cargo\",\"check\",\"--tests\"]` \
        または `[\"cargo\",\"check\",\"--all-targets\"]` を実行してください。\
        実際のテスト実行が必須なら、`bot` で実行不可と残リスクを報告してください。"
            .to_string()
    } else {
        String::new()
    };

    let parent_dir_hint = if tool_results.iter().any(|r| {
        r.label.starts_with("WriteFile(") && r.output.contains("親ディレクトリが存在しません")
    }) {
        "\n\n[書き込み先の親ディレクトリがありません]\n\
        同じ `file` を再試行する前に、必要な親ディレクトリを `mkdir` で作成してください。\
        例: `.github/workflows/file.yml` なら先に `.github` と `.github/workflows` を作成します。"
            .to_string()
    } else {
        String::new()
    };

    format!("{missing_path_hint}{blocked_cargo_hint}{parent_dir_hint}")
}

fn label_inner(label: &str) -> Option<&str> {
    label
        .find('(')
        .and_then(|start| label.strip_suffix(')').map(|s| &s[start + 1..]))
}

fn read_file_hint(read_files: &HashSet<PathBuf>, root: &Path) -> String {
    if read_files.is_empty() {
        return "まだファイルを読み込んでいません。read_file コマンドでファイルを読んでください。"
            .to_string();
    }

    let mut files: Vec<String> = read_files
        .iter()
        .filter_map(|p| p.strip_prefix(root).ok())
        .map(|p| p.display().to_string())
        .collect();
    files.sort();
    format!("読み込み済みのファイル（再読不要）: {}。", files.join(", "))
}

fn build_initial_prompt(user_task: &str) -> String {
    if is_non_actionable_ack(user_task) || is_menu_selection_without_context(user_task) {
        return format!(
            "## New task boundary\n\
            これは新規タスクです。過去の Copilot チャット文脈や別環境のログ内容を続きとして扱わないでください。\n\n\
            [元のタスク] {user_task}\n\n\
            依頼内容が短い相づちだけで、現在のコードベースに対する具体的な作業内容がありません。\
            ツールを実行せず、`bot` で「具体的な作業内容を入力してください」と簡潔に返してください。"
        );
    }

    let boundary = "## New task boundary\n\
        これは新規タスクです。Dynamic context に前回セッションからの再開が明示されていない限り、\
        過去の Copilot チャット文脈や別環境のログ内容を続きとして扱わないでください。\
        ユーザーが明示的に提示したログは失敗パターンの診断材料として扱ってかまいませんが、\
        ログ内の作業対象・crate 名・ファイル名・実行結果を現在の環境の事実として採用しないでください。\
        現在の作業ディレクトリに存在するファイルだけを根拠にしてください。\n\n";

    if task_requires_review_output(user_task) {
        return format!(
            "{boundary}{user_task}\n\n\
            [効率化ヒント]\n\
            - レビュー対象ファイルが依頼文から明確なら、空 grep を挟まず直接 read_file してください。\n\
            - glob や list_dir の結果が返ったら、それを根拠に調査対象を絞り込んでください（「見つからない」と判断して list_dir を重ねないこと）。\n\
            - grep を使う場合は、必ず空でない具体的な pattern を指定してください。\n\
            - 調査が終わったら、bot の message に具体的な指摘・根拠・改善案を省略せず書いてください。\n\
            - 「完了しました」「次の指示をください」だけの短い bot メッセージは不可です。"
        );
    }

    if is_short_open_ended_development_task(user_task) {
        return format!(
            "{boundary}{user_task}\n\n\
            [短い開発依頼への進め方]\n\
            依頼が抽象的でも、具体化質問やプレーンテキスト回答で止めないでください。\
            まず現在の作業ディレクトリに実在するファイルを確認し、そこから実装対象を絞ってください。\
            調査後に候補だけを並べてユーザーへ方向性を聞かず、最小で保守的な改善を1つ選んで実装してください。\
            最初の返答は必ず次の形の JSON コードブロックにしてください:\n\
            ```json\n\
            [\n\
              {{\"type\":\"txt\",\"content\":\"短い依頼なので、まず現在のプロジェクト構成を確認します\"}},\n\
              {{\"type\":\"glob\",\"pattern\":\"src/**/*.rs\"}}\n\
            ]\n\
            ```"
        );
    }

    let next_action_hint = if task_mentions_explicit_filename(user_task) {
        "次のアクションを必ず ```json コードブロックで返してください。\
        依頼文にファイル名が明示されている場合は、glob を省略して直接 `read_file` でファイルを確認してください。"
    } else {
        "次のアクションを必ず ```json コードブロックで返してください。\
        まだ対象ファイルが不明な場合は、まず `txt` と `glob` で現在のプロジェクト構成を確認してください。"
    };

    format!("{boundary}{user_task}\n\n{next_action_hint}")
}

fn is_successful_file_update_result(r: &ToolResult) -> bool {
    !is_error_output(&r.output)
        && (r.label.starts_with("WriteFile(")
            || r.label.starts_with("Edit(")
            || r.label.starts_with("MultiEdit(")
            || r.label.starts_with("Patch("))
}

// ─── コマンド取得 ─────────────────────────────────────────────────────────────

async fn get_commands(
    session: &mut CopilotSession,
    root: &std::path::Path,
    prompt: &str,
    verbose: bool,
) -> anyhow::Result<(Vec<AiCommand>, Vec<String>)> {
    write_browser_log(
        root,
        &format!("before send_raw: prompt_len={}", prompt.len()),
        session,
    )
    .await;
    match tokio::time::timeout(Duration::from_secs(210), session.send_raw(prompt)).await {
        Ok(Ok(())) => {
            write_browser_log(root, "after send_raw: ok", session).await;
        }
        Ok(Err(e)) => {
            write_browser_log(root, &format!("send_raw error: {e}"), session).await;
            return Err(e);
        }
        Err(_) => {
            write_browser_log(root, "send_raw timeout after 210s", session).await;
            anyhow::bail!("Copilot との通信がタイムアウトしました。同じタスクを再入力してください");
        }
    }

    let n = match ai_message_count(&session.page).await {
        Ok(n) => n,
        Err(e) => {
            write_browser_log(root, &format!("ai_message_count error: {e}"), session).await;
            return Err(e);
        }
    };

    let blocks = {
        let mut last = get_codeblocks_from_dom(&session.page, n).await;
        for attempt in 1..=3u32 {
            let (_, errs) = parse_blocks(&last);
            if !errs.iter().any(|e| e.contains("EOF")) {
                break;
            }
            if verbose {
                eprintln!("{DIM}[再取得中 {attempt}/3]{RESET}");
            }
            tokio::time::sleep(Duration::from_secs(attempt as u64 * 2)).await;
            let refreshed = get_codeblocks_from_dom(&session.page, n).await;
            if refreshed != last {
                last = refreshed;
            }
        }
        last
    };

    if blocks.is_empty() {
        if verbose {
            eprintln!("{DIM}[応答再要求]{RESET}");
        }
        let first_text = read_nth_ai_text(&session.page, n).await;
        write_browser_log(root, "no JSON code block in latest AI message", session).await;
        if let Some(block) = plain_json_block(&first_text) {
            let parsed = parse_blocks(&[block.to_string()]);
            if !parsed.0.is_empty() || parsed.1.is_empty() {
                return Ok(parsed);
            }
        }
        if let Err(e) = session
            .send_raw(&format!(
                "JSONコードブロックが見つかりませんでした。次のアクションを必ず ```json ... ``` で出力してください。\n\n{SCHEMA_HINT}"
            ))
            .await
        {
            write_browser_log(root, &format!("retry send_raw error: {e}"), session).await;
            return Err(e);
        }
        let n2 = ai_message_count(&session.page).await?;
        let blocks2 = get_codeblocks_from_dom(&session.page, n2).await;
        if blocks2.is_empty() {
            write_browser_log(root, "still no JSON code block after retry", session).await;
            let retry_text = read_nth_ai_text(&session.page, n2).await;
            if let Some(message) = substantive_plain_text_response(&retry_text)
                .or_else(|| substantive_plain_text_response(&first_text))
            {
                return Ok((
                    vec![AiCommand::Bot {
                        message: Some(message),
                        content: None,
                    }],
                    Vec::new(),
                ));
            }
        }
        return Ok(parse_blocks(&blocks2));
    }

    Ok(parse_blocks(&blocks))
}

fn plain_json_block(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if (trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
    {
        Some(trimmed)
    } else {
        None
    }
}

fn substantive_plain_text_response(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.chars().count() < 160 {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let looks_like_schema_nudge = lower.contains("json")
        && (trimmed.contains("\"type\"")
            || trimmed.contains("read_file")
            || trimmed.contains("list_dir")
            || trimmed.contains("次の作業ステップ"));
    if looks_like_schema_nudge {
        return None;
    }
    Some(trimmed.to_string())
}

// ─── メインエージェントループ ─────────────────────────────────────────────────

/// Ok(true) = 正常完了、Ok(false) = 最大ターン数到達
pub async fn run_agent(
    session: &mut CopilotSession,
    root: &std::path::Path,
    user_task: &str,
    verbose: bool,
    auto_confirm: bool,
    debug: bool,
    checkpoints: &mut CheckpointManager,
    session_store: &SessionStore,
    resume: Option<SessionData>,
) -> anyhow::Result<bool> {
    let mut dbg = DebugLogger::new(root, debug);
    if should_short_circuit_non_actionable_task(user_task, resume.as_ref()) {
        println!(
            "{YELLOW}具体的な作業内容を入力してください。保存済みセッションを再開する場合は、同じタスクを再入力してください。{RESET}"
        );
        return Ok(true);
    }
    if should_short_circuit_read_status_task(user_task, resume.as_ref()) {
        println!(
            "{YELLOW}この新規タスクでは、まだファイル本文は読み込んでいません。読んで要約・確認する場合は、その作業内容を入力してください。{RESET}"
        );
        return Ok(true);
    }

    // 未完了の todo があれば冒頭に表示（前回の続きを把握するため）
    let existing_todos = todo_write::load(root);
    let has_pending = existing_todos
        .iter()
        .any(|t| t.status != TodoStatus::Completed);
    if has_pending {
        println!("{}", todo_write::format_todos(&existing_todos));
        println!();
    }

    // アクティブな worktree があれば effective_root を切り替え
    let mut effective_root = root.to_path_buf();
    if let Some(wt) = worktree::load_state(root) {
        println!(
            "{DIM}[worktree] 隔離ブランチ '{}' で作業中{RESET}",
            wt.branch
        );
        effective_root = wt.path;
    }

    // 前回セッションから状態を復元（または新規開始）
    let (mut prompt, mut read_files, mut done_log) = if let Some(ref data) = resume {
        let read_files: HashSet<PathBuf> =
            data.read_files.iter().map(|rel| root.join(rel)).collect();
        let done_log = data.done_log.clone();

        // #6: 再開時に done_log を表示してユーザーが状況を把握できるようにする
        if !done_log.is_empty() {
            println!("{DIM}── 前回の完了済み操作 ──{RESET}");
            for item in done_log
                .iter()
                .rev()
                .take(5)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                if item.starts_with('✓') {
                    println!("  {GREEN}{item}{RESET}");
                } else {
                    println!("  {RED}{item}{RESET}");
                }
            }
            if done_log.len() > 5 {
                println!("  {DIM}... 他 {} 件{RESET}", done_log.len() - 5);
            }
            println!();
        }

        let ctx = build_context_header(user_task, &read_files, root, &done_log);
        let resume_prompt = format!(
            "{ctx}\n\n[前回セッションからの再開（{}ターン完了済み）]\n\
            上記の状況を踏まえ、残りの作業を続けてください。",
            data.turn_count
        );
        (resume_prompt, read_files, done_log)
    } else {
        (build_initial_prompt(user_task), HashSet::new(), Vec::new())
    };

    let mut consecutive_txt = 0u32;
    let mut parse_error_count = 0u32;
    let mut reached_max = false;
    let mut consecutive_read_file = 0u32; // read_file 連打検知用
    let mut consecutive_ask_user = 0u32; // ask_user 連打検知用
    let mut consecutive_edit_fail = 0u32; // 同一ファイルへの edit 連続失敗検知用
    let mut last_failed_edit_path = String::new(); // 直前の失敗 edit のパス
    let mut rate_limiter = RateLimiter::new();
    let mut total_turns = resume.as_ref().map(|d| d.turn_count).unwrap_or(0);
    let mut has_successful_file_update = done_log.iter().any(|s| {
        s.starts_with("✓ WriteFile(")
            || s.starts_with("✓ Edit(")
            || s.starts_with("✓ MultiEdit(")
            || s.starts_with("✓ Patch(")
    });

    'agent: for turn in 0..MAX_TURNS {
        total_turns += 1;
        rate_limiter.advance();
        dbg.turn_start(turn, MAX_TURNS);

        // ターン間待機: 指数バックオフ + ジッター（§4 Rate Limiter）
        if turn > 0 {
            let wait_ms = rate_limiter.next_delay_ms();
            dbg.log_rate_limiter(rate_limiter.consecutive_issues(), wait_ms);
            if verbose {
                let issues = rate_limiter.consecutive_issues();
                if issues > 0 {
                    eprintln!(
                        "{DIM}待機 {:.1}秒 (バックオフ lv.{issues}){RESET}",
                        wait_ms as f64 / 1000.0
                    );
                } else {
                    eprintln!("{DIM}待機 {:.1}秒...{RESET}", wait_ms as f64 / 1000.0);
                }
            } else {
                let dot_count = ((wait_ms / 1000) as usize).min(5);
                let dots = ".".repeat(dot_count);
                print!("{DIM}{dots}{RESET}");
                std::io::stdout().flush().ok();
            }
            tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
            if !verbose {
                let dot_count = ((wait_ms / 1000) as usize).min(5);
                print!("\r{}\r", " ".repeat(dot_count));
                std::io::stdout().flush().ok();
            }
        }

        // #10: ステップ表示を DIM なし（視認しやすく）
        println!("● ステップ {}/{}", turn + 1, MAX_TURNS);

        // ── 1. AI からコマンドを取得 ───────────────────────────────────────
        // #1: 送信中インジケーター（get_commands は最大 210 秒かかるため）
        {
            use std::io::Write as _;
            eprint!("  {DIM}Copilot に送信中...{RESET}");
            std::io::stderr().flush().ok();
        }

        dbg.log_prompt(&prompt);
        dbg.log_session(&read_files, &done_log, root);

        let (commands, parse_errors) = get_commands(session, root, &prompt, verbose).await?;
        // 送信中メッセージをクリア
        {
            use std::io::Write as _;
            eprint!("\r                          \r");
            std::io::stderr().flush().ok();
        }
        write_ai_log(root, turn, &commands, &parse_errors);

        // ParseError があれば問題を記録、なければ回復
        if !parse_errors.is_empty() {
            rate_limiter.record_issue();
            // #9: 非verbose でも軽微な警告を表示
            if !verbose {
                eprintln!("  {DIM}[解析エラー、リトライ中]{RESET}");
            }
        } else {
            rate_limiter.record_success();
        }

        let mut tool_results: Vec<ToolResult> = parse_errors
            .into_iter()
            .map(|e| ToolResult::new("ParseError", e))
            .collect();

        // コマンドもエラーもない → NoCommands として終了
        if commands.is_empty() && tool_results.is_empty() {
            parse_error_count += 1;
            let hint = if parse_error_count >= 2 {
                "\n  ヒント: タスクをより具体的に書くか、短い指示（例: list_dir src）から始めてみてください"
            } else {
                ""
            };
            println!(
                "{YELLOW}応答からコマンドを取得できませんでした。タスクを再入力してください。{hint}{RESET}"
            );
            return Ok(false);
        }

        // ── 1.5 実行前に AI の計画を表示 ──────────────────────────────────────
        // AI の txt コメントを先行表示（before tools run）
        let txt_lines: Vec<&str> = commands
            .iter()
            .filter_map(|c| {
                if let AiCommand::Txt { content } = c {
                    Some(content.as_str())
                } else {
                    None
                }
            })
            .collect();
        if !txt_lines.is_empty() {
            println!(
                "  {CYAN_BOLD}┌─ AIの判断 {RESET}{CYAN_BOLD}─────────────────────────────────────{RESET}"
            );
            for line in &txt_lines {
                println!("  {CYAN_BOLD}│{RESET} {line}");
            }
            println!("  {CYAN_BOLD}└────────────────────────────────────────────{RESET}");
        }
        // 実行予定ツールの一覧（txt/bot 以外）
        let planned: Vec<String> = commands
            .iter()
            .filter_map(|c| match c {
                AiCommand::ReadFile { path, offset_lines } => Some(if *offset_lines > 0 {
                    format!("read_file {path} (続き)")
                } else {
                    format!("read_file {path}")
                }),
                AiCommand::ListDir { path } => Some(format!("list_dir {path}")),
                AiCommand::Glob { pattern } => Some(format!("glob {pattern}")),
                AiCommand::Grep { pattern, path, .. } => {
                    Some(format!("grep \"{pattern}\" ({path})"))
                }
                AiCommand::File { path, .. } => Some(format!("write_file {path}")),
                AiCommand::Edit { path, .. } => Some(format!("edit {path}")),
                AiCommand::MultiEdit { path, .. } => Some(format!("multi_edit {path}")),
                AiCommand::Patch { path, .. } => Some(format!("patch {path}")),
                AiCommand::DeleteFile { path } => Some(format!("delete_file {path}")),
                AiCommand::Cmd { name, cmd, .. } => Some(if name.is_empty() {
                    format!("cmd {}", cmd.join(" "))
                } else {
                    format!("cmd [{name}] {}", cmd.join(" "))
                }),
                AiCommand::AskUser { question, .. } => Some(format!(
                    "ask_user: {}",
                    question.chars().take(60).collect::<String>()
                )),
                AiCommand::EnterWorktree => Some("enter_worktree".to_string()),
                AiCommand::ExitWorktree { action, .. } => Some(format!("exit_worktree ({action})")),
                AiCommand::TodoWrite { .. } => Some("todo_write".to_string()),
                AiCommand::WebFetch { url, .. } => Some(format!("web_fetch {url}")),
                AiCommand::Mkdir { path } => Some(format!("mkdir {path}")),
                _ => None,
            })
            .collect();
        if !planned.is_empty() {
            let phase = {
                let has_write = commands.iter().any(|c| {
                    matches!(
                        c,
                        AiCommand::File { .. }
                            | AiCommand::Edit { .. }
                            | AiCommand::MultiEdit { .. }
                            | AiCommand::Patch { .. }
                            | AiCommand::DeleteFile { .. }
                            | AiCommand::Mkdir { .. }
                    )
                });
                let has_cmd = commands.iter().any(|c| matches!(c, AiCommand::Cmd { .. }));
                let has_read = commands.iter().any(|c| {
                    matches!(
                        c,
                        AiCommand::ReadFile { .. }
                            | AiCommand::ListDir { .. }
                            | AiCommand::Grep { .. }
                            | AiCommand::Glob { .. }
                    )
                });
                if has_write {
                    "📝 修正"
                } else if has_cmd {
                    "⚙  実行"
                } else if has_read {
                    "🔍 調査"
                } else {
                    "   処理"
                }
            };
            println!("  {DIM}─── {phase} ──────────────────────────────────{RESET}");
            for p in &planned {
                println!("  {DIM}→{RESET} {p}");
            }
        }

        // ── 2. ツールを実行 ───────────────────────────────────────────────
        let is_done = commands.iter().any(|c| matches!(c, AiCommand::Bot { .. }))
            && !commands
                .iter()
                .any(|c| !matches!(c, AiCommand::Bot { .. } | AiCommand::Txt { .. }));
        let bot_message = commands.iter().find_map(|c| {
            if let AiCommand::Bot { message, content } = c {
                message
                    .as_deref()
                    .or(content.as_deref())
                    .map(|s| s.to_string())
            } else {
                None
            }
        });
        let only_txt = !is_done
            && tool_results.is_empty()
            && commands.iter().all(|c| matches!(c, AiCommand::Txt { .. }));

        // ask_user 連打の事前ブロック:
        // consecutive_ask_user >= 1 のときに ask_user を再度実行すると
        // ユーザーが複数回連続で質問を受けてしまうため、execute() の前にブロックする。
        if consecutive_ask_user >= 1
            && !is_done
            && commands
                .iter()
                .any(|c| matches!(c, AiCommand::AskUser { .. }))
        {
            consecutive_ask_user += 1;
            consecutive_txt += 1;
            let ctx = build_context_header(user_task, &read_files, root, &done_log);
            prompt = format!(
                "{ctx}\n\n\
                [⚠ ask_user を連続で送っています ({consecutive_ask_user}回目)]\
                \nユーザーの回答が短い・曖昧であっても、再度 ask_user で詳細を聞き返さないでください。\
                \n手元の情報で最善の判断をして作業を進めてください。\
                \n判断できないなら `bot` で現状と次のステップ候補を提示してください。\
                \n```json\
                \n{{\"type\": \"bot\", \"message\": \"（現状と選択肢）\"}}\
                \n```"
            );
            dbg.turn_end();
            continue;
        }

        // ツール種別による自動確認スキップ（llm-prompts.md §4）
        // 読み取り系のみ → 確認なし / 破壊的操作あり + !auto_confirm → インライン確認
        let commands = filter_by_permission(commands, auto_confirm).await;

        let (exec_results, messages) =
            execute(&effective_root, &commands, &mut read_files, checkpoints).await;

        // enter_worktree / exit_worktree で root が変わった場合に追従
        effective_root = worktree::load_state(root)
            .map(|wt| wt.path)
            .unwrap_or_else(|| root.to_path_buf());
        for r in &exec_results {
            if is_successful_file_update_result(r) {
                has_successful_file_update = true;
            }
            if is_error_output(&r.output) {
                done_log.push(format!(
                    "✗ {} → {}",
                    r.label,
                    r.output.splitn(2, ": ").nth(1).unwrap_or("").trim()
                ));
            } else {
                done_log.push(format!("✓ {}", r.label));
            }
        }
        tool_results.extend(exec_results);

        // read_file 連打カウント更新
        // Txt コマンドが混在しても read_file があれば加算する（以前は all() のため Txt 混在で誤リセットされていた）
        // Glob / ListDir は「構造探索」であり作業進捗とは見なさない（Grep は対象検索なので進捗扱い）
        let read_file_count = commands
            .iter()
            .filter(|c| matches!(c, AiCommand::ReadFile { .. }))
            .count() as u32;
        let has_progress = commands.iter().any(|c| {
            matches!(
                c,
                AiCommand::Grep { .. }
                    | AiCommand::Bot { .. }
                    | AiCommand::File { .. }
                    | AiCommand::Edit { .. }
                    | AiCommand::MultiEdit { .. }
                    | AiCommand::Patch { .. }
                    | AiCommand::Cmd { .. }
            )
        });
        if read_file_count > 0 && !has_progress {
            consecutive_read_file += read_file_count;
        } else {
            consecutive_read_file = 0;
        }

        // ask_user 連打カウント更新
        if commands
            .iter()
            .any(|c| matches!(c, AiCommand::AskUser { .. }))
        {
            consecutive_ask_user += 1;
        } else {
            consecutive_ask_user = 0;
        }

        // edit 連続失敗カウント更新
        // 直近の done_log エントリで同一ファイルへの edit が失敗し続けているか確認
        {
            let failed_edit_path = tool_results
                .iter()
                .filter(|r| {
                    crate::executor::errors::is_error_output(&r.output)
                        && (r.label.starts_with("Edit(") || r.label.starts_with("MultiEdit("))
                })
                .filter_map(|r| {
                    r.label
                        .trim_start_matches("Edit(")
                        .trim_start_matches("MultiEdit(")
                        .strip_suffix(')')
                        .map(|s| s.to_string())
                })
                .next();
            if let Some(path) = failed_edit_path {
                if path == last_failed_edit_path {
                    consecutive_edit_fail += 1;
                } else {
                    consecutive_edit_fail = 1;
                    last_failed_edit_path = path;
                }
            } else if commands
                .iter()
                .any(|c| matches!(c, AiCommand::Edit { .. } | AiCommand::MultiEdit { .. }))
            {
                // edit コマンドが成功した場合はリセット
                consecutive_edit_fail = 0;
                last_failed_edit_path.clear();
            }
        }

        for msg in &messages {
            println!("\n{CYAN_BOLD}[AI]{RESET} {msg}");
        }

        // ── 2.5 ターン完了後にセッションを保存（Ctrl+C やクラッシュに備える）──
        session_store.save(root, user_task, total_turns, &done_log, &read_files);

        // ── 3. 結果を表示 ─────────────────────────────────────────────────
        for r in &tool_results {
            display_result(r, verbose);
        }
        if verbose {
            if commands
                .iter()
                .filter(|c| matches!(c, AiCommand::ReadFile { .. }))
                .count()
                > 1
            {
                println!("  {DIM}複数ファイル読み込みのため複数ターンを使用します{RESET}");
            }
        }

        // ── 4. 次のターンの状態を決定（明示的な Continuation パターン）────
        let ctx = build_context_header(user_task, &read_files, root, &done_log);
        let outcome = determine_outcome(
            user_task,
            bot_message.as_deref(),
            is_done,
            only_txt,
            &tool_results,
            turn,
            &ctx,
            consecutive_txt,
            &read_files,
            root,
            has_successful_file_update,
            consecutive_read_file,
            consecutive_ask_user,
            consecutive_edit_fail,
            &last_failed_edit_path,
        );

        dbg.turn_end();

        match outcome {
            TurnOutcome::Done => break 'agent,

            TurnOutcome::Continue { prompt: next } => {
                consecutive_txt = 0;
                prompt = next;
            }

            TurnOutcome::NudgeForJson { prompt: next } => {
                consecutive_txt += 1;
                prompt = next;
            }

            TurnOutcome::NoCommands => break 'agent,

            TurnOutcome::MaxTurns => {
                reached_max = true;
                break 'agent;
            }
        }
    }

    print_summary(&done_log, reached_max);

    // 正常完了（bot コマンドで終了）のときだけセッションをクリア
    // 中断（MAX_TURNS 到達）のときは次回続きから再開できるよう保持
    if !reached_max {
        session_store.clear();
    }

    Ok(!reached_max)
}

// ─── ヘルパー ─────────────────────────────────────────────────────────────────

fn display_result(r: &ToolResult, verbose: bool) {
    if r.label == "ParseError" {
        // verbose/非verbose 問わず常に表示。出力内容はモードで調整
        if verbose {
            // 最初の行（エラー概要）だけ抜き出して表示
            let summary = r
                .output
                .lines()
                .next()
                .unwrap_or("JSON パース失敗")
                .chars()
                .take(120)
                .collect::<String>();
            println!(
                "  {RED_BOLD}[ParseError]{RESET} {summary} {DIM}(詳細は .copipe_logs/browser_log){RESET}"
            );
        } else {
            println!("  {RED_BOLD}[ParseError]{RESET} {DIM}JSON パース失敗、リトライします{RESET}");
        }
    } else if is_error_output(&r.output) {
        // 種別ごとに色分け（llm-prompts.md §3）
        // Blocked by hook → YELLOW（ブロックは情報的）
        // Permission denied / Tool error / ERROR → RED_BOLD
        if r.output.starts_with("Blocked by hook:") {
            println!("  {YELLOW}[{}]{RESET} {}", r.label, r.output);
        } else {
            println!("  {RED_BOLD}[{}]{RESET} {}", r.label, r.output);
        }
    } else if verbose {
        let preview: String = r.output.lines().take(15).collect::<Vec<_>>().join("\n");
        let suffix = if r.output.lines().count() > 15 {
            "\n  …"
        } else {
            ""
        };
        println!("  {GREEN}✓{RESET} {}{}", r.label, "");
        println!("{}{}", preview, suffix);
    } else {
        println!(
            "  {GREEN}✓{RESET} {} {DIM}{}{RESET}",
            r.label,
            summarize_for_display(&r.label, &r.output)
        );
    }
}

fn print_summary(done_log: &[String], reached_max: bool) {
    if !done_log.is_empty() {
        let header = if reached_max {
            "── 実行サマリー（中断）"
        } else {
            "── 実行サマリー"
        };
        println!("\n{BOLD}{header}{RESET}");
        for item in done_log {
            if item.starts_with('✓') {
                println!("  {GREEN}{item}{RESET}");
            } else {
                let display = if let Some(arrow) = item.find(" → ") {
                    let sep_len = " → ".len(); // " "(1) + "→"(3bytes) + " "(1) = 5
                    let reason = &item[arrow + sep_len..];
                    let first_line: String = reason
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(80)
                        .collect();
                    let suffix = if first_line.chars().count() < reason.chars().count() {
                        "…"
                    } else {
                        ""
                    };
                    format!("{}{suffix}", &item[..arrow + sep_len + first_line.len()])
                } else {
                    item.clone()
                };
                println!("  {RED}{display}{RESET}");
            }
        }
    }

    if reached_max {
        println!("\n{YELLOW}最大ターン数 ({MAX_TURNS}) に達しました。{RESET}");
        println!(
            "{DIM}→ 同じタスクを再入力すると続きから再開できます（セッションを保存済み）。{RESET}"
        );
        println!(
            "{DIM}  タスクが大きい場合は「〇〇だけ修正して」のように絞り込むと効率的です。{RESET}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // task.rs に移動したタスク分類関数をテストスコープへ
    use super::super::task::*;

    #[test]
    fn non_actionable_ack_is_detected() {
        assert!(is_non_actionable_ack("おねがい"));
        assert!(is_non_actionable_ack("お願い！"));
        assert!(is_non_actionable_ack("続き"));
        assert!(is_non_actionable_ack("つづき。"));
        assert!(is_non_actionable_ack("続けて"));
        assert!(is_non_actionable_ack("つづけて。"));
        assert!(is_non_actionable_ack("continue"));
        assert!(is_non_actionable_ack("OK"));
        assert!(is_non_actionable_ack(" yes "));
    }

    #[test]
    fn menu_selection_without_context_is_detected() {
        assert!(is_menu_selection_without_context("1"));
        assert!(is_menu_selection_without_context("1."));
        assert!(is_menu_selection_without_context("1. codegen"));
        assert!(is_menu_selection_without_context("2 parser"));
    }

    #[test]
    fn concrete_task_is_not_treated_as_ack() {
        assert!(!is_non_actionable_ack("src の構成を調べて"));
        assert!(!is_non_actionable_ack("cargo check して修正して"));
        assert!(!is_non_actionable_ack(
            "前回のセッションを resume して続けて"
        ));
        assert!(!is_menu_selection_without_context(
            "1. src/main.rs を修正して"
        ));
        assert!(!is_menu_selection_without_context("2 parser を実装して"));
    }

    #[test]
    fn initial_prompt_adds_review_efficiency_hints() {
        let prompt = build_initial_prompt("src/agent/debug_log.rs をレビューして");

        assert!(prompt.contains("## New task boundary"));
        assert!(prompt.contains("空 grep を挟まず直接 read_file"));
        assert!(prompt.contains("具体的な指摘・根拠・改善案"));
    }

    #[test]
    fn initial_prompt_adds_new_task_boundary_to_non_review_tasks() {
        let task = "cargo check して";
        let prompt = build_initial_prompt(task);

        assert!(prompt.contains("## New task boundary"));
        assert!(prompt.contains("過去の Copilot チャット文脈"));
        assert!(prompt.contains("ログは失敗パターンの診断材料"));
        assert!(prompt.contains("ログ内の作業対象・crate 名・ファイル名・実行結果"));
        assert!(prompt.contains(task));
        assert!(prompt.contains("```json コードブロック"));
    }

    #[test]
    fn initial_prompt_bootstraps_short_open_ended_development_tasks() {
        let prompt = build_initial_prompt("IRの拡張");

        assert!(prompt.contains("短い開発依頼への進め方"));
        assert!(prompt.contains("\"type\":\"txt\""));
        assert!(prompt.contains("\"type\":\"glob\""));
        assert!(prompt.contains("\"pattern\":\"src/**/*.rs\""));
        assert!(prompt.contains("具体化質問やプレーンテキスト回答で止めない"));
        assert!(prompt.contains("最小で保守的な改善を1つ選んで実装"));
    }

    #[test]
    fn short_open_ended_development_task_detection_is_scoped() {
        assert!(is_short_open_ended_development_task("IRの拡張"));
        assert!(is_short_open_ended_development_task("UI改善"));
        assert!(!is_short_open_ended_development_task("cargo check して"));
        assert!(!is_short_open_ended_development_task(
            "src/agent/runner.rs を修正"
        ));
    }

    #[test]
    fn development_task_deferral_is_nudged_to_act() {
        let dir = tempfile::tempdir().unwrap();
        let message = "codegen.rs の内容を確認しました。IR 拡張の次の具体的アクションを提案できますので、どの方向に拡張したいか指示してください。";

        let outcome = determine_outcome(
            "IRの拡張",
            Some(message),
            true,
            false,
            &[],
            0,
            "CTX",
            0,
            &HashSet::new(),
            dir.path(),
            false,
            0,
            0,
            0,
            "",
        );

        match outcome {
            TurnOutcome::NudgeForJson { prompt } => {
                assert!(prompt.contains("開発タスクを質問だけで終了"));
                assert!(prompt.contains("ログ内のファイル名や crate 名"));
            }
            other => panic!("expected nudge, got {other:?}"),
        }
    }

    #[test]
    fn completed_development_message_is_not_treated_as_deferral() {
        let message = "src/agent/runner.rs を修正しました。cargo check も成功しています。";

        assert!(!is_deferring_development_message(message));
    }

    #[test]
    fn final_answer_referenced_missing_paths_are_detected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let message =
            "確認しました。src/main.rs はありますが、src/parser.rs と src/ast.rs も実装済みです。";

        let missing = missing_referenced_project_paths(message, dir.path());

        assert_eq!(missing, vec!["src/parser.rs", "src/ast.rs"]);
    }

    #[test]
    fn final_answer_existing_paths_are_not_flagged() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let missing =
            missing_referenced_project_paths("`src/main.rs` を確認しました。", dir.path());

        assert!(missing.is_empty());
    }

    #[test]
    fn initial_prompt_for_ack_asks_for_concrete_task_without_tools() {
        let prompt = build_initial_prompt("おねがい");

        assert!(prompt.contains("具体的な作業内容がありません"));
        assert!(prompt.contains("ツールを実行せず"));
    }

    #[test]
    fn initial_prompt_for_menu_selection_asks_for_concrete_task_without_tools() {
        let prompt = build_initial_prompt("1. codegen");

        assert!(prompt.contains("具体的な作業内容がありません"));
        assert!(prompt.contains("過去の Copilot チャット文脈"));
        assert!(prompt.contains("ツールを実行せず"));
    }

    #[test]
    fn non_actionable_ack_is_short_circuited_without_resume() {
        assert!(should_short_circuit_non_actionable_task("続けて", None));
        assert!(should_short_circuit_non_actionable_task("続き", None));
        assert!(should_short_circuit_non_actionable_task("1. codegen", None));
    }

    #[test]
    fn non_actionable_ack_can_continue_when_resume_exists() {
        let data = SessionData {
            version: 1,
            project_dir: ".".to_string(),
            user_task: "続けて".to_string(),
            turn_count: 1,
            saved_at: "now".to_string(),
            done_log: Vec::new(),
            read_files: Vec::new(),
        };

        assert!(!should_short_circuit_non_actionable_task(
            "続けて",
            Some(&data)
        ));
    }

    #[test]
    fn read_status_question_is_short_circuited_without_resume() {
        assert!(is_read_status_question("小説全部読みましたか？"));
        assert!(is_read_status_question("小説.md をすべて読んだか確認"));
        assert!(is_read_status_question("README.md を読み込んだか確認"));
        assert!(is_read_status_question("README.md は読み込み済み？"));
        assert!(should_short_circuit_read_status_task(
            "小説全部読みましたか？",
            None
        ));
    }

    #[test]
    fn read_request_is_not_treated_as_status_question() {
        assert!(!is_read_status_question("小説.md を読んで要約して"));
        assert!(!is_read_status_question("src/main.rs を確認して"));
        assert!(!should_short_circuit_read_status_task(
            "小説.md を読んで要約して",
            None
        ));
    }

    #[test]
    fn read_status_question_can_continue_when_resume_exists() {
        let data = SessionData {
            version: 1,
            project_dir: ".".to_string(),
            user_task: "小説.md を読んで".to_string(),
            turn_count: 1,
            saved_at: "now".to_string(),
            done_log: vec!["✓ ReadFile(小説.md)".to_string()],
            read_files: vec!["小説.md".to_string()],
        };

        assert!(!should_short_circuit_read_status_task(
            "小説全部読みましたか？",
            Some(&data)
        ));
    }

    #[test]
    fn recovery_hint_warns_after_missing_read_file() {
        let results = vec![ToolResult::new(
            "ReadFile(src/parser.rs)",
            "ERROR: ファイルが存在しません: 'src/parser.rs'. 同ディレクトリの実在ファイル: main.rs",
        )];

        let hint = recovery_hint_for_tool_results(&results);

        assert!(hint.contains("存在しないファイルを読もうとしました"));
        assert!(hint.contains("src/parser.rs"));
        assert!(hint.contains("推測して回答しないでください"));
        assert!(hint.contains("実在するファイルだけを根拠"));
    }

    #[test]
    fn recovery_hint_suggests_cargo_check_tests_after_blocked_cargo_test() {
        let results = vec![ToolResult::new(
            "Cmd(run tests)",
            "Permission denied: 'cargo test' はビルドスクリプト/proc macro/バイナリ経由で任意コードを実行できるため禁止です",
        )];

        let hint = recovery_hint_for_tool_results(&results);

        assert!(hint.contains("cargo test は実行できません"));
        assert!(hint.contains("[\"cargo\",\"check\",\"--tests\"]"));
        assert!(hint.contains("実際のテスト実行が必須"));
    }

    #[test]
    fn recovery_hint_suggests_mkdir_after_missing_parent_dir() {
        let results = vec![ToolResult::new(
            "WriteFile(.github/workflows/rust-ci.yml)",
            "ERROR: 親ディレクトリが存在しません。先に mkdir で作成し、必要なら list_dir/glob で配置を確認してください: .github/workflows/rust-ci.yml",
        )];

        let hint = recovery_hint_for_tool_results(&results);

        assert!(hint.contains("親ディレクトリがありません"));
        assert!(hint.contains("mkdir"));
        assert!(hint.contains(".github/workflows"));
    }

    #[test]
    fn consecutive_read_file_3_triggers_nudge() {
        let dir = tempfile::tempdir().unwrap();
        // 3件連続 read_file → NudgeForJson が発動する
        let outcome = determine_outcome(
            "調査",
            None,
            false,
            false,
            &[ToolResult::new("ReadFile(src/c.rs)", "content")],
            1,
            "CTX",
            0,
            &HashSet::new(),
            dir.path(),
            false,
            3, // consecutive_read_file
            0,
            0,
            "",
        );
        match outcome {
            TurnOutcome::NudgeForJson { prompt } => {
                assert!(prompt.contains("連続 read_file が 3 件に達しました"));
                assert!(prompt.contains("grep"));
            }
            other => panic!("expected NudgeForJson, got {other:?}"),
        }
    }

    #[test]
    fn consecutive_read_file_2_does_not_trigger_nudge() {
        let dir = tempfile::tempdir().unwrap();
        // 2件では NudgeForJson は発動しない（Continue になる）
        let outcome = determine_outcome(
            "調査",
            None,
            false,
            false,
            &[ToolResult::new("ReadFile(src/b.rs)", "content")],
            1,
            "CTX",
            0,
            &HashSet::new(),
            dir.path(),
            false,
            2, // consecutive_read_file
            0,
            0,
            "",
        );
        assert!(
            matches!(outcome, TurnOutcome::Continue { .. }),
            "2件では nudge しないはず: {outcome:?}"
        );
    }
}
