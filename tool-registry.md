# Tool Registry in `registry.mjs`

対象箇所: [`v2/src/tools/registry.mjs`](../v2/src/tools/registry.mjs)

`v2/src/tools/registry.mjs` は、ツールシステムの登録・一覧化・dispatch を担当する中核ファイルです。

一言でいうと、ここは **ツール名と実装を結びつける Map を作り、LLM には schema を見せ、実行時には該当ツールの `call()` を呼ぶ dispatcher** です。

## 全体像

```text
各ツール module
  -> registry.mjs に import
  -> BUILTIN_TOOLS に並べる
  -> createToolRegistry() で Map に登録
  -> tools.list() で LLM 用 schema を返す
  -> tools.call(name, input) で実ツールを実行
```

`agent-loop.mjs` から見ると、registry は次の 2 つの役割を持ちます。

```text
LLM API 呼び出し前:
  tools.list()
    -> 利用可能なツール定義を LLM に渡す

LLM が tool_use を返した後:
  tools.call(block.name, block.input)
    -> 指定されたツールを実行する
```

## 1. 組み込みツールを import する

冒頭では、すべての組み込みツールを個別 module から import しています。

```js
import { BashTool } from './bash.mjs';
import { ReadTool } from './read.mjs';
import { EditTool } from './edit.mjs';
import { WriteTool } from './write.mjs';
import { GlobTool } from './glob.mjs';
import { GrepTool } from './grep.mjs';
// ...
import { ReadMcpResourceTool } from './read-mcp-resource.mjs';
```

各ツール module は、おおむね次の共通インターフェースを持つ object を export します。

```js
export const SomeTool = {
  name: 'SomeTool',
  description: '...',
  inputSchema: { ... },
  validateInput(input) { ... },
  async call(input) { ... },
};
```

registry は各ツールの内部処理には深く立ち入りません。`name`, `description`, `inputSchema`, `validateInput`, `call` という共通形だけを前提にしています。

## 2. `BUILTIN_TOOLS` に並べる

import したツールは `BUILTIN_TOOLS` 配列にまとめられます。

```js
const BUILTIN_TOOLS = [
    BashTool,
    ReadTool,
    EditTool,
    WriteTool,
    GlobTool,
    GrepTool,
    AgentTool,
    WebFetchTool,
    WebSearchTool,
    TodoWriteTool,
    NotebookEditTool,
    MultiEditTool,
    LsTool,
    ToolSearchTool,
    AskUserTool,
    EnterWorktreeTool,
    ExitWorktreeTool,
    SkillTool,
    SendMessageTool,
    RemoteTriggerTool,
    CronCreateTool,
    CronDeleteTool,
    CronListTool,
    LspTool,
    ReadMcpResourceTool,
];
```

新しい組み込みツールを追加する場合、基本的には次の作業になります。

```text
1. v2/src/tools/new-tool.mjs を作る
2. registry.mjs で import する
3. BUILTIN_TOOLS に追加する
```

この配列に入ったツールは、`createToolRegistry()` 実行時にまとめて Map へ登録されます。

## 3. `createToolRegistry()` が registry を生成する

中心は `createToolRegistry()` です。

```js
export function createToolRegistry() {
    const tools = new Map();
    for (const Tool of BUILTIN_TOOLS) {
        tools.set(Tool.name, Tool);
    }

    const registry = {
        // ...
    };

    ToolSearchTool._registry = registry;
    return registry;
}
```

この関数を呼ぶと、内部に `Map` が作られます。

```js
const tools = new Map();
```

その後、`BUILTIN_TOOLS` の各 tool object を `Tool.name` を key にして登録します。

```js
tools.set(Tool.name, Tool);
```

つまり実体は次のような辞書です。

```text
"Bash"  -> BashTool
"Read"  -> ReadTool
"Edit"  -> EditTool
"Write" -> WriteTool
...
```

この `Map` は `createToolRegistry()` のローカル変数なので、外から直接触れません。外部からは `registry.list()`, `registry.call()`, `registry.register()` などの method 経由で扱います。

## 4. `list()` は LLM に渡す tool schema を作る

`list()` は登録済みツールを LLM API に渡せる形へ変換します。

```js
list() {
    return [...tools.values()].map(t => ({
        name: t.name,
        description: t.description,
        input_schema: t.inputSchema,
    }));
}
```

ここで返るのはツール実装そのものではなく、LLM に見せるための定義です。

```js
{
  name: 'Read',
  description: 'Read a file from the local filesystem.',
  input_schema: {
    type: 'object',
    properties: {
      file_path: { type: 'string', description: 'Absolute path to the file' }
    },
    required: ['file_path']
  }
}
```

重要なのは、ツール実装側では `inputSchema` という property 名なのに、LLM API に渡す形では `input_schema` に変換している点です。

```text
tool object:
  inputSchema

LLM API 用:
  input_schema
```

`agent-loop.mjs` は API 呼び出し時に `tools.list()` を渡します。

```js
callApiStreaming(provider, model, state, tools.list(), settings)
```

これにより、LLM は利用可能なツール名と入力形式を知り、必要に応じて `tool_use` を返します。

## 5. `call(name, input)` が dispatch の本体

実行時の中心は `call()` です。

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
1. tool name で Map から tool object を取得
2. 見つからなければ Unknown tool error
3. validateInput(input) を呼ぶ
4. validation error があれば文字列で返す
5. 問題なければ tool.call(input) を実行
```

### 5.1 ツール名で検索する

```js
const tool = tools.get(name);
```

`name` は LLM が返した `tool_use.name` です。

例えば LLM が次を返した場合、

```js
{
  type: 'tool_use',
  name: 'Read',
  input: { file_path: '/tmp/example.txt' }
}
```

registry は `tools.get('Read')` で `ReadTool` を取り出します。

### 5.2 未知のツールは例外にする

```js
if (!tool) throw new Error(`Unknown tool: ${name}`);
```

存在しないツール名が来た場合は例外を投げます。`agent-loop.mjs` 側ではこの例外を catch して、`Tool error: ...` という結果に変換します。

### 5.3 入力 validation を実行する

```js
const errors = tool.validateInput?.(input) || [];
if (errors.length > 0) return `Validation error: ${errors.join(', ')}`;
```

`validateInput` は任意 method です。存在すれば実行し、なければ空配列扱いです。

validation error は例外ではなく、文字列の tool result として返されます。

例えば `Read` に `file_path` がなければ、次のような結果になります。

```text
Validation error: file_path is required
```

この設計だと、LLM は validation error を読んで、次の turn で正しい引数を出し直すことができます。

### 5.4 実ツールを実行する

```js
return tool.call(input);
```

最後に各ツール object の `call()` を呼びます。

registry はここでもツール内部の詳細には関与しません。Bash なら shell 実行、Read ならファイル読み取り、Edit なら文字列置換、という処理は各 module に閉じ込められています。

## 6. `register(tool)` は後からツールを追加する

```js
register(tool) {
    tools.set(tool.name, tool);
}
```

`register()` は、後から tool object を追加するための method です。

テストでは custom tool 登録にも使われています。plugin や動的ロードの拡張点としても使いやすい形です。

登録する object は、組み込みツールと同じ形である必要があります。

```js
registry.register({
  name: 'CustomTest',
  description: 'Test tool',
  inputSchema: { type: 'object', properties: {} },
  validateInput() { return []; },
  async call() { return 'custom result'; },
});
```

登録後は `list()` にも出ますし、`call('CustomTest', {})` で実行できます。

## 7. `get(name)` と `has(name)` は補助 accessor

```js
get(name) {
    return tools.get(name);
}

has(name) {
    return tools.has(name);
}
```

`get()` は tool object そのものを取り出すために使います。

実際に `index.mjs` では、`Skill` tool や `ReadMcpResource` tool に loader/client を後から接続するために使われています。

```js
const skillTool = tools.get('Skill');
if (skillTool) skillTool._skillsLoader = skillsLoader;
```

この設計では、一部ツールが外部 state を持つ場合、registry から tool object を取り出して field を差し込めます。

`has()` はツール存在確認用です。

## 8. `registerMcpTools()` は MCP tool を通常ツールとして包む

MCP 経由で取得した tool は、組み込みツールと同じ interface ではない可能性があります。

そこで `registerMcpTools()` は、MCP tool definition を registry 用 wrapper に変換します。

```js
registerMcpTools(mcpTools, callFn) {
    ToolSearchTool._mcpTools = mcpTools;

    for (const mcpTool of mcpTools) {
        const wrapper = {
            name: mcpTool.name,
            description: mcpTool.description || '',
            inputSchema: mcpTool.inputSchema || { type: 'object', properties: {} },
            validateInput() { return []; },
            async call(input) { return callFn(mcpTool.name, input); },
        };
        tools.set(mcpTool.name, wrapper);
    }
}
```

ここで行っていることは 2 つです。

```text
1. ToolSearchTool に MCP tool 一覧を渡す
2. MCP tool を registry の共通 interface にラップして Map に登録する
```

### 8.1 ToolSearch 用に MCP tool 一覧を渡す

```js
ToolSearchTool._mcpTools = mcpTools;
```

`ToolSearchTool` は `_mcpTools` を見て、MCP tool も検索対象にします。

### 8.2 wrapper を作る

MCP tool から wrapper を作ります。

```js
const wrapper = {
    name: mcpTool.name,
    description: mcpTool.description || '',
    inputSchema: mcpTool.inputSchema || { type: 'object', properties: {} },
    validateInput() { return []; },
    async call(input) { return callFn(mcpTool.name, input); },
};
```

wrapper は通常ツールと同じ shape です。

```text
name
description
inputSchema
validateInput
call
```

`call()` の中身だけが MCP 用で、実際には渡された `callFn` に委譲します。

```js
async call(input) {
  return callFn(mcpTool.name, input);
}
```

`index.mjs` では、MCP client の `callTool()` が `callFn` として渡されます。

```js
tools.registerMcpTools(
  mcpTools,
  (toolName, toolArgs) => client.callTool(toolName, toolArgs)
);
```

これにより、agent loop 側は MCP tool か組み込み tool かを意識せず、常に次の形で呼べます。

```js
tools.call(block.name, block.input)
```

## 9. `ToolSearchTool._registry = registry`

最後に、`ToolSearchTool` に registry 自身を渡しています。

```js
ToolSearchTool._registry = registry;
return registry;
```

`ToolSearchTool` は `_registry.list()` を使って、登録済みツールを検索します。

つまり `ToolSearch` は普通のツールでありながら、registry の中身を検索するメタツールでもあります。

## 10. `agent-loop.mjs` との関係

`registry.mjs` は単体では LLM を呼びません。実際の制御フローは `agent-loop.mjs` 側です。

関係は次のようになります。

```text
createToolRegistry()
  -> registry object を作る

agent-loop:
  -> tools.list() を LLM API に渡す

LLM:
  -> tool_use を返す

agent-loop:
  -> permissions / hooks を通す
  -> tools.call(tool_use.name, tool_use.input)

registry:
  -> Map から tool object を探す
  -> validateInput()
  -> tool.call()
```

registry は「どのツールをどう呼ぶか」を知っていますが、「いつ呼ぶか」は `agent-loop.mjs` が決めます。

## 11. この設計の特徴

### 小さいが強い dispatcher

`registry.mjs` 自体は短いですが、責務が明確です。

```text
登録
一覧化
名前解決
入力 validation
実行委譲
MCP tool の同一 interface 化
```

### ツール実装と agent loop を分離している

`agent-loop.mjs` は `BashTool` や `ReadTool` の内部実装を知りません。

ただ次のように呼ぶだけです。

```js
tools.call(block.name, block.input)
```

これにより、新しいツール追加時に agent loop を変更する必要がほぼありません。

### 組み込みツールと MCP tool を同じ呼び方にしている

MCP tool も wrapper 化されるため、最終的には組み込みツールと同じ interface になります。

```text
Builtin tool:
  tool.call(input)

MCP tool:
  wrapper.call(input) -> client.callTool(name, input)
```

agent loop からは同じに見えます。

## まとめ

`v2/src/tools/registry.mjs` は、ツールシステムの **登録・schema 公開・dispatch** を担う中核です。

役割を一文にすると次の通りです。

> ツール実装 object を名前付きで登録し、LLM には `name / description / input_schema` を公開し、実行時には `tool_use.name` から該当ツールを探して `validateInput()` と `call()` を行う。

`agent-loop.mjs` が「いつツールを使うか」を制御し、`registry.mjs` が「どのツールをどう呼ぶか」を解決します。
