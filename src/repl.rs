// REPL ロジックを main.rs から切り出す準備用ファイル
// 今は空の骨組みを作成し、後で main.rs から処理を移植します。

pub struct ReplContext;

impl ReplContext {
    pub fn new() -> Self {
        Self {}
    }

    pub fn run(&mut self) {
        // TODO: main.rs の REPL ループをここへ移植
        println!("[repl] REPL モジュールが初期化されました");
    }
}
