use copipe_ai::executor::safety::check_cmd_safety;

#[test]
fn allowed_command_passes() {
    let cmd = vec!["cargo".to_string(), "build".to_string()];
    assert!(check_cmd_safety(&cmd).is_ok());
}

#[test]
fn disallowed_command_fails() {
    let cmd = vec!["rm".to_string(), "-rf".to_string(), "/".to_string()];
    assert!(check_cmd_safety(&cmd).is_err());
}

#[test]
fn absolute_exe_fails() {
    let cmd = vec!["/bin/cargo".to_string(), "build".to_string()];
    assert!(check_cmd_safety(&cmd).is_err());
}

#[test]
fn windows_absolute_exe_fails() {
    let cmd = vec![
        r"C:\Windows\System32\cmd.exe".to_string(),
        "/C".to_string(),
        "dir".to_string(),
    ];
    assert!(check_cmd_safety(&cmd).is_err());
}

#[test]
fn exe_with_path_separator_fails() {
    let cmd = vec!["./cargo".to_string(), "build".to_string()];
    assert!(check_cmd_safety(&cmd).is_err());
}

#[test]
fn arg_with_parent_dir_fails() {
    let cmd = vec!["cargo".to_string(), "build".to_string(), "..".to_string()];
    assert!(check_cmd_safety(&cmd).is_err());
}

#[test]
fn windows_absolute_arg_fails() {
    let cmd = vec!["cat".to_string(), r"C:\Users\akira\secret.txt".to_string()];
    assert!(check_cmd_safety(&cmd).is_err());
}

#[test]
fn windows_unc_arg_fails() {
    let cmd = vec!["cat".to_string(), r"\\server\share\secret.txt".to_string()];
    assert!(check_cmd_safety(&cmd).is_err());
}

#[test]
fn git_allowed_subcommand_passes() {
    let cmd = vec!["git".to_string(), "status".to_string()];
    assert!(check_cmd_safety(&cmd).is_ok());
}

#[test]
fn git_disallowed_subcommand_fails() {
    let cmd = vec!["git".to_string(), "push".to_string()];
    assert!(check_cmd_safety(&cmd).is_err());
}

#[test]
fn blocked_arg_for_sed_fails() {
    let cmd = vec![
        "sed".to_string(),
        "-i".to_string(),
        "s/a/b/".to_string(),
        "file.txt".to_string(),
    ];
    assert!(check_cmd_safety(&cmd).is_err());
}

#[test]
fn ghc_allowed() {
    let cmd = vec!["ghc".to_string(), "Main.hs".to_string()];
    assert!(check_cmd_safety(&cmd).is_ok());
}

#[test]
fn hlint_allowed() {
    let cmd = vec!["hlint".to_string(), "src/".to_string()];
    assert!(check_cmd_safety(&cmd).is_ok());
}

#[test]
fn cabal_build_allowed() {
    let cmd = vec!["cabal".to_string(), "build".to_string()];
    assert!(check_cmd_safety(&cmd).is_ok());
}

#[test]
fn cabal_run_blocked() {
    let cmd = vec!["cabal".to_string(), "run".to_string()];
    assert!(check_cmd_safety(&cmd).is_err());
}

#[test]
fn cabal_test_blocked() {
    let cmd = vec!["cabal".to_string(), "test".to_string()];
    assert!(check_cmd_safety(&cmd).is_err());
}

#[test]
fn stack_build_allowed() {
    let cmd = vec!["stack".to_string(), "build".to_string()];
    assert!(check_cmd_safety(&cmd).is_ok());
}

#[test]
fn stack_ghci_blocked() {
    let cmd = vec!["stack".to_string(), "ghci".to_string()];
    assert!(check_cmd_safety(&cmd).is_err());
}

#[test]
fn stack_exec_blocked() {
    let cmd = vec!["stack".to_string(), "exec".to_string(), "--".to_string(), "myapp".to_string()];
    assert!(check_cmd_safety(&cmd).is_err());
}

#[test]
fn runghc_blocked() {
    // runghc はスクリプトを直接実行できるため allowlist 外
    let cmd = vec!["runghc".to_string(), "Main.hs".to_string()];
    assert!(check_cmd_safety(&cmd).is_err());
}

// ── React / TypeScript ────────────────────────────────────────

#[test]
fn tsc_allowed() {
    let cmd = vec!["tsc".to_string(), "--noEmit".to_string()];
    assert!(check_cmd_safety(&cmd).is_ok());
}

#[test]
fn eslint_allowed() {
    let cmd = vec!["eslint".to_string(), "src/".to_string()];
    assert!(check_cmd_safety(&cmd).is_ok());
}

#[test]
fn prettier_check_allowed() {
    let cmd = vec!["prettier".to_string(), "--check".to_string(), "src/".to_string()];
    assert!(check_cmd_safety(&cmd).is_ok());
}

#[test]
fn npm_install_allowed() {
    let cmd = vec!["npm".to_string(), "install".to_string()];
    assert!(check_cmd_safety(&cmd).is_ok());
}

#[test]
fn npm_audit_allowed() {
    let cmd = vec!["npm".to_string(), "audit".to_string()];
    assert!(check_cmd_safety(&cmd).is_ok());
}

#[test]
fn npm_run_blocked() {
    // npm run は package.json の任意スクリプトを実行できるため禁止
    let cmd = vec!["npm".to_string(), "run".to_string(), "build".to_string()];
    assert!(check_cmd_safety(&cmd).is_err());
}

#[test]
fn npm_exec_blocked() {
    let cmd = vec!["npm".to_string(), "exec".to_string(), "vite".to_string()];
    assert!(check_cmd_safety(&cmd).is_err());
}

#[test]
fn npm_test_blocked() {
    let cmd = vec!["npm".to_string(), "test".to_string()];
    assert!(check_cmd_safety(&cmd).is_err());
}

#[test]
fn node_blocked() {
    // node は JS スクリプトを直接実行できるため allowlist 外
    let cmd = vec!["node".to_string(), "index.js".to_string()];
    assert!(check_cmd_safety(&cmd).is_err());
}

#[test]
fn npx_blocked() {
    // npx は任意パッケージを実行できるため allowlist 外
    let cmd = vec!["npx".to_string(), "vite".to_string(), "build".to_string()];
    assert!(check_cmd_safety(&cmd).is_err());
}
