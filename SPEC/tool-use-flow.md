# Tool Use Control Flow in `agent-loop.mjs`

対象箇所: [`v2/src/core/agent-loop.mjs`](../v2/src/core/agent-loop.mjs)

`v2/src/core/agent-loop.mjs` の `tool_use` 周辺は、LLM が「このツールを使いたい」と返してきた content block を、実際のローカルツール実行に変換する制御フローです。

全体像は次の通りです。

```text
ユーザー入力
  -> LLM API 呼び出し
  -> assistant response を受け取る
  -> response.content を走査
  -> tool_use block を集める
  -> hook check
  -> permission check
  -> tools.call()
  -> tool_result を会話履歴に追加
  -> LLM に続きを聞く
```

## 1. LLM にツール定義を渡す

LLM API を呼ぶ時点で、登録済みツール一覧を渡しています。

```js
response = await callApiStreaming(provider, model, state, tools.list(), settings);
```

`tools.list()` は `v2/src/tools/registry.mjs` 側で、各ツールを次の形に変換します。

```js
{
  name,
  description,
  input_schema
}
```

これにより LLM は「どんな名前のツールがあり、どんな入力 JSON を受け取るか」を知ります。

## 2. API 応答を受け取る

streaming の場合、SSE event を読みながら text delta や thinking delta を UI 側に流します。

最後に accumulated response を作ります。

```js
response = response.accumulated;
```

accumulated response は、おおむね次のような形になります。

```js
{
  content: [
    { type: 'text', text: '...' },
    {
      type: 'tool_use',
      id: 'toolu_...',
      name: 'Read',
      input: { file_path: '/path/to/file' }
    }
  ],
  stop_reason: 'tool_use',
  usage: {}
}
```

streaming 中に `tool_use.input` が JSON fragment として来る場合は、文字列として貯めて、`content_block_stop` 時に `JSON.parse()` します。

## 3. assistant message を履歴に積む

API 応答後、assistant の返答全体を履歴に追加します。

```js
const assistantMessage = { role: 'assistant', content: response.content };
state.messages.push(assistantMessage);
```

ここでは `tool_use` も assistant message の一部として保存されます。後で `tool_result` と対応づけるためです。

## 4. content block を走査して `tool_use` を集める

中心になる処理は次の部分です。

```js
const toolUseBlocks = [];

for (const block of response.content || []) {
    if (block.type === 'text') {
        yield { type: 'assistant', content: block.text };
    }

    if (block.type === 'thinking') {
        yield { type: 'thinking_complete', thinking: block.thinking };
    }

    if (block.type === 'tool_use') {
        toolUseBlocks.push(block);
    }
}
```

ここではすぐに実行せず、まず `toolUseBlocks` に集めます。LLM 応答には text / thinking / tool_use が混在し得るため、テキストは UI に流し、tool_use は後段でまとめて処理します。

## 5. `tool_use` があればツール実行フェーズへ

```js
if (toolUseBlocks.length > 0) {
    const toolResults = [];

    for (const block of toolUseBlocks) {
        // ...
    }
}
```

`block` は例えば次のような値です。

```js
{
  type: 'tool_use',
  id: 'toolu_123',
  name: 'Bash',
  input: {
    command: 'ls'
  }
}
```

重要なのは `id`, `name`, `input` です。

- `id`: 後で `tool_result.tool_use_id` として対応づける
- `name`: `Bash`, `Read`, `Edit` などのツール名
- `input`: ツールに渡す JSON 引数

## 6. PreToolUse hook を実行

```js
const hookResult = await hooks.runPreToolUse(block.name, block.input);
if (!hookResult.allow) {
    // ...
    continue;
}
```

hook が拒否した場合、実ツールは呼ばれません。代わりに `tool_result` として「Blocked by hook」と積みます。

```js
toolResults.push({
    type: 'tool_result',
    tool_use_id: block.id,
    content: `Blocked by hook: ${hookResult.message}`,
});
```

拒否された場合でも、LLM には「そのツール呼び出しはこういう結果だった」と返す必要があります。assistant の `tool_use` に対応する `tool_result` が必要だからです。

## 7. Permission check を実行

```js
const allowed = await permissions.check(block.name, block.input);
if (!allowed) {
    // ...
    continue;
}
```

例えば `Bash` の危険コマンド検出、`Edit` / `Write` / `Read` のパス検証、permission mode による制限は `v2/src/permissions/checker.mjs` 側で行われます。

拒否された場合も実ツールは呼ばず、次のような結果を返します。

```js
{
  type: 'tool_result',
  tool_use_id: block.id,
  content: 'Permission denied'
}
```

## 8. 実ツールを呼ぶ

ここが実行の本丸です。

```js
yield { type: 'tool_progress', tool: block.name, status: 'running' };

let result;
try {
    result = await tools.call(block.name, block.input);
} catch (err) {
    result = `Tool error: ${err.message}`;
}
```

`tools.call()` の実体は `v2/src/tools/registry.mjs` です。

```js
async call(name, input) {
    const tool = tools.get(name);
    if (!tool) throw new Error(`Unknown tool: ${name}`);
    const errors = tool.validateInput?.(input) || [];
    if (errors.length > 0) return `Validation error: ${errors.join(', ')}`;
    return tool.call(input);
}
```

流れは次の通りです。

```text
tool name で Map から探す
  -> validateInput(input)
  -> 問題なければ tool.call(input)
  -> 結果を返す
```

## 9. PostToolUse hook を実行

ツール実行後、結果を hook で加工できます。

```js
if (hooks) {
    result = await hooks.runPostToolUse(block.name, result);
}
```

post hook は「結果を観察する」だけでなく、戻り値を書き換える可能性があります。

## 10. UI / 呼び出し元へ result event を yield

```js
yield { type: 'result', tool: block.name, result };
```

これは内部会話履歴用ではなく、外側の CLI / UI が「ツール実行結果」を表示・処理するためのイベントです。

## 11. LLM に返す `tool_result` を作る

次に、LLM API に返すための `tool_result` block を作ります。

```js
toolResults.push({
    type: 'tool_result',
    tool_use_id: block.id,
    content: typeof result === 'string' ? result : JSON.stringify(result),
});
```

ここで `tool_use_id: block.id` が重要です。assistant が出した `tool_use.id` と、user message 側の `tool_result.tool_use_id` を対応させています。

イメージは次の通りです。

```js
// assistant message
{
  role: 'assistant',
  content: [
    {
      type: 'tool_use',
      id: 'toolu_abc',
      name: 'Read',
      input: { file_path: '/tmp/a.txt' }
    }
  ]
}

// 次の user message
{
  role: 'user',
  content: [
    {
      type: 'tool_result',
      tool_use_id: 'toolu_abc',
      content: '1\tHello'
    }
  ]
}
```

## 12. `tool_result` を user message として履歴に追加

全 `tool_use` の処理が終わったら、結果をまとめて user message として追加します。

```js
state.messages.push({ role: 'user', content: toolResults });
```

Anthropic Messages API では、tool result は「ユーザー側から返される情報」として次リクエストに含める形になります。

## 13. 再帰的に agent loop を続ける

最後に agent loop を継続します。

```js
yield* run(null, { continuation: true });
return;
```

`continuation: true` なので、新しい user message は追加されません。すでに `tool_result` を追加済みなので、その状態で LLM に再問い合わせします。

流れは次のようになります。

```text
LLM: Read を使って
ローカル: Read 実行
ローカル: 結果を履歴へ追加
LLM: 結果を読んで次の返答、または次の tool_use
```

## まとめ

この制御フローの核心は、`tool_use` 検出そのものよりも、その後の実行パイプラインです。

```text
hook
permission
registry validation
tool.call
post hook
tool_result 化
recursive continuation
```

LLM の `tool_use` をそのまま信用して直接実行するのではなく、hook、permission、registry validation を通してからローカル実行し、その結果を `tool_result` として会話履歴へ戻します。

一言でいうと、`agent-loop.mjs` は **LLM の tool_use とローカルツール実行をつなぐ司令塔**です。
