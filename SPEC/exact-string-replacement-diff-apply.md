# EditTool / MultiEditTool による差分適用

このプロジェクトの「差分適用」は、`git apply` のように unified diff をパースする方式ではなく、現在のファイル本文から `old_string` を探して `new_string` に置き換える **exact string replacement** 方式です。

対象の中心は次の2つです。

- `v2/src/tools/edit.mjs`
- `v2/src/tools/multi-edit.mjs`

## EditTool

`EditTool` は、1回のツール呼び出しで基本的に1ファイル・1箇所を置換します。

入力例:

```js
{
  file_path: "/abs/path/to/file",
  old_string: "置換前の完全一致テキスト",
  new_string: "置換後のテキスト",
  replace_all: false
}
```

処理の流れ:

```text
file_path を絶対パス化
  -> ファイル存在確認
  -> 事前に Read 済みか確認
  -> ファイル全体を文字列として読み込み
  -> old_string が含まれるか確認
  -> replace_all=false なら old_string が一意か確認
  -> content.replace(old_string, new_string)
  -> fs.writeFileSync() で保存
```

重要なのは、`old_string` が完全一致でなければ失敗する点です。空白、改行、インデント、コメント、末尾スペースまで一致している必要があります。

`replace_all` が `false` の場合、同じ `old_string` がファイル内に複数あると失敗します。

```js
const firstIdx = content.indexOf(input.old_string);
const secondIdx = content.indexOf(input.old_string, firstIdx + 1);
if (secondIdx !== -1) {
    return `Error: old_string is not unique...`;
}
```

これは誤った箇所を書き換えないためです。複数ある場合は、`old_string` に前後の文脈を含めて一意にするか、`replace_all: true` を使います。

`replace_all: true` の場合は、`old_string` を正規表現用にエスケープして全出現箇所を置換します。

```js
const escaped = input.old_string.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
content = content.replace(new RegExp(escaped, 'g'), input.new_string);
```

## MultiEditTool

`MultiEditTool` は、複数の `{ file_path, old_string, new_string }` をまとめて適用します。

入力例:

```js
{
  edits: [
    {
      file_path: "/abs/path/a.js",
      old_string: "before",
      new_string: "after"
    },
    {
      file_path: "/abs/path/b.js",
      old_string: "foo",
      new_string: "bar"
    }
  ]
}
```

特徴は、先に全件検証してから書くことです。

```text
Phase 1: 全 edit についてファイル読み込み・old_string 存在確認
  -> 1件でもエラーがあれば中止
Phase 2: メモリ上で順番に replace
Phase 3: 変更後の内容を各ファイルへ write
```

そのため `MultiEditTool` は、単発の `EditTool` より「まとめて失敗できる」作りです。ただし厳密なトランザクションではありません。Phase 3 の途中で `writeFileSync` が失敗した場合に、すでに書いたファイルを巻き戻す処理はありません。

また、現状の `MultiEditTool` には `EditTool` と違って次の安全機構がありません。

- Read 済みチェック
- `old_string` の一意性チェック
- `replace_all`
- 書き込み失敗時の `try/catch`
- checkpoint 保存

つまり `MultiEditTool` は「複数 edit を事前検証してから適用する」ものですが、`EditTool` より安全チェックは薄いです。

## 具体例

元のファイル:

```js
function greet() {
    return "hello";
}
```

`EditTool` に渡す入力:

```js
{
  file_path: "/path/example.js",
  old_string: "return \"hello\";",
  new_string: "return \"こんにちは\";"
}
```

保存後:

```js
function greet() {
    return "こんにちは";
}
```

この方式のポイントは、モデルが「この行をこう変える」という diff を直接渡すのではなく、置換前の実テキストと置換後の実テキストをペアで渡すことです。

安全に動かすには、まず `ReadTool` で現状のファイルを読み、その出力から十分な文脈を含む `old_string` を作る流れになります。

