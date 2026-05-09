/// 実行を許可するコマンド名（allowlist 方式）
/// mv / cp / touch は file/patch/mkdir で代替できるため除外
pub const ALLOWED_EXECUTABLES: &[&str] = &[
    // Rust toolchain
    "cargo", "rustc", "rustfmt",
    // バージョン管理（読み取り系のみ。書き込み系は ALLOWED_GIT_SUBCMDS で制限）
    "git",
    // ファイル閲覧・検索（書き込みなし）
    "cat", "head", "tail", "grep", "rg", "find", "ls", "wc", "diff", "file",
    // テキスト処理（sed は -i を別途ブロック）
    "sort", "uniq", "tr", "cut", "awk", "sed", "jq",
    // 情報表示
    "echo", "printf", "date", "env",
];

/// git で許可する読み取り系サブコマンド（それ以外はすべて拒否）
const ALLOWED_GIT_SUBCMDS: &[&str] = &[
    "log", "status", "diff", "show", "blame", "ls-files",
    "describe", "branch", "tag", "grep", "rev-parse", "cat-file",
    "shortlog", "reflog",
];

/// コマンド固有の危険フラグ（allowlist 通過後に追加チェック）
const BLOCKED_ARGS: &[(&str, &[&str])] = &[
    ("sed",  &["-i", "--in-place"]),
    ("find", &["-delete", "-exec", "-execdir"]),
];

pub fn check_cmd_safety(cmd: &[String]) -> Result<(), String> {
    let exe = cmd.first().ok_or_else(|| "cmd が空です".to_string())?;

    if exe.starts_with('/') {
        return Err(format!("アクセス拒否: 絶対パス '{exe}' での実行は禁止です"));
    }
    // 相対パス付き実行（./cargo, tools/git など）は basename allowlist を迂回できるため拒否
    if exe.contains('/') || exe.contains('\\') {
        return Err(format!(
            "アクセス拒否: パス区切りを含む '{exe}' は禁止です。コマンド名のみを指定してください"
        ));
    }

    let basename = exe.as_str();

    // allowlist: 許可リストにないコマンドはすべて拒否
    if !ALLOWED_EXECUTABLES.contains(&basename) {
        return Err(format!(
            "アクセス拒否: '{basename}' は許可されていません。許可コマンド: {}",
            ALLOWED_EXECUTABLES.join(", ")
        ));
    }

    // 引数チェック: 絶対パス・`..` を拒否
    for arg in &cmd[1..] {
        if arg.starts_with('/') {
            return Err(format!("アクセス拒否: 引数 '{arg}' に絶対パスが含まれています"));
        }
        if arg.contains("..") {
            return Err(format!("アクセス拒否: 引数 '{arg}' に '..' が含まれています"));
        }
    }

    // git は読み取り系サブコマンドのみ許可
    if basename == "git" {
        let subcmd = cmd.get(1).map(|s| s.as_str()).unwrap_or("");
        if !ALLOWED_GIT_SUBCMDS.contains(&subcmd) {
            return Err(format!(
                "アクセス拒否: 'git {subcmd}' は許可されていません。許可サブコマンド: {}",
                ALLOWED_GIT_SUBCMDS.join(", ")
            ));
        }
    }

    // コマンド固有の危険フラグを拒否
    for (target, blocked) in BLOCKED_ARGS {
        if basename == *target {
            for arg in &cmd[1..] {
                if blocked.contains(&arg.as_str()) {
                    return Err(format!(
                        "アクセス拒否: '{basename} {arg}' は危険なため禁止です"
                    ));
                }
            }
        }
    }

    Ok(())
}
