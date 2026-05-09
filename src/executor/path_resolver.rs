use std::path::{Path, PathBuf};

/// resolve() を executor/mod.rs から分離したモジュール
/// パス検証ロジックは元の実装をそのまま移植
pub fn resolve_path(root: &Path, raw: &str) -> Result<PathBuf, String> {
    let raw_path = Path::new(raw);

    if raw_path.is_absolute() {
        return Err(format!("アクセス拒否: 絶対パス '{raw}' は使えません"));
    }
    if raw_path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!("アクセス拒否: '..' を含むパス '{raw}' は使えません"));
    }

    let root_canonical = root
        .canonicalize()
        .map_err(|e| format!("root の解決に失敗: {e}"))?;
    let joined = root_canonical.join(raw_path);

    if joined.exists() {
        let canonical = joined
            .canonicalize()
            .map_err(|e| format!("パスの解決に失敗: {e}"))?;
        if !canonical.starts_with(&root_canonical) {
            return Err(format!(
                "アクセス拒否: '{raw}' はプロジェクトルート外を指しています（シンボリックリンク経由の可能性）"
            ));
        }
        return Ok(canonical);
    }

    // 新規パス: 既存の最近祖先を canonicalize してルート内か確認
    let ancestor = {
        let mut cur = joined
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| joined.clone());
        loop {
            match cur.canonicalize() {
                Ok(c) => break c,
                Err(_) => match cur.parent() {
                    Some(p) => cur = p.to_path_buf(),
                    None => break root_canonical.clone(),
                },
            }
        }
    };

    if !ancestor.starts_with(&root_canonical) {
        return Err(format!("アクセス拒否: '{raw}' はプロジェクトルート外です"));
    }

    Ok(joined)
}
