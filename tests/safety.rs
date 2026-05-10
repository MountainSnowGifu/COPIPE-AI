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
