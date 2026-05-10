/// エラー文言の種別分離（llm-prompts.md §3 準拠）
///
/// Claude Code と同じ3種類のプレフィックスを使うことで
/// AI が原因を正確に識別できるようにする。

/// セキュリティ・アクセス拒否（パストラバーサル・未読ガード・安全でないコマンド等）
pub fn perm_denied(detail: impl std::fmt::Display) -> String {
    format!("Permission denied: {detail}")
}

pub fn unread_file(path: &str) -> String {
    perm_denied(format!(
        "'{path}' はこのタスク内で未読です。前のタスクで読んだ内容は安全確認に使えません。\
        先に次を実行してから、同じ書き込みコマンドを再試行してください: \
        {{\"type\":\"read_file\",\"path\":\"{path}\"}}"
    ))
}

/// ツール実行エラー（IO失敗・コマンド失敗・diff適用失敗等）
pub fn tool_error(detail: impl std::fmt::Display) -> String {
    format!("Tool error: {detail}")
}

/// hook によるブロック（大ファイル・カスタムルール等）
pub fn blocked_by_hook(detail: impl std::fmt::Display) -> String {
    format!("Blocked by hook: {detail}")
}

/// エラー文字列がどの種別かを判定する（表示ロジック等で使用）
pub fn is_error_output(s: &str) -> bool {
    s.starts_with("Permission denied:")
        || s.starts_with("Tool error:")
        || s.starts_with("Blocked by hook:")
        || s.starts_with("ERROR:") // 移行期の旧形式
        || s.starts_with("BLOCKED:") // 移行期の旧形式
}
