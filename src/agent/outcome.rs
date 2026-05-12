/// ターン結果の状態遷移ロジック（runner.rs から切り出したピュア関数群）
///
/// `determine_outcome` と `apply_stop_hooks` を独立モジュールに分離することで
/// runner.rs のサイズを縮小し、単体テストを書きやすくする。
use super::task::{
    is_deferring_development_message, is_incomplete_handoff_message,
    is_placeholder_bot_message, is_placeholder_review_message,
    missing_referenced_project_paths, task_requires_development_action,
    task_requires_file_update, task_requires_review_output,
};
use crate::executor::errors::is_error_output;
use crate::executor::tools::todo_write;
use crate::executor::ToolResult;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

// ─── ターン結果型 ─────────────────────────────────────────────────────────────

/// 1ターンの処理結果を表す明示的な状態
#[derive(Debug)]
pub(super) enum TurnOutcome {
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

pub(super) fn turn_outcome_name(outcome: &TurnOutcome) -> &'static str {
    match outcome {
        TurnOutcome::Done => "done",
        TurnOutcome::Continue { .. } => "continue",
        TurnOutcome::NudgeForJson { .. } => "nudge_for_json",
        TurnOutcome::NoCommands => "no_commands",
        TurnOutcome::MaxTurns => "max_turns",
    }
}

// ─── ターン入力状態 ───────────────────────────────────────────────────────────

/// ターン終了時の評価に必要な入力状態（core-internals.md §「run()の1ターン」参照）
pub(super) struct TurnState<'a> {
    pub(super) user_task: &'a str,
    pub(super) bot_message: Option<&'a str>,
    pub(super) is_done: bool,
    pub(super) only_txt: bool,
    pub(super) tool_results: &'a [ToolResult],
    pub(super) turn: u32,
    pub(super) ctx: &'a str,
    pub(super) consecutive_txt: u32,
    pub(super) read_files: &'a HashSet<PathBuf>,
    pub(super) done_log: &'a [String],
    pub(super) root: &'a Path,
    pub(super) has_successful_file_update: bool,
    pub(super) consecutive_read_file: u32,
    pub(super) consecutive_ask_user: u32,
    pub(super) consecutive_edit_fail: u32,
    pub(super) last_failed_edit_path: &'a str,
    pub(super) consecutive_empty_grep: u32,
}

// ─── ストップフック ───────────────────────────────────────────────────────────

/// SPEC core-internals.md §「hooks.runStop()」相当:
/// `TurnOutcome::Done` が返った際に完了条件を検証し、条件未達なら `NudgeForJson` に変換する。
/// Done 以外の outcome はそのまま返す（フックは Done のときだけ評価する）。
pub(super) fn apply_stop_hooks(
    outcome: TurnOutcome,
    user_task: &str,
    ctx: &str,
    done_log: &[String],
    is_rust_project: bool,
    has_successful_file_update: bool,
    root: &Path,
) -> TurnOutcome {
    if !matches!(outcome, TurnOutcome::Done) {
        return outcome;
    }

    // フック 1: 未完了 todo があれば続ける
    let todos = todo_write::load(root);
    let unfinished_todos = todo_write::unfinished(&todos);
    if !unfinished_todos.is_empty() {
        return TurnOutcome::NudgeForJson {
            prompt: format!(
                "{ctx}\n\n\
                [⚠ todo_write に未完了タスクが残っています]\n\
                完了報告の前に、TODO.JSON の未完了タスクを実行してください。\
                `read_log todo` だけで止まらず、in_progress があればそのタスクを実行し、\
                完了したら `todo_write` で completed に更新してください。\n\n\
                [現在の TODO.JSON]\n{}\n\n\
                ```json\n[{{\"type\":\"txt\",\"content\":\"TODO.JSON の in_progress タスクを実行します\"}},{{\"type\":\"read_log\",\"filename\":\"todo\"}}]\n```",
                todo_write::format_todos_plain(&todos)
            ),
        };
    }

    // フック 2: Rust プロジェクトでファイル編集後のコンパイル確認
    if is_rust_project && has_successful_file_update && task_requires_development_action(user_task)
    {
        let has_cmd_verification = done_log.iter().any(|e| e.contains(" Cmd("));
        if !has_cmd_verification {
            return TurnOutcome::NudgeForJson {
                prompt: format!(
                    "{ctx}\n\n\
                    [⚠ ファイルを変更しましたが、コンパイル確認がまだ実行されていません]\n\
                    Rust プロジェクトの変更後は `cargo check` でコンパイルを確認してから完了報告してください。\n\
                    ```json\n\
                    [{{\"type\":\"txt\",\"content\":\"変更後のコンパイルを確認します\"}},\
                    {{\"type\":\"cmd\",\"name\":\"コンパイル確認\",\"cmd\":[\"cargo\",\"check\"],\"workdir\":\".\",\"timeout\":60}}]\n\
                    ```"
                ),
            };
        }
    }

    outcome
}

// ─── ターン結果決定 ───────────────────────────────────────────────────────────

/// ターン終了時の状態を判定して TurnOutcome を返す
pub(super) fn determine_outcome(state: &TurnState<'_>) -> TurnOutcome {
    let user_task = state.user_task;
    let bot_message = state.bot_message;
    let is_done = state.is_done;
    let only_txt = state.only_txt;
    let tool_results = state.tool_results;
    let turn = state.turn;
    let ctx = state.ctx;
    let consecutive_txt = state.consecutive_txt;
    let read_files = state.read_files;
    let done_log = state.done_log;
    let root = state.root;
    let has_successful_file_update = state.has_successful_file_update;
    let consecutive_read_file = state.consecutive_read_file;
    let consecutive_ask_user = state.consecutive_ask_user;
    let consecutive_edit_fail = state.consecutive_edit_fail;
    let last_failed_edit_path = state.last_failed_edit_path;
    let consecutive_empty_grep = state.consecutive_empty_grep;

    // 連続空 grep（2回以上）→ list_dir / glob への誘導
    if consecutive_empty_grep >= 2 && !is_done {
        return TurnOutcome::NudgeForJson {
            prompt: format!(
                "{ctx}\n\n\
                [⚠ {consecutive_empty_grep} 回連続で grep 結果が空です]\n\
                異なるキーワードで grep を繰り返しても結果が得られていません。\
                プロジェクトが空か最小構成の可能性があります。\n\
                次のいずれかを実行してください:\n\
                1. `list_dir` でディレクトリ内の実在ファイルを確認する\n\
                2. 判明済みのファイルを `read_file` で直接読む\n\
                3. プロジェクトが空または最小構成なら、必要なファイルを `file` コマンドで直接作成する\n\
                ```json\n{{\"type\":\"list_dir\",\"path\":\"src\"}}\n```"
            ),
        };
    }

    if is_done && bot_message.map(is_placeholder_bot_message).unwrap_or(true) {
        return TurnOutcome::NudgeForJson {
            prompt: format!(
                "{ctx}\n\n\
                [⚠ 最終回答がテンプレートの仮文です]\n\
                `（完全な回答）` のようなプレースホルダで完了にしないでください。\
                ユーザーにそのまま見せられる具体的な完了報告、または必要な次アクションを `bot` の message に入れてください。\n\
                ```json\n{{\"type\":\"bot\",\"message\":\"（ここに具体的な回答本文を省略せず書く）\"}}\n```"
            ),
        };
    }

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
            .map(|m| is_placeholder_review_message(m))
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

    if is_done
        && task_requires_development_action(user_task)
        && has_successful_file_update
        && bot_message
            .map(is_incomplete_handoff_message)
            .unwrap_or(false)
    {
        return TurnOutcome::NudgeForJson {
            prompt: format!(
                "{ctx}\n\n\
                [⚠ 作業後のつなぎが未完了メニューになっています]\n\
                ファイル変更後に「次の選択肢を選んでください」で終了しないでください。\
                ユーザーの依頼と現在のツール結果から明らかな残作業があるなら、ask_user ではなく続けて実行してください。\
                残作業が本質的に判断不能なら、選択式メニューではなく、実施済み内容・未実施内容・次に必要な具体情報を短く `bot` で報告してください。\n\
                ```json\n{{\"type\":\"bot\",\"message\":\"（実施済み内容と、未実施があればその理由を簡潔に報告）\"}}\n```"
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
                \n候補メニューや「選んでください」で止めず、ツール結果と依頼文から保守的な次アクションを1つ選んで実行してください。\
                \n本質的に作業不能な情報だけが欠けている場合に限り、`bot` で不足している具体情報を1つだけ短く報告してください。\
                \n```json\
                \n[{{\"type\":\"txt\",\"content\":\"追加質問せず、手元の情報から次の作業を進めます\"}},{{\"type\":\"glob\",\"pattern\":\"src/**/*.rs\"}}]\
                \n```"
            ),
        };
    }

    // edit / multi_edit / patch の連続失敗（2回以上）検知 → 再読み込みを強制
    if consecutive_edit_fail >= 2 && !is_done && !last_failed_edit_path.is_empty() {
        return TurnOutcome::NudgeForJson {
            prompt: format!(
                "{ctx}\n\n\
                [⚠ `{last_failed_edit_path}` への edit / patch が {consecutive_edit_fail} 回連続で失敗しています]\n\
                `patch` が失敗している場合: diff の行数宣言ずれが原因です。`patch` の代わりに `multi_edit` を使うと確実に置換できます。\n\
                `edit` / `multi_edit` が失敗している場合: old_string が現在のファイル内容と一致していません。余分な空白・改行が含まれている可能性があります。\n\
                改めて read_file でファイルの実際の内容を確認し、`multi_edit` で正確に修正してください。\n\
                ```json\n{{\"type\":\"read_file\",\"path\":\"{last_failed_edit_path}\"}}\n```"
            ),
        };
    }

    // 書き込み成功後に read_file で再確認するだけの無駄ループ検知
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

    if consecutive_read_file >= 3
        && !is_done
        && unread_guard_recovery_read_paths(done_log, tool_results)
            .is_some_and(|paths| !paths.is_empty())
    {
        return TurnOutcome::NudgeForJson {
            prompt: format!(
                "{ctx}\n\n\
                [未読ガード回復の read_file が完了しています]\n\
                直前に未読として拒否された書き込み対象を読み終えています。\
                この read_file 群は安全確認のために必要なものなので、grep や追加調査へ逸れず、\
                失敗した file / edit / patch コマンドを現在の内容に合わせて再試行してください。\n\
                ```json\n{{\"type\":\"file\",\"path\":\"対象ファイル\",\"content\":\"（読み込み済み内容を踏まえた更新後の全文）\"}}\n```"
            ),
        };
    }

    // read_file 連打（3件以上）で grep/bot への誘導
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

    if turn + 1 >= crate::agent::runner::MAX_TURNS {
        return TurnOutcome::MaxTurns;
    }

    // コンパイラの help: 提案がある場合は追加調査なしで直接修正するよう促す
    let compiler_hint = if tool_results.iter().any(|r| {
        r.label.starts_with("Cmd(")
            && is_error_output(&r.output)
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

    TurnOutcome::Continue {
        prompt: format!(
            "{ctx}\n\n## Tool results\n{}{}{}",
            crate::executor::format_tool_results(tool_results),
            compiler_hint,
            recovery_hint
        ),
    }
}

// ─── ヘルパー関数 ─────────────────────────────────────────────────────────────

pub(super) fn recovery_hint_for_tool_results(tool_results: &[ToolResult]) -> String {
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

    let unread_guard_paths: Vec<String> = tool_results
        .iter()
        .filter(|r| r.output.contains("このタスク内で未読です"))
        .filter_map(|r| label_inner(&r.label).map(str::to_string))
        .collect();
    let unread_guard_hint = if unread_guard_paths.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n[未読ファイルへの書き込みが拒否されました]\n\
            対象パス: {}\n\
            これは安全確認のための拒否です。対象ファイルを同じ JSON 配列内でまとめて `read_file` し、\
            読み終えたら grep や追加質問に逸れず、同じ書き込みを現在の内容に合わせて再試行してください。",
            unread_guard_paths.join(", ")
        )
    };

    let grep_results: Vec<&ToolResult> = tool_results
        .iter()
        .filter(|r| r.label.starts_with("Grep("))
        .collect();
    let empty_grep_hint = if grep_results.len() >= 2
        && grep_results.iter().all(|r| r.output.contains("マッチなし"))
    {
        "\n\n[同一ターン内の全 grep 結果が空です]\n\
        複数のキーワードを同時に検索しても全てマッチなしでした。\
        プロジェクトが空か最小構成の可能性があります。\
        `list_dir` でディレクトリ構成を確認するか、判明済みのファイルを直接 `read_file` してください。"
            .to_string()
    } else {
        String::new()
    };

    let patch_fail_hint = if tool_results
        .iter()
        .any(|r| r.label.starts_with("Patch(") && is_error_output(&r.output))
    {
        "\n\n[patch が失敗しました]\n\
        diff の行数宣言とハンク内容が一致しないか、コンテキスト行がファイル内に見つかりません。\n\
        `patch` の代わりに `multi_edit` を使うと確実に置換できます。\
        read_file でファイル内容を確認してから old_string / new_string を指定してください:\n\
        ```json\n\
        {\"type\":\"multi_edit\",\"path\":\"対象ファイル\",\"edits\":[{\"old_string\":\"変更前の行\",\"new_string\":\"変更後の行\"}]}\n\
        ```"
            .to_string()
    } else {
        String::new()
    };

    let truncated_cmd_hint = if tool_results.iter().any(|r| {
        r.label.starts_with("Cmd(")
            && r.output.contains("出力が")
            && r.output.contains("文字を超えたため省略しました")
    }) {
        "\n\n[Cmd の出力が省略されています]\n\
        エラーの全体像を把握するために、まず次の手順で全出力を確認してください:\n\
        1. `read_log` で `filename: \"cmd_log\"` を指定して全エラーを読む\n\
        2. 全エラーを確認してから `edit` / `multi_edit` で修正する\n\
        省略されたまま修正すると見落としが生じます。必ず read_log を先に実行してください。"
            .to_string()
    } else {
        String::new()
    };

    format!(
        "{missing_path_hint}{blocked_cargo_hint}{parent_dir_hint}{unread_guard_hint}{empty_grep_hint}{patch_fail_hint}{truncated_cmd_hint}"
    )
}

fn label_inner(label: &str) -> Option<&str> {
    label
        .find('(')
        .and_then(|start| label.strip_suffix(')').map(|s| &s[start + 1..]))
}

pub(super) fn unread_guard_recovery_read_paths(
    done_log: &[String],
    tool_results: &[ToolResult],
) -> Option<Vec<String>> {
    if tool_results.is_empty()
        || !tool_results
            .iter()
            .all(|r| r.label.starts_with("ReadFile(") && !is_error_output(&r.output))
    {
        return None;
    }

    let current_reads: HashSet<String> = tool_results
        .iter()
        .filter_map(|r| label_inner(&r.label).map(str::to_string))
        .collect();
    if current_reads.is_empty() {
        return None;
    }

    let mut pending = HashSet::new();
    let mut ready = HashSet::new();
    for entry in done_log {
        if entry.starts_with("✗ ")
            && entry.contains("このタスク内で未読です")
            && let Some(path) = done_log_label_inner(entry)
        {
            pending.insert(path.to_string());
        } else if entry.starts_with("✓ ReadFile(")
            && let Some(path) = done_log_label_inner(entry)
            && pending.contains(path)
        {
            ready.insert(path.to_string());
        } else if is_successful_write_done_log(entry)
            && let Some(path) = done_log_label_inner(entry)
        {
            pending.remove(path);
            ready.remove(path);
        }
    }

    let mut paths: Vec<String> = ready.intersection(&current_reads).cloned().collect();
    paths.sort();
    Some(paths)
}

fn done_log_label_inner(entry: &str) -> Option<&str> {
    let start = entry.find('(')?;
    let rest = &entry[start + 1..];
    let end = rest.find(')')?;
    Some(&rest[..end])
}

fn is_successful_write_done_log(entry: &str) -> bool {
    entry.starts_with("✓ WriteFile(")
        || entry.starts_with("✓ Edit(")
        || entry.starts_with("✓ MultiEdit(")
        || entry.starts_with("✓ Patch(")
}

pub(super) fn read_file_hint(read_files: &HashSet<PathBuf>, root: &Path) -> String {
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
