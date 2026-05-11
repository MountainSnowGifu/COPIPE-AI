# Permission Checker in `checker.mjs`

対象箇所: [`v2/src/permissions/checker.mjs`](../v2/src/permissions/checker.mjs)

`v2/src/permissions/checker.mjs` は、ツール実行前の権限・安全チェックを担当するファイルです。

一言でいうと、ここは **agent loop がツールを実行する直前に通すゲート** です。`Bash` の危険コマンド検出、ファイル操作のパス検証、permission mode による許可・拒否をまとめて判断します。

## 全体像

```text
agent-loop.mjs
  -> permissions.check(toolName, input)
      -> Bash なら injection check
      -> file operation なら path check
      -> permission mode で allow / deny
  -> allowed なら tools.call()
  -> denied なら tool_result: "Permission denied"
```

`checker.mjs` は単体ですべての安全判定を実装しているわけではありません。次の 3 つの helper を使います。

```js
import { requiresPermission } from './prompt.mjs';
import { checkInjection } from './injection-check.mjs';
import { validatePath } from './path-check.mjs';
```

役割は次の通りです。

```text
prompt.mjs:
  どのツールが default mode で許可確認を必要とするか

injection-check.mjs:
  Bash command に危険な shell pattern が含まれるか

path-check.mjs:
  Read / Edit / Write / MultiEdit の対象 path が危険でないか
```

## 1. `createPermissionChecker(config)` が checker を作る

中心は `createPermissionChecker()` です。

```js
export function createPermissionChecker(config = {}) {
    const mode = config.defaultMode || process.env.CLAUDE_CODE_PERMISSION_MODE || 'default';
    const rl = config.rl || null;

    return {
        mode,
        async check(toolName, input) {
            // ...
        },
    };
}
```

この関数は、`mode` と `check()` method を持つ object を返します。

`mode` は次の優先順位で決まります。

```text
1. config.defaultMode
2. process.env.CLAUDE_CODE_PERMISSION_MODE
3. 'default'
```

つまり、設定で明示されていればそれを使い、なければ環境変数、さらに何もなければ `default` mode になります。

## 2. `check(toolName, input)` が実行前ゲート

`check()` は agent loop から呼ばれます。

```js
const allowed = await permissions.check(block.name, block.input);
if (!allowed) {
    toolResults.push({
        type: 'tool_result',
        tool_use_id: block.id,
        content: 'Permission denied',
    });
    continue;
}
```

`check()` が `true` を返すとツール実行へ進み、`false` を返すとツールは実行されません。

この関数の中では、permission mode の判定より前に、常時実行される安全チェックがあります。

```text
1. Bash command の injection check
2. file operation の path check
3. permission mode 判定
```

## 3. Bash は常に injection check を通る

最初の安全チェックは `Bash` 用です。

```js
if (toolName === 'Bash' && input?.command) {
    const injection = checkInjection(input.command);
    if (!injection.safe) {
        return false;
    }
}
```

これは permission mode に関係なく常に実行されます。

つまり `bypassPermissions` mode であっても、この injection check で危険と判定されれば `false` になります。現在の実装では、`switch (mode)` より前にこの処理があるためです。

`checkInjection()` は [`v2/src/permissions/injection-check.mjs`](../v2/src/permissions/injection-check.mjs) にあり、危険 pattern を正規表現で検出します。

例:

```js
const DANGEROUS_PATTERNS = [
    { pattern: /;\s*rm\s+-rf\s+\//, label: 'rm -rf /' },
    { pattern: /\|\s*sh\b/, label: 'pipe to sh' },
    { pattern: /\|\s*bash\b/, label: 'pipe to bash' },
    { pattern: /`[^`]+`/, label: 'backtick execution' },
    { pattern: /\$\([^)]+\)/, label: 'command substitution' },
    { pattern: /curl\s.*\|\s*(bash|sh)/, label: 'curl pipe to shell' },
    { pattern: /wget\s.*\|\s*(bash|sh)/, label: 'wget pipe to shell' },
];
```

`checkInjection(command)` の返り値は次の形です。

```js
{ safe: true }
```

または危険時:

```js
{
  safe: false,
  pattern: '...',
  label: 'curl pipe to shell'
}
```

ただし `checker.mjs` は現在、`label` や `reason` を agent loop に渡していません。単に `false` を返すため、最終的な tool result は `"Permission denied"` になります。

## 4. ファイル操作は常に path check を通る

次に、ファイル操作系ツールには path validation が入ります。

```js
if (['Edit', 'Write', 'Read', 'MultiEdit'].includes(toolName) && input?.file_path) {
    const pathResult = validatePath(input.file_path, { write: toolName !== 'Read' });
    if (!pathResult.safe) {
        return false;
    }
}
```

対象ツールは次の 4 つです。

```text
Read
Edit
Write
MultiEdit
```

`Read` の場合は `write: false`、それ以外は `write: true` になります。

```js
validatePath(input.file_path, { write: toolName !== 'Read' })
```

これも permission mode に関係なく、mode 判定より前に常時実行されます。

## 5. `validatePath()` が見るもの

`validatePath()` は [`v2/src/permissions/path-check.mjs`](../v2/src/permissions/path-check.mjs) にあります。

主なチェックは次の通りです。

```text
1. path が空または非文字列でないか
2. null byte が含まれていないか
3. sensitive file pattern に一致しないか
4. write operation の場合、protected directory へ書こうとしていないか
5. cwd 外または /tmp 外なら warning を付ける
```

### 5.1 sensitive file pattern

次のようなファイルは read/write ともに拒否されます。

```js
const SENSITIVE_PATTERNS = [
    /\.env$/,
    /\.env\..+$/,
    /credentials\.json$/,
    /credentials\.yaml$/,
    /\.pem$/,
    /\.key$/,
    /id_rsa$/,
    /id_ed25519$/,
    /\.ssh\/config$/,
    /\.netrc$/,
    /\.pgpass$/,
    /\.aws\/credentials$/,
    /\.docker\/config\.json$/,
    /secrets\.yaml$/,
    /secrets\.json$/,
];
```

### 5.2 protected directory

write operation の場合、次の directory への書き込みは拒否されます。

```js
const PROTECTED_DIRS = [
    '/etc',
    '/usr',
    '/sbin',
    '/boot',
    '/sys',
    '/proc',
];
```

`Read` は `write: false` なので、この protected directory write check の対象外です。ただし sensitive file pattern は `Read` でも拒否されます。

### 5.3 cwd 外 path の扱い

`validatePath()` は cwd 外 path を即拒否にはしていません。

```js
if (!resolved.startsWith(cwd) && !resolved.startsWith('/tmp')) {
    warning = 'Path is outside the current working directory';
}
```

cwd 外かつ `/tmp` 外の場合は `warning` を付けますが、`safe: true` のまま返します。

ただし `checker.mjs` は現在、この warning を使っていません。`pathResult.safe` だけを見ています。

## 6. permission mode 判定

安全チェックを通過したあと、`mode` によって許可・拒否が決まります。

```js
switch (mode) {
    case 'bypassPermissions': return true;
    case 'acceptEdits':
        if (toolName === 'Bash' || toolName === 'Agent') {
            return !requiresPermission(toolName) || !!config.bypassBash;
        }
        return true;
    case 'auto': return true;
    case 'dontAsk': return false;
    case 'plan': return toolName === 'Read' || toolName === 'Glob' || toolName === 'Grep';
    case 'default':
    default:
        if (!requiresPermission(toolName)) return true;
        if (!rl) return true;
        return true;
}
```

各 mode の意味を実装ベースで見ると次の通りです。

## 7. `bypassPermissions`

```js
case 'bypassPermissions': return true;
```

mode 判定まで到達したツールはすべて許可します。

ただし、先に説明した通り、Bash injection check と file path check はこの前に実行されます。したがって、この実装では `bypassPermissions` でも危険 command / 危険 path は止まります。

## 8. `acceptEdits`

```js
case 'acceptEdits':
    if (toolName === 'Bash' || toolName === 'Agent') {
        return !requiresPermission(toolName) || !!config.bypassBash;
    }
    return true;
```

`acceptEdits` は、基本的に file operation を許可する mode です。

`Bash` と `Agent` だけは特別扱いで、通常は `requiresPermission()` により permission が必要なツールなので拒否されます。ただし `config.bypassBash` が truthy なら許可されます。

読み替えると次のような挙動です。

```text
Edit / Write / MultiEdit:
  許可

Read / Glob / Grep:
  許可

Bash / Agent:
  config.bypassBash が true なら許可
  それ以外は基本拒否
```

## 9. `auto`

```js
case 'auto': return true;
```

`auto` は、mode 判定まで到達したツールをすべて許可します。

コメントでは `AI decides` とありますが、現在の実装上は `true` 固定です。

## 10. `dontAsk`

```js
case 'dontAsk': return false;
```

`dontAsk` は、mode 判定まで到達したツールをすべて拒否します。

コメントでは `deny everything not pre-approved` とありますが、現在の `checker.mjs` には pre-approved list の参照はありません。そのため実装上は常に `false` です。

## 11. `plan`

```js
case 'plan': return toolName === 'Read' || toolName === 'Glob' || toolName === 'Grep';
```

`plan` は read-only に近い mode です。

許可されるのは次の 3 つだけです。

```text
Read
Glob
Grep
```

注意点として、`LS` は `prompt.mjs` の safe tools には含まれていますが、`plan` mode の許可リストには入っていません。したがって現在の実装では、`plan` mode で `LS` は拒否されます。

## 12. `default`

```js
case 'default':
default:
    if (!requiresPermission(toolName)) return true;
    if (!rl) return true;
    return true;
```

`default` mode では、まず `requiresPermission(toolName)` を見ます。

`requiresPermission()` は [`v2/src/permissions/prompt.mjs`](../v2/src/permissions/prompt.mjs) にあります。

```js
export function requiresPermission(toolName) {
    const SAFE_TOOLS = new Set([
        'Read', 'Glob', 'Grep', 'LS', 'ToolSearch',
        'AskUser', 'CronList', 'TodoWrite',
    ]);
    return !SAFE_TOOLS.has(toolName);
}
```

safe tools は許可確認なしで通ります。

```text
Read
Glob
Grep
LS
ToolSearch
AskUser
CronList
TodoWrite
```

safe tools 以外は、本来なら interactive permission prompt を出す想定です。`prompt.mjs` には `promptPermission()` があります。

```js
export async function promptPermission(toolName, input, rl) {
    if (!rl || typeof rl.question !== 'function') {
        return false;
    }

    const summary = formatToolSummary(toolName, input);
    return new Promise(resolve => {
        rl.question(`Allow ${summary}? [y/N] `, answer => {
            resolve(answer.trim().toLowerCase() === 'y');
        });
    });
}
```

ただし現在の `checker.mjs` は `promptPermission()` を import していません。`rl` がある場合も最終的に `return true` しています。

```js
if (!rl) return true;
return true;
```

つまり、現在の実装では `default` mode の interactive prompt は未接続です。

## 13. agent loop から見た位置づけ

`checker.mjs` は `agent-loop.mjs` の tool execution pipeline の中で、hook の後、実ツール実行の前に呼ばれます。

```text
tool_use block
  -> PreToolUse hook
  -> permissions.check(toolName, input)
  -> tools.call(toolName, input)
  -> PostToolUse hook
  -> tool_result
```

permission check が拒否した場合、`tools.call()` は呼ばれません。

```js
const allowed = await permissions.check(block.name, block.input);
if (!allowed) {
    yield { type: 'hookPermissionResult', tool: block.name, allowed: false };
    toolResults.push({
        type: 'tool_result',
        tool_use_id: block.id,
        content: 'Permission denied',
    });
    continue;
}
```

LLM には `"Permission denied"` という `tool_result` が返るため、LLM は別の方法を考えるか、ユーザーに許可を求める返答へ進めます。

## 14. 実装上の注意点

### 14.1 `bypassPermissions` でも事前安全チェックは残る

`Bash` の injection check と file path check は、mode の `switch` より前にあります。

そのため現在のコードでは、`bypassPermissions` は「permission mode 判定を bypass する」という意味であり、すべての安全チェックを無効化するわけではありません。

### 14.2 `default` mode の prompt は未接続

`prompt.mjs` には `promptPermission()` が実装されていますが、`checker.mjs` は `requiresPermission()` しか import していません。

そのため現在の `default` mode は、危険ツールでも最終的に `true` を返します。

```js
if (!rl) return true;
return true;
```

コメント上は interactive permission を想定していますが、現状では permissive な挙動です。

### 14.3 `pathResult.warning` は使われていない

`validatePath()` は cwd 外 path に warning を付けますが、`checker.mjs` は `safe` だけを見ています。

```js
if (!pathResult.safe) {
    return false;
}
```

したがって cwd 外 path でも、sensitive file や protected directory に該当しなければ許可されます。

### 14.4 `MultiEdit` の入力 shape に注意

`checker.mjs` は `input?.file_path` を見て path check します。

```js
if (['Edit', 'Write', 'Read', 'MultiEdit'].includes(toolName) && input?.file_path) {
    // ...
}
```

もし `MultiEdit` が複数ファイルを扱う shape になった場合、この checker は全ファイルを見られません。現在の実装では `file_path` 単一を前提にしています。

## まとめ

`v2/src/permissions/checker.mjs` は、ツール実行前に置かれた permission gate です。

役割を一文にすると次の通りです。

> `toolName` と `input` を受け取り、Bash の危険 command、ファイル操作の危険 path、permission mode を判定して、実行を許可するか拒否する。

agent loop との関係は次の通りです。

```text
LLM の tool_use
  -> hook check
  -> permission checker
  -> tool registry dispatch
  -> tool result
```

つまり `checker.mjs` は、LLM が要求したツール呼び出しをローカル環境で実行してよいかを決める、実行直前の安全ゲートです。
