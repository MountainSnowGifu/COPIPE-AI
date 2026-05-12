use std::path::{Path, PathBuf};

/// `canonicalize()` を呼び、Windows の `\\?\` 拡張パスプレフィックスを除去する。
///
/// `main.rs` の `canonicalize_clean` と同じ処理を共通化したもの。
/// `ctx.resolve()` 内で `canonicalize()` を呼ぶと Windows では `\\?\C:\...` 形式が
/// 返ることがあり、clean な root（`\\?\` なし）との `strip_prefix` が失敗する。
pub fn canonicalize_clean(path: &Path) -> std::io::Result<PathBuf> {
    let canonical = path.canonicalize()?;
    #[cfg(target_os = "windows")]
    {
        let s = canonical.to_string_lossy();
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            return Ok(PathBuf::from(stripped));
        }
    }
    Ok(canonical)
}

pub fn home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(profile));
    }

    let drive = std::env::var_os("HOMEDRIVE").filter(|v| !v.is_empty());
    let path = std::env::var_os("HOMEPATH").filter(|v| !v.is_empty());
    match (drive, path) {
        (Some(drive), Some(path)) => {
            let mut combined = PathBuf::from(drive);
            combined.push(path);
            Some(combined)
        }
        _ => None,
    }
}

pub fn local_data_dir() -> PathBuf {
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) {
        return PathBuf::from(local_app_data);
    }
    if let Some(home) = home_dir() {
        return if cfg!(target_os = "windows") {
            home.join("AppData").join("Local")
        } else {
            home.join(".local").join("share")
        };
    }
    std::env::temp_dir()
}

pub fn is_absolute_path_arg(arg: &str) -> bool {
    Path::new(arg).is_absolute() || has_windows_absolute_prefix(arg)
}

pub fn has_parent_component_arg(arg: &str) -> bool {
    Path::new(arg)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
        || arg.split(['/', '\\']).any(|part| part == "..")
}

fn has_windows_absolute_prefix(arg: &str) -> bool {
    let bytes = arg.as_bytes();
    if arg.starts_with("\\\\") || arg.starts_with("//") {
        return true;
    }
    bytes.len() >= 3
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
        && bytes[0].is_ascii_alphabetic()
}

#[cfg(test)]
mod tests {
    use super::is_absolute_path_arg;

    #[test]
    fn detects_windows_absolute_paths_on_all_targets() {
        assert!(is_absolute_path_arg(r"C:\Users\akira\file.txt"));
        assert!(is_absolute_path_arg("D:/work/file.txt"));
        assert!(is_absolute_path_arg(r"\\server\share\file.txt"));
    }

    #[test]
    fn leaves_relative_windows_like_paths_allowed() {
        assert!(!is_absolute_path_arg(r"dir\file.txt"));
        assert!(!is_absolute_path_arg(r"C:relative\file.txt"));
    }

    #[test]
    fn detects_parent_components_with_windows_separators_on_all_targets() {
        assert!(super::has_parent_component_arg(r"dir\..\secret.txt"));
        assert!(super::has_parent_component_arg("../secret.txt"));
        assert!(!super::has_parent_component_arg("dir/not..parent/file.txt"));
    }
}
