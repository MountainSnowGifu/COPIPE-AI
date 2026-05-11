# core/ モジュール 詳細解説

対象ファイル：

- [`v2/src/core/agent-loop.mjs`](../v2/src/core/agent-loop.mjs)
- [`v2/src/core/streaming.mjs`](../v2/src/core/streaming.mjs)
- [`v2/src/core/providers.mjs`](../v2/src/core/providers.mjs)
- [`v2/src/core/rate-limiter.mjs`](../v2/src/core/rate-limiter.mjs)
- [`v2/src/core/context-manager.mjs`](../v2/src/core/context-manager.mjs)
- [`v2/src/core/system-prompt.mjs`](../v2/src/core/system-prompt.mjs)
- [`v2/src/core/session.mjs`](../v2/src/core/session.mjs)
- [`v2/src/core/checkpoints.mjs`](../v2/src/core/checkpoints.mjs)
- [`v2/src/core/cache.mjs`](../v2/src/core/cache.mjs)

---

## 1. `agent-loop.mjs` — 全体の司令塔

`createAgentLoop()` が呼び出されると、**クロージャ内に会話の全状態を保持し**、`run()` という async generator を返します。

### 初期化フェーズ（1回だけ）

```
createAgentLoop({ model, tools, permissions, settings, hooks })
    │
    ├─ ContextManager を作る（最大トークン数を渡す）
    ├─ buildSystemPrompt() でシステムプロンプトを構築
    └─ state オブジェクトを作る
         {
           messages: [],        ← 会話履歴（全ターン共有）
           systemPrompt: "...", ← 固定
           turnCount: 0,
           tokenUsage: { input:0, output:0 },
           model,
           tools,
           _contextManager,
         }
```

`state` はクロージャに閉じ込められ、ターンをまたいで共有されます。

---

### `run()` の1ターンの流れ（詳細）

```
run(userMessage, { continuation: false })
 │
 ├─ [1] メッセージ追加
 │       continuation=false のときだけ
 │       state.messages に { role:'user', content:userMessage } を push
 │       contextManager.addMessage() 経由（自動圧縮チェック込み）
 │       turnCount++
 │
 ├─ [2] maxTurns チェック
 │       settings.maxTurns 超えたら error + stop を yield して終了
 │
 ├─ [3] 自動コンテキスト圧縮
 │       contextManager.shouldCompact(state.messages) → true なら
 │       yield { type: 'compaction' }
 │       state.messages = contextManager.compact(...)
 │
 ├─ [4] yield { type: 'stream_request_start' }
 │       UI 側がスピナー等を開始するためのシグナル
 │
 ├─ [5] API 呼び出し
 │       detectProvider(model) でプロバイダ判定
 │       streaming なら callApiStreaming() → SSE イベントを逐次 yield
 │       非streaming なら callApi() → JSON レスポンスを待つ
 │
 ├─ [6] トークン使用量を state.tokenUsage に加算
 │
 ├─ [7] assistant message を state.messages に push
 │       (tool_use も含む — 次の tool_result と対応させるため必須)
 │
 ├─ [8] content blocks を走査
 │       text     → yield { type: 'assistant', content: text }
 │       thinking → yield { type: 'thinking_complete', thinking }
 │       tool_use → toolUseBlocks[] に蓄積（まだ実行しない）
 │
 ├─ [9] tool_use があれば実行フェーズ
 │   │
 │   ├─ 各 block について:
 │   │    hooks.runPreToolUse() → deny なら tool_result に "Blocked by hook" を積む
 │   │    permissions.check() → false なら "Permission denied" を積む
 │   │    tools.call() → 実行 → エラーなら "Tool error: ..."
 │   │    hooks.runPostToolUse() → result を書き換え可能
 │   │    yield { type: 'result', tool, result }
 │   │    toolResults に { type:'tool_result', tool_use_id, content } を積む
 │   │
 │   ├─ state.messages.push({ role:'user', content: toolResults })
 │   │    (Anthropic API 仕様: tool_result はユーザー側メッセージとして送る)
 │   │
 │   └─ yield* run(null, { continuation: true })  ← 再帰
 │        ↑ ここで次のターンへ。tool_result を送って続きを聞く
 │
 └─ [10] tool_use なし → Stop フック実行
          hooks.runStop() が false を返したら:
            "[System: A hook prevented stopping...]" を追加して再帰継続
          true なら:
            yield { type: 'stop', reason: stop_reason }  ← 正常終了
```

**重要な設計：再帰 generator**
`yield* run(null, { continuation: true })` はツール結果を送って続きを得る "内側のループ" を外側の generator に **そのままスルー** させます。呼び出し元は1つの `for await` ループで全ターン分のイベントを受け取れます。

---

## 2. `streaming.mjs` — SSE の解析と組み立て

Anthropic のストリーミングAPIは HTTP SSE（Server-Sent Events）形式で返します。

### SSE の生データ例

```
event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}
```

### `streamResponse()` — SSE バイトストリームをイベントに変換

```
ReadableStream (fetch response.body)
    → TextDecoder でバイト → 文字列
    → '\n\n' で区切ってSSEチャンクに分割
    → parseSSEChunk() で event: / data: 行を解析
    → { type, ...data } オブジェクトを yield
```

`parseSSEChunk()` の処理：

- `event: ping` → `{ type: 'ping' }` を返す（中身なし）
- `data: [DONE]` → `{ type: 'done' }` （OpenAI互換）
- それ以外 → `JSON.parse(data)` してオブジェクトに変換

### `accumulateStream()` — イベントを完全なメッセージに組み立て

ストリーミングと非ストリーミングの **出力形式を統一** するための関数。

| イベント              | 処理                                                                     |
| --------------------- | ------------------------------------------------------------------------ |
| `message_start`       | id, model, input*tokens, cache*\*\_tokens を記録                         |
| `content_block_start` | 新ブロック作成（type により初期フィールドが違う）                        |
| `content_block_delta` | `text_delta` → .text append / `input_json_delta` → .input(文字列) append |
| `content_block_stop`  | tool_use の場合: `JSON.parse(input文字列)` でオブジェクトに変換          |
| `message_delta`       | stop_reason, output_tokens を記録                                        |
| `error`               | `throw new Error(...)`                                                   |

`tool_use.input` が最初 `""` で始まり、`input_json_delta` で断片が追加され、`content_block_stop` で `JSON.parse` されるのがポイントです。JSONが途中で届いても壊れないよう文字列で蓄積する設計になっています。

---

## 3. `providers.mjs` + `agent-loop.mjs` — マルチプロバイダー変換

### プロバイダー自動検出

```js
function detectProvider(model) {
  if (
    model.startsWith("gpt-") ||
    model.startsWith("o1") ||
    model.startsWith("o3")
  )
    return "openai";
  if (model.startsWith("gemini")) return "google";
  return "anthropic"; // デフォルト
}
```

### Anthropic リクエスト構造

```js
{
  model: "claude-sonnet-4-6",
  max_tokens: 16384,
  messages: state.messages,
  system: state.systemPrompt,
  tools: toolDefs,
  stream: true,
  // opus または settings.thinking=true のとき:
  thinking: { type: 'enabled', budget_tokens: 10000 }
}
```

### OpenAI 変換（`callOpenAI`）

Anthropic 形式の `state.messages` を OpenAI 形式にリアルタイム変換：

| Anthropic                          | OpenAI                                   |
| ---------------------------------- | ---------------------------------------- |
| `{ role:'user', content:'...' }`   | `{ role:'user', content:'...' }`         |
| `{ role:'assistant', content:[] }` | テキストブロックのみ展開                 |
| `tool_result` blocks               | `{ role:'tool', tool_call_id, content }` |

ツール定義の変換：

```
Anthropic: { name, description, input_schema }
OpenAI:    { type:'function', function:{ name, description, parameters } }
```

レスポンスは `convertOpenAIResponse()` で Anthropic 形式に逆変換：

```js
choices[0].message.tool_calls[i]
  → { type:'tool_use', id, name, input: JSON.parse(arguments) }
```

### Google (Gemini) 変換（`callGoogle`）

```
system prompt  →  systemInstruction: { parts: [{ text }] }
messages       →  contents: [{ role:'model'|'user', parts:[{text}] }]
```

Google は `assistant` ではなく `model` という role 名を使います。

---

## 4. `rate-limiter.mjs` — 429/529 のリトライ制御

### 対象HTTPステータス

| コード | 意味                             | 対処                                       |
| ------ | -------------------------------- | ------------------------------------------ |
| `429`  | Rate Limited（リクエスト過多）   | `Retry-After` ヘッダーを読んで指定秒数待機 |
| `529`  | API Overloaded（サーバー過負荷） | 指数バックオフ＋ランダムジッター           |

### バックオフ計算式

```js
exponential = baseDelay(1000ms) × 2^retryCount
jitter      = Math.random() × baseDelay(1000ms)
delay       = min(exponential + jitter, maxDelay(60000ms))
```

| retryCount | 指数部   | jitter | 最大待ち時間 |
| ---------- | -------- | ------ | ------------ |
| 0          | 1秒      | 0〜1秒 | 2秒          |
| 1          | 2秒      | 0〜1秒 | 3秒          |
| 2          | 4秒      | 0〜1秒 | 5秒          |
| 3          | 8秒      | 0〜1秒 | 9秒          |
| 4          | 16秒     | 0〜1秒 | 17秒         |
| 5          | → `fail` | —      | —            |

成功したら `retryCount` をリセット。`handleResponse()` は `'ok'` / `'retry'` / `'fail'` の3値を返す。

---

## 5. `context-manager.mjs` — コンテキスト圧縮エンジン

### トークン数推定

```js
const CHARS_PER_TOKEN = 4; // 英語テキストの近似値

推定トークン数 = sum(メッセージごとの文字数) / 4;
```

各ブロック型ごとの文字数計上：

| block.type    | 文字数の取得元                            |
| ------------- | ----------------------------------------- |
| `text`        | `block.text.length`                       |
| `tool_result` | `block.content.length`                    |
| `tool_use`    | `JSON.stringify(block.input).length + 20` |
| `thinking`    | `block.thinking.length`                   |
| role ヘッダー | +16文字（固定）                           |

### 圧縮の2段階

**マイクロ圧縮**（5ターン以上前の `tool_result` だけ切り詰め）：

```
古い tool_result（200文字超）
    → 先頭100文字 + "...[truncated]"
```

会話の流れは残しつつ、verbose なツール出力だけ削ります。これだけで閾値を下回れば終了（フル圧縮しない）。

**フル圧縮**（マイクロ圧縮後もまだ80%超えの場合）：

```
全メッセージ（N個）
    ↓
最新6件 → そのまま保持
残り(N-6件) → 下記の要約1メッセージに置換

{ role: 'user', content:
  "[Context compacted — summary of N earlier messages]\n" +
  "user: 先頭200文字\n" +
  "assistant: [tool:Bash] [result:ls出力先頭80文字]\n" +
  ... （全体2000文字上限）
}
```

**なぜ `role: 'user'` か？**
Anthropic Messages API の制約として `user → assistant → user → ...` の交互構造が必要です。要約メッセージを `user` にすることで、次の `assistant` レスポンスとペアになれます。

---

## 6. `system-prompt.mjs` — CLAUDE.md 収集とキャッシュ分割

### CLAUDE.md の収集順序

```
1. ~/.claude/CLAUDE.md                ← グローバル（最も汎用的）
2. / → ... → 親ディレクトリ → cwd までの各ディレクトリ
     CLAUDE.md / .claude/CLAUDE.md  ← プロジェクトルートが先、ローカルが後
3. addDirs で追加指定したディレクトリの CLAUDE.md
```

`projectFiles.reverse()` で **親が先、近い方が後** になるよう順序を制御。後から読んだものが `\n\n` で連結されるため、より近いディレクトリの指示がコンテキストとして後に来ます。

### キャッシュ境界の分割

```
staticPrefix（キャッシュ対象）
  = "You are an AI coding assistant.\n\n"
  + CLAUDE.md_global + "\n\n"
  + CLAUDE.md_parent + "\n\n"
  + CLAUDE.md_project

dynamicSuffix（キャッシュ対象外）
  = "\n\nAvailable tools:\n"
  + "- Bash: Execute a bash command...\n"
  + "- Read: ...\n"
  + ...
```

**なぜ分割するか？**
ツール一覧はMCPサーバーの追加などでリクエストごとに変わりうるためキャッシュできません。CLAUDE.md の内容は変わらないのでキャッシュできます。

Anthropic APIに送る際の構造：

```json
[
  {
    "type": "text",
    "text": "<staticPrefix>",
    "cache_control": { "type": "ephemeral" }
  },
  {
    "type": "text",
    "text": "<dynamicSuffix>"
  }
]
```

---

## 7. `session.mjs` — セッションの保存・復元・テレポート

### 保存パス

```
~/.claude/projects/<SHA256(プロジェクトディレクトリ絶対パス)[:16]>/session.json
```

例：プロジェクトが `/home/user/myapp` なら  
`~/.claude/projects/a3f9c12d4b0e1f87/session.json`

### session.json の構造

```json
{
  "id": "sess_1746871234_a3b4",
  "conversationId": null,
  "projectDir": "/home/user/myapp",
  "startedAt": "2026-05-10T09:00:00.000Z",
  "savedAt": "2026-05-10T09:30:00.000Z",
  "model": "claude-sonnet-4-6",
  "turnCount": 12,
  "tokenUsage": { "input": 45000, "output": 8200 },
  "messages": ["..."],
  "systemPrompt": "..."
}
```

### テレポート機能

```js
exportForTeleport(state)
  → JSON化 → base64エンコード → 文字列を返す

importFromTeleport(base64string, state)
  → base64デコード → JSON.parse → state に復元
  → sessionId を "sess_teleport_<timestamp>" に変える
```

base64 文字列を別マシンや別セッションに渡すことで会話履歴を移植できます。

---

## 8. `checkpoints.mjs` — ファイル編集のUndo

### チェックポイントの保存構造

```
.claude/checkpoints/
  └─ ckpt_1746871234_a3b4.json
       {
         "id": "ckpt_...",
         "filePath": "/absolute/path/to/file.ts",
         "relativePath": "src/file.ts",
         "content": "元のファイル内容全体",
         "timestamp": "2026-05-10T09:15:00.000Z",
         "size": 2048
       }
```

### スタック管理

```js
this.history = []; // チェックポイントIDのスタック
this.maxCheckpoints = 50; // 超えたら古いものから削除
```

| 操作             | 内容                                                              |
| ---------------- | ----------------------------------------------------------------- |
| `save(filePath)` | ファイル内容を読んでJSONに書く → IDを `history.push()`            |
| `undo()`         | `history.pop()` → JSONを読んでファイルを復元 → JSONファイルを削除 |

50件を超えると `history.shift()` で最古のIDを取り出し、そのファイルを削除。

---

## 9. `cache.mjs` — prompt cache_control の統計管理

`PromptCache` クラス自体は **キャッシュの実体を持ちません**。Anthropic のサーバー側がキャッシュを保持し、このクラスは **APIレスポンスの使用量から統計を集計** するだけです。

### `applyCacheControl()` の動作

```js
// 文字列 → 1ブロックにして ephemeral を付ける
"You are..." → [{ type:'text', text:'You are...', cache_control:{type:'ephemeral'} }]

// 配列 → インデックス0番（通常CLAUDE.md相当）にだけ付ける
[blockA, blockB] →
  [{ ...blockA, cache_control:{type:'ephemeral'} }, blockB]
```

### 統計の追跡

| APIレスポンスの使用量フィールド   | 意味                              | 統計            |
| --------------------------------- | --------------------------------- | --------------- |
| `cache_creation_input_tokens > 0` | 初回：キャッシュに書き込まれた    | `cacheMisses++` |
| `cache_read_input_tokens > 0`     | 2回目以降：キャッシュから読まれた | `cacheHits++`   |

```
ヒット率     = cacheHits / totalRequests × 100%
節約トークン = cacheReadTokens（読まれたトークンは課金されない）
```

---

## 全体の依存・データフロー

```
createAgentLoop()
    │
    ├─[起動時]─ buildSystemPrompt()
    │             CLAUDE.md収集 → staticPrefix + dynamicSuffix
    │             state.systemPrompt にセット
    │
    └─[毎ターン]─ run()
                   │
                   ├─ contextManager.addMessage()
                   │    shouldCompact → compact()
                   │       microCompact → [truncated]
                   │       fullCompact → [Context compacted...]
                   │
                   ├─ callAnthropic() / callOpenAI() / callGoogle()
                   │    Anthropic: そのまま送信
                   │    OpenAI:    messages・tools を変換して送信
                   │    Google:    contents・systemInstruction に変換して送信
                   │
                   ├─ streamResponse()
                   │    バイトストリーム → SSEチャンク → { type, ...data }
                   │
                   ├─ accumulateStream()
                   │    content_block_start/delta/stop を追跡
                   │    tool_use.input は文字列蓄積 → JSON.parse
                   │
                   ├─ RateLimiter.handleResponse()
                   │    429 → Retry-After 待機
                   │    529 → 指数バックオフ+ジッター
                   │
                   ├─ ツール実行（各 tool_use block ごと）
                   │    PreToolUse hook → permissions.check → tools.call → PostToolUse hook
                   │
                   ├─ state.messages.push(tool_results)
                   │
                   └─ yield* run(null, { continuation:true })
                               ← 再帰でツール結果を送って続ける
```
