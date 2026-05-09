use crate::command::{parse_commands, AiCommand};

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
  read_file / list_dir / file / patch / mkdir / delete_file
  cmd / txt / read_log / bot / error
必須フィールド:
  read_file: path  ※ offset_lines(省略可) で続きを読める
  list_dir:  path
  file:      path, content
  patch:     path, diff
  mkdir:     path
  delete_file: path
  cmd:       name, cmd(配列), timeout(必須・秒数)
  txt:       content
  read_log:  filename(cmd_log/ai_log/ai_readonly)
  bot:       message
JSONの後に文章を続けず、コードブロック(```json ... ```)で出力してください。"#;

/// JSON文字列値内の生の改行・タブをエスケープして修復を試みる
fn sanitize_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_string = false;
    let mut escaped = false;
    for ch in s.chars() {
        if escaped {
            out.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_string => { out.push(ch); escaped = true; }
            '"' => { in_string = !in_string; out.push(ch); }
            '\n' if in_string => out.push_str("\\n"),
            '\r' if in_string => out.push_str("\\r"),
            '\t' if in_string => out.push_str("\\t"),
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
        let result = parse_commands(b)
            .or_else(|_| parse_commands(&sanitize_json(b)));
        match result {
            Ok(cmds) => commands.extend(cmds),
            Err(e) => errors.push(format!(
                "JSONパースエラー: {e}\n\n元のブロック:\n```\n{b}\n```\n\n{SCHEMA_HINT}"
            )),
        }
    }
    (commands, errors)
}
