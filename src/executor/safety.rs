/// 実行を許可するコマンド名（allowlist 方式）
/// mv / cp / touch は file/patch/mkdir で代替できるため除外
/// env / make はサブコマンド経由で任意実行になり得るため除外
///
/// 注意（Windows）: cat / head / tail / grep / find / ls / wc / diff / file /
/// sort / uniq / tr / cut は Windows に標準では存在しない。
/// Git for Windows または busybox をインストールしている環境では使用可能。
/// それ以外の環境では実行時 NotFound エラーになる（クラッシュはしない）。
pub const ALLOWED_EXECUTABLES: &[&str] = &[
    // Rust toolchain（サブコマンドは ALLOWED_CARGO_SUBCMDS で制限）
    "cargo", "rustc", "rustfmt",
    // Haskell toolchain（cabal/stack はサブコマンドを制限。runghc/runhaskell はスクリプト直接実行のため除外）
    "ghc", "ghc-pkg", "cabal", "stack", "hlint", "hoogle",
    // Node.js / TypeScript toolchain（node/npx はスクリプト直接実行のため除外。npm はサブコマンドを制限）
    "tsc", "eslint", "prettier", "npm",
    // バージョン管理（読み取り系のみ。書き込み系は ALLOWED_GIT_SUBCMDS で制限）
    "git",
    // ファイル閲覧・検索（書き込みなし）
    // ※ Windows では Git for Windows / busybox 経由でのみ使用可能
    "cat", "head", "tail", "grep", "rg", "find", "ls", "wc", "diff", "file",
    // テキスト処理（awk は system() でシェル実行可、sed は w コマンドで書き込み可のため除外）
    // ※ Windows では Git for Windows / busybox 経由でのみ使用可能
    "sort", "uniq", "tr", "cut", "jq",
    // 情報表示（引数ゼロ限定。環境変数表示のみ）
    "echo", "printf", "date",
    // Windows: PATH 検索（where.exe）
    "where",
];

/// git で許可する読み取り系サブコマンド（それ以外はすべて拒否）
const ALLOWED_GIT_SUBCMDS: &[&str] = &[
    "log",
    "status",
    "diff",
    "show",
    "blame",
    "ls-files",
    "describe",
    "branch",
    "tag",
    "grep",
    "rev-parse",
    "cat-file",
    "shortlog",
    "reflog",
];

/// cargo で許可するサブコマンド
/// 注意: check / clippy / doc も build.rs・proc macro 経由で任意コードを実行し得る。
/// build は加えてバイナリ生成まで行うため除外。untrusted リポジトリへの使用は本質的にリスクを伴う。
const ALLOWED_CARGO_SUBCMDS: &[&str] = &["check", "fmt", "clippy", "doc", "clean"];

/// cargo で明示的に拒否するサブコマンド（任意コード実行の恐れ）
const BLOCKED_CARGO_SUBCMDS: &[&str] = &[
    "build", "run", "test", "bench", "fix", "install", "publish",
];

/// cabal で許可するサブコマンド
/// run / test / bench / exec は任意コードを実行するため除外。
/// build / haddock は Setup.hs・カスタムセットアップ経由で任意コードを実行し得るため除外。
const ALLOWED_CABAL_SUBCMDS: &[&str] = &["check", "clean", "sdist", "info", "list", "freeze"];

/// cabal で明示的に拒否するサブコマンド（任意コード実行の恐れ）
const BLOCKED_CABAL_SUBCMDS: &[&str] = &[
    "build", "haddock", "run", "test", "bench", "exec", "install", "upload", "publish",
];

/// stack で許可するサブコマンド
/// run / test / exec / script / ghci はバイナリ・テストコードを実行するため除外。
/// build / haddock は Setup.hs・Template Haskell 経由で任意コードを実行し得るため除外。
const ALLOWED_STACK_SUBCMDS: &[&str] = &["clean", "sdist", "ls", "query", "path", "dot", "ide"];

/// stack で明示的に拒否するサブコマンド（任意コード実行の恐れ）
const BLOCKED_STACK_SUBCMDS: &[&str] = &[
    "build", "haddock", "run", "test", "bench", "exec", "ghci", "repl", "script", "install",
    "upload", "publish",
];

/// npm で許可するサブコマンド
/// run / exec / start / test はpackage.jsonの任意スクリプトを実行するため除外。
/// install / ci / pack は preinstall / postinstall / prepack 等のライフサイクルスクリプトを
/// 実行し得るが、--ignore-scripts を cmd.rs 側で自動付加することで許可する。
pub const NPM_INSTALL_SUBCMDS: &[&str] = &["install", "ci", "pack"];
const ALLOWED_NPM_SUBCMDS: &[&str] = &[
    "install", "ci", "list", "ls", "audit", "outdated", "view", "info", "show", "pack",
];

/// npm で明示的に拒否するサブコマンド（任意コード実行の恐れ）
const BLOCKED_NPM_SUBCMDS: &[&str] = &[
    "run", "exec", "start", "test", "publish", "init", "link", "unlink",
];

/// コマンド固有の危険フラグ（allowlist 通過後に追加チェック）
const BLOCKED_ARGS: &[(&str, &[&str])] = &[("find", &["-delete", "-exec", "-execdir"])];

pub fn check_cmd_safety(cmd: &[String]) -> Result<(), String> {
    let exe = cmd.first().ok_or_else(|| "cmd が空です".to_string())?;

    if crate::paths::is_absolute_path_arg(exe) {
        return Err(format!(
            "Permission denied: 絶対パス '{exe}' での実行は禁止です"
        ));
    }
    // 相対パス付き実行（./cargo, tools/git など）は basename allowlist を迂回できるため拒否
    if exe.contains('/') || exe.contains('\\') {
        return Err(format!(
            "Permission denied: パス区切りを含む '{exe}' は禁止です。コマンド名のみを指定してください"
        ));
    }

    let basename = exe.as_str();

    // allowlist: 許可リストにないコマンドはすべて拒否
    if !ALLOWED_EXECUTABLES.contains(&basename) {
        return Err(format!(
            "Permission denied: '{basename}' は許可されていません。許可コマンド: {}",
            ALLOWED_EXECUTABLES.join(", ")
        ));
    }

    // 引数チェック: 絶対パス・`..` を拒否
    for arg in &cmd[1..] {
        if crate::paths::is_absolute_path_arg(arg) {
            return Err(format!(
                "Permission denied: 引数 '{arg}' に絶対パスが含まれています"
            ));
        }
        if crate::paths::has_parent_component_arg(arg) {
            return Err(format!(
                "Permission denied: 引数 '{arg}' に '..' が含まれています"
            ));
        }
    }

    // git は読み取り系サブコマンドのみ許可
    if basename == "git" {
        let subcmd = cmd.get(1).map(|s| s.as_str()).unwrap_or("");
        if !ALLOWED_GIT_SUBCMDS.contains(&subcmd) {
            return Err(format!(
                "Permission denied: 'git {subcmd}' は許可されていません。許可サブコマンド: {}",
                ALLOWED_GIT_SUBCMDS.join(", ")
            ));
        }
    }

    // cargo はサブコマンドを制限（ビルドスクリプト/proc macro 経由の任意実行を抑制）
    if basename == "cargo" {
        let subcmd = cmd.get(1).map(|s| s.as_str()).unwrap_or("");
        if BLOCKED_CARGO_SUBCMDS.contains(&subcmd) {
            return Err(blocked_cargo_subcmd_message(subcmd));
        }
        if !ALLOWED_CARGO_SUBCMDS.contains(&subcmd) {
            return Err(format!(
                "Permission denied: 'cargo {subcmd}' は許可されていません。許可サブコマンド: {}",
                ALLOWED_CARGO_SUBCMDS.join(", ")
            ));
        }
    }

    // cabal はサブコマンドを制限
    if basename == "cabal" {
        let subcmd = cmd.get(1).map(|s| s.as_str()).unwrap_or("");
        if BLOCKED_CABAL_SUBCMDS.contains(&subcmd) {
            return Err(format!(
                "Permission denied: 'cabal {subcmd}' は任意コードを実行できるため禁止です。許可サブコマンド: {}",
                ALLOWED_CABAL_SUBCMDS.join(", ")
            ));
        }
        if !ALLOWED_CABAL_SUBCMDS.contains(&subcmd) {
            return Err(format!(
                "Permission denied: 'cabal {subcmd}' は許可されていません。許可サブコマンド: {}",
                ALLOWED_CABAL_SUBCMDS.join(", ")
            ));
        }
    }

    // stack はサブコマンドを制限
    if basename == "stack" {
        let subcmd = cmd.get(1).map(|s| s.as_str()).unwrap_or("");
        if BLOCKED_STACK_SUBCMDS.contains(&subcmd) {
            return Err(format!(
                "Permission denied: 'stack {subcmd}' は任意コードを実行できるため禁止です。許可サブコマンド: {}",
                ALLOWED_STACK_SUBCMDS.join(", ")
            ));
        }
        if !ALLOWED_STACK_SUBCMDS.contains(&subcmd) {
            return Err(format!(
                "Permission denied: 'stack {subcmd}' は許可されていません。許可サブコマンド: {}",
                ALLOWED_STACK_SUBCMDS.join(", ")
            ));
        }
    }

    // npm はサブコマンドを制限（package.json の任意スクリプト実行を防ぐ）
    if basename == "npm" {
        let subcmd = cmd.get(1).map(|s| s.as_str()).unwrap_or("");
        if BLOCKED_NPM_SUBCMDS.contains(&subcmd) {
            return Err(format!(
                "Permission denied: 'npm {subcmd}' は任意コードを実行できるため禁止です。許可サブコマンド: {}",
                ALLOWED_NPM_SUBCMDS.join(", ")
            ));
        }
        if !ALLOWED_NPM_SUBCMDS.contains(&subcmd) {
            return Err(format!(
                "Permission denied: 'npm {subcmd}' は許可されていません。許可サブコマンド: {}",
                ALLOWED_NPM_SUBCMDS.join(", ")
            ));
        }
    }

    // date / echo / printf は引数なし（または安全な書式フラグのみ）を許可
    // 引数に実行可能なコマンド名が渡される可能性は低いが、念のためシェル特殊文字を拒否
    if matches!(basename, "date" | "echo" | "printf") {
        for arg in &cmd[1..] {
            if arg.contains('$') || arg.contains('`') || arg.contains(';') {
                return Err(format!(
                    "Permission denied: '{basename}' の引数にシェル特殊文字が含まれています: '{arg}'"
                ));
            }
        }
    }

    // コマンド固有の危険フラグを拒否
    for (target, blocked) in BLOCKED_ARGS {
        if basename == *target {
            for arg in &cmd[1..] {
                if blocked.contains(&arg.as_str()) {
                    return Err(format!(
                        "Permission denied: '{basename} {arg}' は危険なため禁止です"
                    ));
                }
            }
        }
    }

    Ok(())
}

/// ファイル引数を取るコマンドについて、引数がシンボリックリンク経由でルート外を指していないか確認する。
/// workdir から相対パスで解決し、canonicalize 後に root 内に収まっているかチェックする。
/// ファイルパスを引数に取らないコマンド（echo/printf/date/where）だけを除外し、
/// それ以外の許可コマンド全体に適用することで sort/cut/uniq 等の迂回を防ぐ。
pub fn check_file_args_within_root(
    cmd: &[String],
    workdir: &std::path::Path,
    root: &std::path::Path,
) -> Result<(), String> {
    // ファイルパスを引数に取らないコマンドのみ除外（引数が文字列リテラル・書式・コマンド名のみ）
    const NO_FILE_ARG_CMDS: &[&str] = &["echo", "printf", "date", "where"];
    let exe = cmd.first().map(|s| s.as_str()).unwrap_or("");
    if NO_FILE_ARG_CMDS.contains(&exe) {
        return Ok(());
    }
    let root_canonical = crate::paths::canonicalize_clean(root)
        .map_err(|e| format!("Permission denied: root の解決失敗: {e}"))?;
    for arg in &cmd[1..] {
        // フラグ・空文字はスキップ
        if arg.starts_with('-') || arg.is_empty() {
            continue;
        }
        // 絶対パスは check_cmd_safety で拒否済みのためスキップ
        if std::path::Path::new(arg).is_absolute() {
            continue;
        }
        let candidate = workdir.join(arg);
        if candidate.exists() {
            match crate::paths::canonicalize_clean(&candidate) {
                Ok(canonical) => {
                    if !canonical.starts_with(&root_canonical) {
                        return Err(format!(
                            "Permission denied: '{arg}' はプロジェクトルート外を指しています（シンボリックリンク経由の可能性）"
                        ));
                    }
                }
                Err(e) => {
                    return Err(format!(
                        "Permission denied: '{arg}' のパス解決に失敗しました: {e}"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn blocked_cargo_subcmd_message(subcmd: &str) -> String {
    let mut message = format!(
        "Permission denied: 'cargo {subcmd}' はビルドスクリプト/proc macro/バイナリ経由で任意コードを実行できるため禁止です"
    );

    if subcmd == "test" {
        message.push_str(
            "。テストコードのコンパイル確認だけなら、代わりに ['cargo','check','--tests'] または ['cargo','check','--all-targets'] を実行してください。実際のテスト実行が必要な場合は bot でその旨を報告してください",
        );
    }

    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_cargo_test_suggests_check_tests() {
        let cmd = vec!["cargo".to_string(), "test".to_string()];
        let err = check_cmd_safety(&cmd).unwrap_err();

        assert!(err.contains("cargo test"));
        assert!(err.contains("cargo','check','--tests"));
        assert!(err.contains("実際のテスト実行が必要な場合"));
    }

    #[test]
    fn cargo_check_tests_is_allowed() {
        let cmd = vec![
            "cargo".to_string(),
            "check".to_string(),
            "--tests".to_string(),
        ];

        assert!(check_cmd_safety(&cmd).is_ok());
    }

    #[test]
    fn cargo_build_is_blocked() {
        let cmd = vec!["cargo".to_string(), "build".to_string()];
        assert!(check_cmd_safety(&cmd).is_err());
    }

    // シンボリックリンクは Unix 固有の仕組みのため、関連テストを unix のみに制限する
    #[cfg(unix)]
    fn make_symlink_outside_root() -> (tempfile::TempDir, tempfile::NamedTempFile) {
        let dir = tempfile::tempdir().unwrap();
        let target = tempfile::NamedTempFile::new().unwrap();
        let link = dir.path().join("secret");
        std::os::unix::fs::symlink(target.path(), &link).unwrap();
        (dir, target)
    }

    #[cfg(unix)]
    #[test]
    fn check_file_args_blocks_symlink_outside_root_for_cat() {
        let (dir, _target) = make_symlink_outside_root();
        let cmd = vec!["cat".to_string(), "secret".to_string()];
        assert!(
            check_file_args_within_root(&cmd, dir.path(), dir.path()).is_err(),
            "cat: symlink outside root must be blocked"
        );
    }

    #[cfg(unix)]
    #[test]
    fn check_file_args_blocks_symlink_outside_root_for_sort() {
        let (dir, _target) = make_symlink_outside_root();
        let cmd = vec!["sort".to_string(), "secret".to_string()];
        assert!(
            check_file_args_within_root(&cmd, dir.path(), dir.path()).is_err(),
            "sort: symlink outside root must be blocked"
        );
    }

    #[cfg(unix)]
    #[test]
    fn check_file_args_blocks_symlink_outside_root_for_cut() {
        let (dir, _target) = make_symlink_outside_root();
        let cmd = vec!["cut".to_string(), "-c1-".to_string(), "secret".to_string()];
        assert!(
            check_file_args_within_root(&cmd, dir.path(), dir.path()).is_err(),
            "cut: symlink outside root must be blocked"
        );
    }

    #[test]
    fn check_file_args_allows_file_within_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "hello").unwrap();

        let cmd = vec!["cat".to_string(), "README.md".to_string()];
        assert!(check_file_args_within_root(&cmd, dir.path(), dir.path()).is_ok());
    }

    #[test]
    fn check_file_args_skips_echo_args() {
        // echo の引数はファイルパスではないのでチェック不要（存在しない名前でもエラーなし）
        let dir = tempfile::tempdir().unwrap();
        let cmd = vec!["echo".to_string(), "hello world".to_string()];
        assert!(check_file_args_within_root(&cmd, dir.path(), dir.path()).is_ok());
    }
}
