use crate::executor::ToolResult;

/// タスク途中でユーザーに質問して回答を受け取る
///
/// AI が作業方針を確認したいとき、bot で完了する代わりに
/// ask_user で中間確認を入れられる。
///
/// 回答はそのまま ToolResult.output に入り、
/// 次のターンのプロンプトに [ツール実行結果] として組み込まれる。
pub async fn handle(question: &str, hint: &Option<String>) -> ToolResult {
    use std::io::Write as _;

    // 質問をユーザーに表示
    println!();
    println!(
        "{}[AI 質問]{} {question}",
        crate::color::CYAN_BOLD,
        crate::color::RESET
    );
    if let Some(h) = hint {
        println!("{}  ヒント: {h}{}", crate::color::DIM, crate::color::RESET);
    }
    print!("{}回答 > {}", crate::color::BOLD, crate::color::RESET);
    std::io::stdout().flush().ok();

    // spawn_blocking + 60秒タイムアウト
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        tokio::task::spawn_blocking(|| {
            let mut buf = String::new();
            std::io::stdin().read_line(&mut buf).ok();
            buf.trim().to_string()
        }),
    )
    .await;

    println!(); // 回答後に空行

    // §5 llm-prompts.md 準拠: Claude Code と同じ定型文を使う
    let output = match result {
        Ok(Ok(s)) if !s.is_empty() => s,
        Ok(Ok(_)) => "[non-interactive: no user input available]".to_string(),
        Ok(Err(_)) => "[non-interactive: no user input available]".to_string(),
        Err(_) => "[timeout: no response]".to_string(),
    };

    ToolResult::new("AskUser", output)
}
