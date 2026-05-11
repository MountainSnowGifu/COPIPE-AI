# Streaming Tool Use Assembly in `streaming.mjs`

対象箇所: [`v2/src/core/streaming.mjs`](../v2/src/core/streaming.mjs)

`v2/src/core/streaming.mjs` は、Anthropic Messages API の streaming response を読み取り、SSE event を JavaScript object に変換し、最終的な assistant message へ組み立てるためのファイルです。

特に `content_block_start` / `content_block_delta` / `content_block_stop` の処理が、streaming 応答内の `tool_use.input` JSON を復元する中心です。

## 全体像

```text
HTTP streaming response
  -> streamResponse(response)
      -> SSE chunk を読む
      -> event/data 行を parse
      -> JSON object を yield
  -> accumulateStream(events)
      -> message object を作る
      -> content block を順に組み立てる
      -> input_json_delta を結合する
      -> content_block_stop で JSON.parse()
  -> complete assistant message
```

最終的に、streaming response は non-streaming API response と近い shape に変換されます。

```js
{
  id: 'msg_...',
  role: 'assistant',
  content: [
    { type: 'text', text: '...' },
    {
      type: 'tool_use',
      id: 'toolu_...',
      name: 'Read',
      input: { file_path: '/path/to/file' }
    }
  ],
  model: '...',
  stop_reason: 'tool_use',
  usage: {
    input_tokens: 0,
    output_tokens: 0,
    cache_creation_input_tokens: 0,
    cache_read_input_tokens: 0
  }
}
```

## 1. `streamResponse(response)` が SSE を読む

最初の入口は `streamResponse()` です。

```js
export async function* streamResponse(response) {
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let buffer = '';

    try {
        while (true) {
            const { done, value } = await reader.read();
            if (done) break;

            buffer += decoder.decode(value, { stream: true });

            while (buffer.includes('\n\n')) {
                const idx = buffer.indexOf('\n\n');
                const chunk = buffer.slice(0, idx);
                buffer = buffer.slice(idx + 2);

                const event = parseSSEChunk(chunk);
                if (event) yield event;
            }
        }

        if (buffer.trim()) {
            const event = parseSSEChunk(buffer.trim());
            if (event) yield event;
        }
    } finally {
        reader.releaseLock();
    }
}
```

ここでは `response.body.getReader()` で HTTP stream を読みます。

SSE は通常、event ごとに空行で区切られます。

```text
event: content_block_delta
data: {"type":"content_block_delta", ...}

event: content_block_stop
data: {"type":"content_block_stop", ...}
```

そのため `buffer.includes('\n\n')` を見て、1 event 分の chunk を切り出しています。

```js
const idx = buffer.indexOf('\n\n');
const chunk = buffer.slice(0, idx);
buffer = buffer.slice(idx + 2);
```

切り出した chunk は `parseSSEChunk()` に渡され、parse できた event object が `yield` されます。

## 2. `parseSSEChunk(chunk)` が SSE chunk を object にする

SSE chunk の parse は `parseSSEChunk()` が担当します。

```js
function parseSSEChunk(chunk) {
    let eventType = null;
    let dataLines = [];

    for (const line of chunk.split('\n')) {
        if (line.startsWith('event: ')) {
            eventType = line.slice(7).trim();
        } else if (line.startsWith('data: ')) {
            dataLines.push(line.slice(6));
        } else if (line.startsWith(':')) {
            continue;
        }
    }

    if (eventType === 'ping') {
        return { type: 'ping' };
    }

    if (dataLines.length === 0) return null;

    const raw = dataLines.join('\n');
    if (raw === '[DONE]') return { type: 'done' };

    try {
        const data = JSON.parse(raw);
        return { type: eventType || data.type || 'unknown', ...data };
    } catch {
        return null;
    }
}
```

この関数は、SSE の `event:` 行と `data:` 行を分けて読みます。

```text
event: content_block_delta
data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"hello"}}
```

これが次のような object になります。

```js
{
  type: 'content_block_delta',
  delta: {
    type: 'text_delta',
    text: 'hello'
  }
}
```

### ping event

`ping` は data がないことがあるため、特別扱いされています。

```js
if (eventType === 'ping') {
    return { type: 'ping' };
}
```

### data JSON parse

`data:` 行は複数行の可能性があるので、`join('\n')` してから `JSON.parse()` します。

```js
const raw = dataLines.join('\n');
const data = JSON.parse(raw);
return { type: eventType || data.type || 'unknown', ...data };
```

戻り値には `type` が必ず入るようになっています。

## 3. `accumulateStream(events)` が message を作る

`accumulateStream()` は、`streamResponse()` が yield した event を受け取り、complete message に組み立てます。

```js
export async function accumulateStream(events) {
    const message = {
        id: null,
        role: 'assistant',
        content: [],
        model: null,
        stop_reason: null,
        usage: {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0
        },
    };

    let currentBlock = null;
    let blockIndex = -1;

    for await (const event of events) {
        switch (event.type) {
            // ...
        }
    }

    return message;
}
```

ここで重要なのは、streaming event が細切れに届くため、現在組み立て中の content block を `currentBlock` に保持していることです。

```js
let currentBlock = null;
let blockIndex = -1;
```

## 4. `message_start` で metadata と input token usage を拾う

```js
case 'message_start':
    if (event.message) {
        message.id = event.message.id;
        message.model = event.message.model;
        if (event.message.usage) {
            message.usage.input_tokens = event.message.usage.input_tokens || 0;
            message.usage.cache_creation_input_tokens = event.message.usage.cache_creation_input_tokens || 0;
            message.usage.cache_read_input_tokens = event.message.usage.cache_read_input_tokens || 0;
        }
    }
    break;
```

`message_start` では、message id、model、入力 token usage を保存します。

tool_use の組み立てには直接関係しませんが、最終 response の `usage` に必要です。

## 5. `content_block_start` で block を初期化する

質問の中心に近いのがここです。

```js
case 'content_block_start':
    blockIndex = event.index ?? message.content.length;
    currentBlock = { ...event.content_block };
    if (currentBlock.type === 'text') currentBlock.text = '';
    if (currentBlock.type === 'thinking') currentBlock.thinking = '';
    if (currentBlock.type === 'tool_use') {
        currentBlock.input = '';
    }
    message.content[blockIndex] = currentBlock;
    break;
```

`content_block_start` は、新しい content block が始まったことを表します。

例えば tool use の場合、最初に次のような event が来ます。

```js
{
  type: 'content_block_start',
  index: 1,
  content_block: {
    type: 'tool_use',
    id: 'toolu_123',
    name: 'Read',
    input: {}
  }
}
```

この event を受けて、`currentBlock` を作ります。

```js
currentBlock = { ...event.content_block };
```

そして block type ごとに、streaming delta を受けるための field を初期化します。

```text
text:
  currentBlock.text = ''

thinking:
  currentBlock.thinking = ''

tool_use:
  currentBlock.input = ''
```

tool_use の場合、ここで `input` を object ではなく **空文字列** にしているのが重要です。

```js
if (currentBlock.type === 'tool_use') {
    currentBlock.input = '';
}
```

なぜなら streaming では tool input JSON が完成済み object ではなく、`input_json_delta` として断片的に届くからです。

## 6. `content_block_delta` で JSON fragment を結合する

次に `content_block_delta` です。

```js
case 'content_block_delta':
    if (!currentBlock) break;
    if (event.delta?.type === 'text_delta') {
        currentBlock.text += event.delta.text;
    } else if (event.delta?.type === 'thinking_delta') {
        currentBlock.thinking += event.delta.thinking;
    } else if (event.delta?.type === 'input_json_delta') {
        currentBlock.input += event.delta.partial_json;
    }
    break;
```

ここでは delta type に応じて、現在の block に文字列を足していきます。

```text
text_delta:
  currentBlock.text += event.delta.text

thinking_delta:
  currentBlock.thinking += event.delta.thinking

input_json_delta:
  currentBlock.input += event.delta.partial_json
```

tool_use input の場合は、`input_json_delta` が重要です。

例えば、LLM が `Read` tool を使うために次の input を生成しているとします。

```js
{ "file_path": "/tmp/example.txt" }
```

streaming ではこれが分割されて届く可能性があります。

```js
{ type: 'input_json_delta', partial_json: '{"file_' }
{ type: 'input_json_delta', partial_json: 'path":' }
{ type: 'input_json_delta', partial_json: ' "/tmp/example.txt"}' }
```

`accumulateStream()` はこれらを単純に連結します。

```js
currentBlock.input += event.delta.partial_json;
```

最終的に `currentBlock.input` は次の文字列になります。

```js
'{"file_path": "/tmp/example.txt"}'
```

この時点ではまだ object ではなく string です。

## 7. `content_block_stop` で `JSON.parse()` する

tool input JSON の組み立てが完了するのは `content_block_stop` です。

```js
case 'content_block_stop':
    // Parse tool_use input from accumulated JSON string
    if (currentBlock?.type === 'tool_use' && typeof currentBlock.input === 'string') {
        try {
            currentBlock.input = JSON.parse(currentBlock.input || '{}');
        } catch {
            currentBlock.input = {};
        }
    }
    currentBlock = null;
    break;
```

`currentBlock.type === 'tool_use'` かつ `input` が string のときだけ `JSON.parse()` します。

成功すると、

```js
currentBlock.input = '{"file_path": "/tmp/example.txt"}';
```

が、

```js
currentBlock.input = { file_path: '/tmp/example.txt' };
```

になります。

parse に失敗した場合は、空 object にフォールバックします。

```js
catch {
    currentBlock.input = {};
}
```

最後に `currentBlock = null` にして、現在の block が完了したことにします。

## 8. `message_delta` で stop reason と output token usage を拾う

```js
case 'message_delta':
    if (event.delta?.stop_reason) {
        message.stop_reason = event.delta.stop_reason;
    }
    if (event.usage) {
        message.usage.output_tokens = event.usage.output_tokens || 0;
    }
    break;
```

`message_delta` では `stop_reason` と出力 token usage を保存します。

tool use が発生した場合、`stop_reason` は `tool_use` になることがあります。

```js
message.stop_reason = 'tool_use';
```

ただし agent loop 側では、`stop_reason` だけで tool execution を始めるわけではありません。実際には `response.content` の中にある `type: 'tool_use'` block を見て実行します。

## 9. `error` event は例外にする

```js
case 'error':
    throw new Error(`Stream error: ${event.error?.message || JSON.stringify(event)}`);
```

streaming 中に API error event が来た場合は例外になります。

この例外は agent loop 側の API call 部分で catch され、`yield { type: 'error', message: err.message }` として外側へ流れます。

## 10. tool_use input 復元の具体例

Anthropic streaming から次のような event 列が来たとします。

```js
{
  type: 'content_block_start',
  index: 0,
  content_block: {
    type: 'tool_use',
    id: 'toolu_123',
    name: 'Read',
    input: {}
  }
}

{
  type: 'content_block_delta',
  delta: {
    type: 'input_json_delta',
    partial_json: '{"file_'
  }
}

{
  type: 'content_block_delta',
  delta: {
    type: 'input_json_delta',
    partial_json: 'path": "/tmp/example.txt"}'
  }
}

{
  type: 'content_block_stop'
}
```

`accumulateStream()` の内部状態は次のように変化します。

```text
content_block_start:
  currentBlock = {
    type: 'tool_use',
    id: 'toolu_123',
    name: 'Read',
    input: ''
  }

1st input_json_delta:
  currentBlock.input = '{"file_'

2nd input_json_delta:
  currentBlock.input = '{"file_path": "/tmp/example.txt"}'

content_block_stop:
  currentBlock.input = JSON.parse(currentBlock.input)
  currentBlock.input = { file_path: '/tmp/example.txt' }
```

最終的な `message.content` は次のようになります。

```js
[
  {
    type: 'tool_use',
    id: 'toolu_123',
    name: 'Read',
    input: {
      file_path: '/tmp/example.txt'
    }
  }
]
```

この形になって初めて、`agent-loop.mjs` が `block.name` と `block.input` を使って tool registry へ dispatch できます。

## 11. `agent-loop.mjs` 内の類似実装

注意点として、`agent-loop.mjs` には `accumulateFromCollected(events)` という似た処理があります。

```js
function accumulateFromCollected(events) {
    const message = {
        content: [],
        stop_reason: null,
        usage: { input_tokens: 0, output_tokens: 0 },
    };

    let currentBlock = null;

    for (const event of events) {
        switch (event.type) {
            case 'content_block_start':
                currentBlock = { ...event.content_block };
                if (currentBlock.type === 'text') currentBlock.text = '';
                if (currentBlock.type === 'thinking') currentBlock.thinking = '';
                if (currentBlock.type === 'tool_use') currentBlock.input = '';
                message.content.push(currentBlock);
                break;
            case 'content_block_delta':
                if (!currentBlock) break;
                if (event.delta?.type === 'text_delta') currentBlock.text += event.delta.text;
                else if (event.delta?.type === 'thinking_delta') currentBlock.thinking += event.delta.thinking;
                else if (event.delta?.type === 'input_json_delta') currentBlock.input += event.delta.partial_json;
                break;
            case 'content_block_stop':
                if (currentBlock?.type === 'tool_use' && typeof currentBlock.input === 'string') {
                    try {
                        currentBlock.input = JSON.parse(currentBlock.input || '{}');
                    } catch {
                        currentBlock.input = {};
                    }
                }
                currentBlock = null;
                break;
        }
    }

    return message;
}
```

役割は `streaming.mjs` の `accumulateStream()` とかなり近く、streaming events を最終 message に変換します。

違いは、`streaming.mjs` の `accumulateStream()` は async iterable を直接 consume する汎用関数で、`agent-loop.mjs` の `accumulateFromCollected()` は一度 collected array に入れた event を後から同期的に畳み込む helper です。

## 12. agent loop との関係

streaming response が tool execution へ進む流れは次のようになります。

```text
callAnthropic(..., stream = true)
  -> streamResponse(res)
  -> events を agent-loop に yield
  -> collected events から accumulated response を作る
  -> response.content に tool_use block が入る
  -> agent-loop が tool_use block を集める
  -> permissions.check()
  -> tools.call()
```

`streaming.mjs` はツールを実行しません。

役割はあくまで、streaming で断片的に届いた tool input JSON を、agent loop が扱える完成済み object に変換することです。

```text
streaming.mjs:
  '{"file_' + 'path": "..."}'
    -> { file_path: '...' }

agent-loop.mjs:
  { type: 'tool_use', name: 'Read', input: { file_path: '...' } }
    -> tools.call('Read', { file_path: '...' })
```

## 13. 実装上の注意点

### 13.1 JSON parse 失敗時は `{}` になる

`input_json_delta` の結合結果が invalid JSON の場合、例外は外へ投げずに空 object にします。

```js
catch {
    currentBlock.input = {};
}
```

その後、agent loop は空 input のまま registry に渡します。

多くの tool は `validateInput()` を持っているため、例えば `Read` なら `file_path is required` の validation error になります。

### 13.2 `currentBlock` は 1 つだけ

この実装は、同時に 1 つの `currentBlock` を組み立てる前提です。

```js
let currentBlock = null;
```

Anthropic の content block event は start / delta / stop の順に block ごとに流れる想定なので、この構造で成立します。

### 13.3 `blockIndex` を使って content 配列へ入れる

`accumulateStream()` では event の `index` を使います。

```js
blockIndex = event.index ?? message.content.length;
message.content[blockIndex] = currentBlock;
```

これにより、API event に index が含まれる場合は、その index に対応する位置へ block を入れます。

### 13.4 `parseSSEChunk()` は parse 失敗を握りつぶす

SSE `data:` の JSON parse に失敗した場合、`parseSSEChunk()` は `null` を返します。

```js
catch {
    return null;
}
```

つまり壊れた chunk は無視されます。ただし API が明示的に `error` event を返した場合は、`accumulateStream()` 側で例外になります。

## まとめ

`v2/src/core/streaming.mjs` は、streaming response を最終的な assistant message へ復元するための組み立て役です。

`tool_use` については、次の 3 段階が核心です。

```text
content_block_start:
  tool_use block を作り、input を空文字列で初期化

content_block_delta:
  input_json_delta.partial_json を input に連結

content_block_stop:
  連結済み input 文字列を JSON.parse して object 化
```

この処理によって、streaming 中に断片として届いた tool input が、

```js
'{"file_path": "/tmp/example.txt"}'
```

から、

```js
{ file_path: '/tmp/example.txt' }
```

へ変換されます。

その完成済み `tool_use` block を `agent-loop.mjs` が受け取り、permission check と tool registry dispatch を通して実ツールを実行します。
