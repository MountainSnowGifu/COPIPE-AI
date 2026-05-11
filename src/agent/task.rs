/// タスク分類・内容判定ユーティリティ（core-internals.md §「agent-loop / run()」参考）
///
/// runner.rs のメインループから切り出したピュア文字列解析関数群。
/// 副作用なし・I/O なしのため独立してテスト可能。
use super::session_store::SessionData;
use std::path::Path;

// ─── 初期プロンプト分類 ────────────────────────────────────────────────────────

/// 依頼文に既知の拡張子を持つファイル名が明示されているか判定する。
///
/// ファイル名が明示されている場合、AI は glob を省略して直接 read_file へ進める。
pub(super) fn task_mentions_explicit_filename(task: &str) -> bool {
    const KNOWN_EXTS: &[&str] = &[
        "md", "txt", "rs", "toml", "json", "yaml", "yml", "py", "js", "ts", "c", "cpp", "h",
        "html", "css", "sh",
    ];
    task.split_whitespace().any(|word| {
        let w = word.trim_matches(|c: char| "「」。、！？()[]{}\"'".contains(c));
        if let Some((_, ext)) = w.rsplit_once('.') {
            !ext.is_empty() && KNOWN_EXTS.contains(&ext)
        } else {
            false
        }
    })
}

/// 24文字以下の抽象的な開発依頼（例: "IRの拡張", "UI改善"）を検出する。
pub(super) fn is_short_open_ended_development_task(task: &str) -> bool {
    let trimmed = task.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 24 {
        return false;
    }

    if trimmed.contains(".rs")
        || trimmed.contains(".md")
        || trimmed.contains(".txt")
        || trimmed.to_ascii_lowercase().contains("cargo")
    {
        return false;
    }

    let development_markers = [
        "実装", "追加", "拡張", "改善", "修正", "対応", "作成", "リファクタ", "IR", "API", "UI",
    ];
    development_markers
        .iter()
        .any(|marker| trimmed.contains(marker))
}

/// 具体的な作業内容を持たない相づち（例: "おねがい", "続き", "OK"）を検出する。
pub(super) fn is_non_actionable_ack(task: &str) -> bool {
    let normalized = task
        .trim()
        .trim_matches(|c: char| {
            c.is_ascii_punctuation()
                || c.is_whitespace()
                || matches!(c, '。' | '、' | '！' | '？' | '!' | '?')
        })
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "お願い"
            | "おねがい"
            | "頼む"
            | "よろしく"
            | "続き"
            | "つづき"
            | "続けて"
            | "つづけて"
            | "はい"
            | "うん"
            | "ok"
            | "okay"
            | "continue"
            | "yes"
            | "y"
            | "go"
    )
}

/// 数字のみ、または "1. codegen" 形式のメニュー選択（文脈なし）を検出する。
pub(super) fn is_menu_selection_without_context(task: &str) -> bool {
    let normalized = task.trim().trim_matches(|c: char| {
        c.is_ascii_punctuation()
            || c.is_whitespace()
            || matches!(c, '。' | '、' | '！' | '？' | '!' | '?' | '．')
    });

    if normalized.chars().all(|c| c.is_ascii_digit()) {
        return !normalized.is_empty();
    }

    let mut parts = normalized.split_whitespace();
    let Some(first) = parts.next() else {
        return false;
    };
    let rest = parts.collect::<Vec<_>>().join(" ");
    let first =
        first.trim_end_matches(|c: char| c.is_ascii_punctuation() || matches!(c, '。' | '、' | '．'));

    first.chars().all(|c| c.is_ascii_digit())
        && !rest.is_empty()
        && rest.chars().count() <= 32
        && !rest.contains("して")
        && !rest.contains("修正")
        && !rest.contains("実装")
        && !rest.contains("追加")
        && !rest.contains("調査")
        && !rest.contains("レビュー")
        && !rest.to_ascii_lowercase().contains("fix")
        && !rest.to_ascii_lowercase().contains("implement")
}

/// resume なしの相づち/メニュー選択はショートサーキットする。
pub(super) fn should_short_circuit_non_actionable_task(
    task: &str,
    resume: Option<&SessionData>,
) -> bool {
    resume.is_none() && (is_non_actionable_ack(task) || is_menu_selection_without_context(task))
}

/// "読みましたか？" のような既読確認質問を検出する（実際の読み込み作業とは区別）。
pub(super) fn is_read_status_question(task: &str) -> bool {
    let trimmed = task.trim();
    if trimmed.is_empty() {
        return false;
    }

    let asks_status = trimmed.contains("読みましたか")
        || trimmed.contains("読んだか")
        || trimmed.contains("読んだ?")
        || trimmed.contains("読んだ？")
        || trimmed.contains("読んでますか")
        || trimmed.contains("読み込みましたか")
        || trimmed.contains("読み込んだか")
        || trimmed.contains("読み込んだ?")
        || trimmed.contains("読み込んだ？")
        || trimmed.contains("読み込み済み")
        || trimmed.contains("既読");

    let asks_to_read_now = trimmed.contains("読んで")
        || trimmed.contains("読み込んで")
        || trimmed.contains("要約")
        || trimmed.contains("レビュー")
        || trimmed.contains("調査")
        || trimmed.contains("確認して");

    asks_status && !asks_to_read_now
}

/// resume なしの既読確認質問はショートサーキットする。
pub(super) fn should_short_circuit_read_status_task(
    task: &str,
    resume: Option<&SessionData>,
) -> bool {
    resume.is_none() && is_read_status_question(task)
}

// ─── ターン中判定 ─────────────────────────────────────────────────────────────

/// ファイル内容の書き換え・翻訳タスクか判定する。
pub(super) fn task_requires_file_update(task: &str) -> bool {
    let has_file_like_target = task.contains(".txt")
        || task.contains(".md")
        || task.contains(".rs")
        || task.contains("ファイル");
    let asks_update = task.contains("書き換")
        || task.contains("書換")
        || task.contains("翻訳")
        || task.contains("英語")
        || task.contains("日本語")
        || task.contains("更新")
        || task.contains("修正")
        || task.contains("変換");
    has_file_like_target && asks_update
}

/// 実装・追加・修正など実際の開発アクションを要求するタスクか判定する。
pub(super) fn task_requires_development_action(task: &str) -> bool {
    let lower = task.to_ascii_lowercase();
    task.contains("実装")
        || task.contains("追加")
        || task.contains("拡張")
        || task.contains("改善")
        || task.contains("修正")
        || task.contains("対応")
        || task.contains("作成")
        || task.contains("リファクタ")
        || lower.contains("implement")
        || lower.contains("add")
        || lower.contains("improve")
        || lower.contains("fix")
        || lower.contains("refactor")
}

/// レビュー・調査・分析系の出力を求めるタスクか判定する。
pub(super) fn task_requires_review_output(task: &str) -> bool {
    let lower = task.to_ascii_lowercase();
    let explicit_review = task.contains("レビュー")
        || lower.contains("review")
        || task.contains("総括")
        || task.contains("問題点")
        || task.contains("調査")
        || task.contains("分析")
        || task.contains("チェック")
        || task.contains("調べ")
        || lower.contains("analyz")
        || lower.contains("inspect");

    if lower.contains("cargo check") && !explicit_review {
        return false;
    }

    explicit_review || lower.contains("check")
}

// ─── ファイナルアンサー検証 ───────────────────────────────────────────────────

/// 開発タスクを質問で先延ばしにしているメッセージを検出する。
pub(super) fn is_deferring_development_message(message: &str) -> bool {
    let m = message.trim();
    if m.chars().count() < 40 {
        return false;
    }

    let asks_user_to_choose = [
        "指示してください",
        "教えてください",
        "選んでください",
        "どの方向",
        "どれを",
        "必要であれば",
        "続けて行えます",
        "次に何を",
        "具体的に",
    ]
    .iter()
    .any(|p| m.contains(*p));

    let mentions_work_without_doing = [
        "提案できます",
        "候補",
        "次のステップ",
        "改善案",
        "方向",
        "実装できます",
    ]
    .iter()
    .any(|p| m.contains(*p));

    let reports_finished_work = [
        "実装しました",
        "修正しました",
        "追加しました",
        "更新しました",
        "cargo check",
        "ビルド",
        "テスト",
    ]
    .iter()
    .any(|p| m.contains(*p));

    (asks_user_to_choose || mentions_work_without_doing) && !reports_finished_work
}

/// 「次に返します」のような段取り説明だけでレビュー本文が欠けているメッセージを検出する。
pub(super) fn is_placeholder_review_message(message: &str) -> bool {
    let m = message.trim();
    if m.chars().count() < 500 {
        return true;
    }
    let placeholder_phrases = [
        "次は",
        "次に",
        "これから",
        "以下に",
        "返します",
        "まとめます",
        "準備が整",
        "読み込みが完了",
        "作業ステップ",
    ];
    let placeholder_hits = placeholder_phrases
        .iter()
        .filter(|p| m.contains(**p))
        .count();
    let concrete_markers = [
        "問題",
        "原因",
        "改善",
        "リスク",
        "修正",
        "src/",
        ".rs",
        "line",
        "行",
        "Permission",
        "ERROR",
    ];
    let concrete_hits = concrete_markers.iter().filter(|p| m.contains(**p)).count();
    placeholder_hits >= 2 && concrete_hits < 3
}

/// bot のメッセージに現在のプロジェクトに存在しないパスが含まれていれば列挙する。
pub(super) fn missing_referenced_project_paths(message: &str, root: &Path) -> Vec<String> {
    let mut paths = Vec::new();
    for token in message.split_whitespace() {
        let Some(start) = token.find(|c: char| c.is_ascii_alphanumeric() || c == '.') else {
            continue;
        };
        let candidate = token[start..].trim_matches(|c: char| {
            c.is_ascii_punctuation()
                || c.is_whitespace()
                || matches!(
                    c,
                    '`' | '"'
                        | '\''
                        | '「'
                        | '」'
                        | '『'
                        | '』'
                        | '（'
                        | '）'
                        | '、'
                        | '。'
                        | '：'
                        | '；'
                )
        });
        if !looks_like_project_path(candidate) {
            continue;
        }
        if !root.join(candidate).exists() && !paths.iter().any(|p| p == candidate) {
            paths.push(candidate.to_string());
        }
    }
    paths
}

/// `src/foo/bar.rs` のような相対プロジェクトパスらしい文字列か判定する。
pub(super) fn looks_like_project_path(s: &str) -> bool {
    if s.starts_with('/')
        || s.starts_with("http://")
        || s.starts_with("https://")
        || s.contains("..")
        || !s.contains('/')
    {
        return false;
    }

    let Some(file_name) = s.rsplit('/').next() else {
        return false;
    };
    file_name.contains('.') && !file_name.ends_with('.')
}
