use crate::command::{AiCommand, parse_commands};

pub(super) const SCHEMA_HINT: &str = r#"【正しいJSON形式の例】
単一コマンド:
```json
{"type": "read_file", "path": "src/main.rs"}
```
複数コマンド（配列）:
```json
[
  {"type": "txt", "content": "作業を開始します"},
  {"type": "cmd", "name": "ビルド", "cmd": ["cargo", "build"], "workdir": ".", "timeout": 60}
]
```
使えるtypeの一覧:
  read_file / list_dir / grep / glob / file / edit / patch / mkdir / delete_file
  cmd / txt / read_log / ask_user / todo_write / multi_edit / web_fetch
  enter_worktree / exit_worktree / bot / error
必須フィールド:
  read_file: path  ※ offset_lines(省略可) で続きを読める
  list_dir:  path
  file:      path, content
  patch:     path, diff
  mkdir:     path
  delete_file: path
  cmd:       name, cmd(配列), timeout(必須・秒数)
  txt:       content
  read_log:  filename(cmd_log/ai_log/browser_log)
  bot:       message
JSONの後に文章を続けず、コードブロック(```json ... ```)で出力してください。"#;

/// JSON文字列値内の生の制御文字をエスケープして修復を試みる
fn sanitize_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 64);
    let mut in_string = false;
    let mut escaped = false;
    for ch in s.chars() {
        if escaped {
            out.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_string => {
                out.push(ch);
                escaped = true;
            }
            '"' => {
                in_string = !in_string;
                out.push(ch);
            }
            '\n' if in_string => out.push_str("\\n"),
            '\r' if in_string => out.push_str("\\r"),
            '\t' if in_string => out.push_str("\\t"),
            '\x08' if in_string => out.push_str("\\b"),
            '\x0c' if in_string => out.push_str("\\f"),
            c if in_string && (c as u32) < 0x20 => {
                // その他の制御文字 (U+0000–U+001F) を \uXXXX へ変換
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            _ => out.push(ch),
        }
    }
    out
}

pub(super) fn parse_blocks(blocks: &[String]) -> (Vec<AiCommand>, Vec<String>) {
    let mut commands = Vec::new();
    let mut errors = Vec::new();
    for b in blocks {
        // まずそのままパース、失敗したらサニタイズして再試行
        let result = parse_commands(b).or_else(|_| parse_commands(&sanitize_json(b)));
        match result {
            Ok(cmds) => commands.extend(cmds),
            Err(e) => errors.push(format!(
                "JSONパースエラー: {e}\n\n元のブロック:\n```\n{b}\n```\n\n{SCHEMA_HINT}"
            )),
        }
    }
    (commands, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_blocks_accepts_logged_array_response() {
        let block = r#"[
  {
    "type": "txt",
    "content": "警告を減らしてコードをきれいにします。"
  },
  {
    "type": "cmd",
    "name": "cargo check",
    "cmd": ["cargo", "check"],
    "workdir": ".",
    "timeout": 120
  }
]"#;

        let (commands, errors) = parse_blocks(&[block.to_string()]);

        assert!(errors.is_empty(), "{errors:#?}");
        assert_eq!(commands.len(), 2);
        assert!(matches!(commands[0], AiCommand::Txt { .. }));
        assert!(matches!(commands[1], AiCommand::Cmd { .. }));
    }

    #[test]
    fn parse_blocks_repairs_raw_newline_inside_string() {
        let block = "{ \"type\": \"txt\", \"content\": \"line1\nline2\" }";

        let (commands, errors) = parse_blocks(&[block.to_string()]);

        assert!(errors.is_empty(), "{errors:#?}");
        assert_eq!(commands.len(), 1);
        assert!(matches!(commands[0], AiCommand::Txt { .. }));
    }
}
