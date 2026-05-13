/// タスク分類・内容判定ユーティリティ（core-internals.md §「agent-loop / run()」参考）
///
/// runner.rs のメインループから切り出したピュア文字列解析関数群。
/// 副作用なし・I/O なしのため独立してテスト可能。
use super::session_store::SessionData;
use std::path::Path;

// ─── 初期プロンプト分類 ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InitialCommandRequest {
    pub name: &'static str,
    pub cmd: Vec<&'static str>,
    pub timeout: u64,
}

/// 依頼文に既知の拡張子を持つファイル名が明示されているか判定する。
///
/// ファイル名が明示されている場合、AI は glob を省略して直接 read_file へ進める。
pub(super) fn task_mentions_explicit_filename(task: &str) -> bool {
    const KNOWN_EXTS: &[&str] = &[
        "md",
        "txt",
        "rs",
        "toml",
        "json",
        "yaml",
        "yml",
        "py",
        "js",
        "ts",
        "tsx",
        "jsx",
        "c",
        "cpp",
        "h",
        "html",
        "css",
        "sh",
        "hs",
        "lhs",
        "cabal",
        "go",
        "java",
        "kt",
        "swift",
        "rb",
        "php",
        "cs",
        "scala",
        "ex",
        "exs",
        "vue",
        "svelte",
        "astro",
        "sql",
        "graphql",
        "proto",
        "tf",
        "dockerfile",
        "lock",
        "mod",
        "sum",
        "conf",
        "ini",
        "env",
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

/// 短い抽象的な開発依頼（例: "IRの拡張", "UI改善", "add feature"）を検出する。
///
/// 日本語テキストは 24 文字以下、ASCII 主体のテキストは 48 文字以下を「短い」と判定する。
/// （日本語は 1 文字あたりの情報量が多いため閾値を低く設定する）
pub(super) fn is_short_open_ended_development_task(task: &str) -> bool {
    let trimmed = task.trim();
    if trimmed.is_empty() {
        return false;
    }

    // 主に日本語か ASCII かで文字数上限を分ける
    let non_ascii_count = trimmed.chars().filter(|c| !c.is_ascii()).count();
    let total_count = trimmed.chars().count();
    let is_primarily_japanese = non_ascii_count * 2 > total_count;
    let char_limit = if is_primarily_japanese { 24 } else { 48 };
    if total_count > char_limit {
        return false;
    }

    let lower = trimmed.to_ascii_lowercase();

    // ファイル名・具体的コマンドが含まれていれば短い依頼としては扱わない
    if trimmed.contains(".rs")
        || trimmed.contains(".md")
        || trimmed.contains(".txt")
        || trimmed.contains(".ts")
        || trimmed.contains(".py")
        || trimmed.contains(".go")
        || lower.contains("cargo")
        || lower.contains("npm")
        || lower.contains("pytest")
        || lower.contains("make")
    {
        return false;
    }

    let development_markers_ja = [
        "実装",
        "追加",
        "拡張",
        "改善",
        "改修",
        "修正",
        "変更",
        "直し",
        "直して",
        "対応",
        "作成",
        "リファクタ",
        "IR",
        "API",
        "UI",
    ];
    let development_markers_en = [
        "implement",
        "add",
        "extend",
        "improve",
        "refactor",
        "fix",
        "update",
        "create",
        "change",
        "migrate",
        "optimize",
        "cleanup",
        "clean up",
        "support",
    ];
    development_markers_ja
        .iter()
        .any(|marker| trimmed.contains(marker))
        || development_markers_en
            .iter()
            .any(|marker| lower.contains(marker))
}

/// ユーザーが安全な検証・整形・読み取りコマンドを明示した場合の初手コマンドを返す。
///
/// 任意コマンド実行にしないため、executor の allowlist の中でもコーディング作業で
/// 初手に使いやすい読み取り/検証系だけをここで扱う。
pub(super) fn initial_command_request(task: &str) -> Option<InitialCommandRequest> {
    let lower = task.to_ascii_lowercase();

    let command = if lower.contains("cargo check")
        || lower.contains("cargo-check")
        || lower.contains("cargo　check")
        || task.contains("カーゴチェック")
        || task.contains("カーゴ check")
        || task.contains("cargoチェック")
    {
        Some(("cargo check", vec!["cargo", "check"], 120))
    } else if lower.contains("cargo fmt") || task.contains("カーゴfmt") {
        Some(("cargo fmt", vec!["cargo", "fmt"], 120))
    } else if lower.contains("cargo clippy") || task.contains("カーゴclippy") {
        Some(("cargo clippy", vec!["cargo", "clippy"], 180))
    } else if lower.contains("cargo doc") {
        Some(("cargo doc", vec!["cargo", "doc"], 180))
    } else if lower.contains("cargo test")
        || task.contains("カーゴtest")
        || task.contains("カーゴテスト")
    {
        Some(("cargo test", vec!["cargo", "test"], 180))
    } else if lower.contains("cargo build")
        || task.contains("カーゴbuild")
        || task.contains("カーゴビルド")
    {
        Some(("cargo build", vec!["cargo", "build"], 180))
    } else if lower.contains("tsc --noemit")
        || lower.contains("tsc --no-emit")
        || lower.contains("tsc")
        || task.contains("型チェック")
    {
        Some(("tsc --noEmit", vec!["tsc", "--noEmit"], 120))
    } else if lower.contains("npm test") || lower.contains("npm run test") {
        Some(("npm test", vec!["npm", "test"], 180))
    } else if lower.contains("npm run build") || lower.contains("npm build") {
        Some(("npm run build", vec!["npm", "run", "build"], 180))
    } else if lower.contains("npm run dev") {
        Some(("npm run dev", vec!["npm", "run", "dev"], 60))
    } else if lower.contains("yarn test") {
        Some(("yarn test", vec!["yarn", "test"], 180))
    } else if lower.contains("yarn build") {
        Some(("yarn build", vec!["yarn", "build"], 180))
    } else if lower.contains("pnpm test") {
        Some(("pnpm test", vec!["pnpm", "test"], 180))
    } else if lower.contains("pnpm build") {
        Some(("pnpm build", vec!["pnpm", "build"], 180))
    } else if lower.contains("pytest") || lower.contains("py.test") {
        Some(("pytest", vec!["pytest"], 180))
    } else if lower.contains("python -m pytest") {
        Some(("python -m pytest", vec!["python", "-m", "pytest"], 180))
    } else if lower.contains("go test") {
        Some(("go test", vec!["go", "test", "./..."], 180))
    } else if lower.contains("go build") {
        Some(("go build", vec!["go", "build", "./..."], 120))
    } else if lower.contains("go vet") {
        Some(("go vet", vec!["go", "vet", "./..."], 60))
    } else if lower.contains("dotnet build") {
        Some(("dotnet build", vec!["dotnet", "build"], 180))
    } else if lower.contains("dotnet test") {
        Some(("dotnet test", vec!["dotnet", "test"], 180))
    } else if lower.contains("make test") {
        Some(("make test", vec!["make", "test"], 180))
    } else if lower.contains("make build") {
        Some(("make build", vec!["make", "build"], 180))
    } else if lower == "make" || lower.trim_end() == "make" {
        Some(("make", vec!["make"], 180))
    } else if lower.contains("mvn test") || lower.contains("maven test") {
        Some(("mvn test", vec!["mvn", "test"], 300))
    } else if lower.contains("mvn compile") || lower.contains("mvn build") {
        Some(("mvn compile", vec!["mvn", "compile"], 300))
    } else if lower.contains("gradle test") {
        Some(("gradle test", vec!["./gradlew", "test"], 300))
    } else if lower.contains("gradle build") {
        Some(("gradle build", vec!["./gradlew", "build"], 300))
    } else if lower.contains("git status") || task.contains("gitステータス") {
        Some(("git status", vec!["git", "status"], 60))
    } else if lower.contains("git diff") {
        Some(("git diff", vec!["git", "diff"], 60))
    } else if lower.contains("git log") {
        Some(("git log", vec!["git", "log", "--oneline", "-20"], 30))
    } else {
        None
    }?;

    Some(InitialCommandRequest {
        name: command.0,
        cmd: command.1,
        timeout: command.2,
    })
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

/// resume なしでは対象が分からない「中身を入れて」系の短い依頼を検出する。
pub(super) fn is_ambiguous_file_fill_request(task: &str) -> bool {
    let trimmed = task.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() > 32
        || task_mentions_explicit_filename(trimmed)
    {
        return false;
    }

    [
        "中身をいれて",
        "中身を入れて",
        "内容をいれて",
        "内容を入れて",
        "本文をいれて",
        "本文を入れて",
        "原文をいれて",
        "原文を入れて",
    ]
    .iter()
    .any(|phrase| trimmed.contains(phrase))
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
    let first = first
        .trim_end_matches(|c: char| c.is_ascii_punctuation() || matches!(c, '。' | '、' | '．'));

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
    resume.is_none()
        && (is_non_actionable_ack(task)
            || is_menu_selection_without_context(task)
            || is_ambiguous_file_fill_request(task))
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
        || task.contains(".ts")
        || task.contains(".tsx")
        || task.contains(".js")
        || task.contains(".jsx")
        || task.contains(".py")
        || task.contains(".go")
        || task.contains(".java")
        || task.contains(".kt")
        || task.contains(".cs")
        || task.contains(".swift")
        || task.contains(".rb")
        || task.contains(".html")
        || task.contains(".css")
        || task.contains(".json")
        || task.contains(".yaml")
        || task.contains(".yml")
        || task.contains(".toml")
        || task.contains("ファイル")
        || task.to_ascii_lowercase().contains("file");
    let asks_update = task.contains("書き換")
        || task.contains("書換")
        || task.contains("翻訳")
        || task.contains("英語")
        || task.contains("日本語")
        || task.contains("更新")
        || task.contains("修正")
        || task.contains("改修")
        || task.contains("変換")
        || task.contains("分割")
        || task.to_ascii_lowercase().contains("translate")
        || task.to_ascii_lowercase().contains("rewrite")
        || task.to_ascii_lowercase().contains("update")
        || task.to_ascii_lowercase().contains("convert")
        || task.to_ascii_lowercase().contains("split");
    has_file_like_target && asks_update
}

/// ファイル本文を複数ファイルへ分ける依頼か判定する。
pub(super) fn task_requires_file_split(task: &str) -> bool {
    task_requires_file_update(task) && task.contains("分割")
}

/// 実装・追加・修正など実際の開発アクションを要求するタスクか判定する。
pub(super) fn task_requires_development_action(task: &str) -> bool {
    let lower = task.to_ascii_lowercase();
    task.contains("実装")
        || task.contains("追加")
        || task.contains("拡張")
        || task.contains("改善")
        || task.contains("改修")
        || task.contains("修正")
        || task.contains("変更")
        || task.contains("直し")
        || task.contains("直して")
        || task.contains("対応")
        || task.contains("作成")
        || task.contains("エラー")
        || task.contains("バグ")
        || task.contains("コンパイル")
        || task.contains("ビルド")
        || task.contains("リント")
        || task.contains("リファクタ")
        || lower.contains("implement")
        || lower.contains("add")
        || lower.contains("improve")
        || lower.contains("fix")
        || lower.contains("bug")
        || lower.contains("error")
        || lower.contains("compile")
        || lower.contains("build")
        || lower.contains("lint")
        || lower.contains("refactor")
}

/// レビュー・調査・分析系の出力を求めるタスクか判定する。
pub(super) fn task_requires_review_output(task: &str) -> bool {
    let lower = task.to_ascii_lowercase();
    if initial_command_request(task).is_some() {
        return false;
    }

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
        "build",
        "test",
    ]
    .iter()
    .any(|p| m.contains(*p));

    (asks_user_to_choose || mentions_work_without_doing) && !reports_finished_work
}

/// 作業後の最終応答が、未完了の「次の選択肢」提示で終わっているかを検出する。
pub(super) fn is_incomplete_handoff_message(message: &str) -> bool {
    let m = message.trim();
    if m.chars().count() < 80 {
        return false;
    }

    let asks_next_choice = [
        "次に選べる作業",
        "次の選択肢",
        "選択を教えて",
        "選んでください",
        "どれか一つ",
        "次に進める",
        "次に進める方針",
    ]
    .iter()
    .any(|p| m.contains(*p));

    let mentions_unfinished_work = [
        "プレースホルダ",
        "原文を",
        "挿入",
        "完了する",
        "作業が完了",
        "次に",
        "残り",
        "追加の",
    ]
    .iter()
    .any(|p| m.contains(*p));

    let reports_verified_completion = [
        "cargo check",
        "ビルド",
        "テスト",
        "build",
        "test",
        "確認済み",
        "完了しました",
    ]
    .iter()
    .any(|p| m.contains(*p));

    asks_next_choice && mentions_unfinished_work && !reports_verified_completion
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

/// `bot` の最終回答がテンプレートの仮文のままか判定する。
pub(super) fn is_placeholder_bot_message(message: &str) -> bool {
    let m = message.trim();
    if m.is_empty() {
        return true;
    }

    let normalized = m.trim_matches(|c: char| {
        c.is_ascii_punctuation()
            || c.is_whitespace()
            || matches!(
                c,
                '（' | '）' | '(' | ')' | '「' | '」' | '『' | '』' | '。' | '、'
            )
    });

    matches!(
        normalized,
        "完全な回答" | "完全な回答をここに" | "ここにレビュー本文" | "現状と選択肢"
    ) || (m.contains("（完全な回答") && m.chars().count() <= 40)
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
