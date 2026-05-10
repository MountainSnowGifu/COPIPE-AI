# LLMに投げる文言まとめ

`v2/src/` 内でAPIリクエストに乗る・または会話履歴に挿入されるテキストを網羅的に整理。

---

## 1. システムプロンプト — `core/system-prompt.mjs`

### ベーステキスト（固定）

```
You are an AI coding assistant.
```

### CLAUDE.md の結合（静的プレフィックス）

以下の順でファイルを読み込み `\n\n` で連結。**Anthropic prompt cacheの対象**。

| 優先順 | パス                                                                 |
| ------ | -------------------------------------------------------------------- |
| 1      | `~/.claude/CLAUDE.md`（グローバル）                                  |
| 2      | プロジェクトルートから親へ遡った各 `CLAUDE.md` / `.claude/CLAUDE.md` |
| 3      | `addDirs` オプションで追加したディレクトリの `CLAUDE.md`             |

### 動的サフィックス（キャッシュ対象外）

```
\n\nAvailable tools:\n
- {ToolName}: {description 先頭100文字}
- ...
```

### cache_control ブロック構造

```json
[
  {
    "type": "text",
    "text": "<staticPrefix>",
    "cache_control": { "type": "ephemeral" }
  },
  { "type": "text", "text": "<dynamicSuffix>" }
]
```

---

## 2. コンテキスト圧縮メッセージ — `core/context-manager.mjs`

使用トークンがウィンドウの **80%** を超えたときに自動挿入される合成ユーザーメッセージ。

### マイクロ圧縮（5ターン以上前のツール結果を切り詰め）

```
...[truncated]
```

（200文字超の `tool_result` の末尾をこれに置換）

### フル圧縮（要約サマリーメッセージ）

```
[Context compacted — summary of {N} earlier messages]
user: {古いメッセージ先頭200文字}
assistant: [tool:ToolName] [result:結果先頭80文字]
...
```

- 最新6メッセージ（約3ターン）は保持
- サマリー全体の上限は2000文字

---

## 3. ツール実行結果としてLLMに返るテキスト — `core/agent-loop.mjs`

| 状況                   | LLMに返る `tool_result` の内容          |
| ---------------------- | --------------------------------------- |
| フックにブロックされた | `Blocked by hook: {hookResult.message}` |
| パーミッション拒否     | `Permission denied`                     |
| ツール実行エラー       | `Tool error: {err.message}`             |

---

## 4. ユーザーへの許可確認プロンプト — `permissions/prompt.mjs`

LLMではなく**ユーザー**に表示するテキスト（ツール呼び出し前のインタラクティブ確認）。

```
Allow {ToolSummary}? [y/N]
```

`{ToolSummary}` の例：
| ツール | 表示例 |
|--------|--------|
| `Bash` | `Bash: npm install ...` |
| `Edit` | `Edit: src/index.mjs` |
| `Write` | `Write: dist/out.js (1234 chars)` |
| `MultiEdit` | `MultiEdit: foo.ts (3 edits)` |
| `Agent` | `Agent: Fix the bug in...` |
| `WebFetch` | `WebFetch: https://example.com/...` |

許可不要（常に許可）なツール: `Read`, `Glob`, `Grep`, `Ls`, `ToolSearch`, `AskUser`, `CronList`, `TodoWrite`

---

## 5. AskUser ツール — `tools/ask-user.mjs`

ターミナルに出力するプロンプト文言。

```
? {question}
>
```

非インタラクティブ環境またはタイムアウト時の返答（LLMへの `tool_result` に入る）:

```
[non-interactive: no user input available]
[timeout: no response]
```

---

## 6. WebSearch ツール フォールバック — `tools/web-search.mjs`

APIキー未設定時にLLMへ返る文言:

```
No search API configured. Set BRAVE_API_KEY or SEARXNG_URL environment variable.
Alternatively, use the WebFetch tool to fetch specific URLs directly.
```

---

## 全体フロー図

```
ユーザー入力
    │
    ▼
[messages 配列に追加]  ←── 必要なら Context Compaction (要約メッセージ挿入)
    │
    ▼
API リクエスト
    ├─ system: staticPrefix (cache) + dynamicSuffix (ツール一覧)
    └─ messages: 会話履歴 + 最新ユーザーメッセージ
    │
    ▼
Claude レスポンス
    ├─ text block → UI表示
    ├─ thinking block → UI表示
    └─ tool_use block → ツール実行 → tool_result を messages に追加 → 再度API呼び出し
```
