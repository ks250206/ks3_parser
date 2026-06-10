# ks3_parser

共和標準フォーマット3（`.KS3`）のアナログ集録データを読み取り、共和電業ツールのサンプル出力に近い Shift-JIS CSV を生成する Rust ツールです。`ks2_parser` をベースに、CLI/TUI の操作感を維持しつつ内部ロジックを KS3 の識別ヘッダ走査型パーサーに置き換えています。

## できること

- `.KS3` ファイルの 24 byte 識別ヘッダを順次解析
- 未対応の大分類・小分類を仕様通り無視
- アナログデータ型 `short` / `long` / `float` / `double` に対応
- 内部カウンタ同時集録 ON のデータを読み飛ばして変換
- KS3 内のCH数、サンプリング周波数、係数、単位、開始時刻を使ってCSVを生成
- `ratatui` + `crossterm` による設定確認TUI

物理量は次の式で計算します。

```text
raw * 工学値変換係数 * ケーブル係数 * 任意補正係数 + オフセット + オフセットゼロ値
```

オフセットゼロ値は、KS3内のオフセットゼロ設定がONのCHだけ加算します。

## 起動方法

### TUI

```bash
cargo run
```

別の設定ファイルを使う場合:

```bash
cargo run -- --config my.toml
```

### CLI

```bash
cargo run -- --cli
cargo run -- --cli --config my.toml
```

CLI実行時は標準出力に `records`、`channels`、`sampling_frequency_hz` を表示します。CSVは `output_dir/output_file_name` に書き出されます。

## 設定ファイル

```toml
input_path = "samples/TEST#0003.KS3"
output_dir = "out"
output_file_name = "result.csv"
```

| 項目 | 内容 |
| --- | --- |
| `input_path` | 入力 `.KS3` ファイル |
| `output_dir` | CSV出力先ディレクトリ |
| `output_file_name` | 出力CSV名。省略時は `output.csv` |

KS2版の手動オフセット、エンディアン、AD係数設定は不要です。KS3はファイル内メタデータから必要な情報を取得します。

## CSV出力

出力は Shift-JIS、CRLF、サンプルCSV準拠のメタ情報付き形式です。

```csv
"ID番号","CTRS-100A"
"タイトル",""
"試験日時","2026/06/09","17:58:22"
"測定CH数",4
"デジタル入力","OFF"
"サンプリング周波数(Hz)",1000
"集録データ数/CH",4700
"測定時間(sec)",4.7
"CH名称"
"CH No","CH1","CH2","CH3","CH4"
...
0.000,-2.325439453125,0.4857177734375,0.397705078125,-0.2764892578125
```

## TUI操作

| キー | 動作 |
| --- | --- |
| `1`-`4` | ペイン切り替え |
| `j` / `k`, `↑` / `↓` | 移動またはスクロール |
| `Enter`, `a` | 一覧ペインで編集開始、編集中は確定 |
| `i` | 一覧ペインで編集開始、カーソルを先頭に置く |
| `Esc` | 編集取消、選択解除、ヘルプを閉じる |
| `g` `g` | フォーカス中ペインの先頭へ |
| `G` | フォーカス中ペインの末尾へ |
| `v` | 行選択開始 / 解除 |
| `y` | 選択範囲をOSクリップボードへコピー |
| `u` | 設定変更の undo |
| `Ctrl+R` | redo |
| `s` | 設定ファイル保存 |
| `r` | 変換実行 |
| `?` | キーヘルプ表示 |
| `q` | 終了 |

## テスト

```bash
cargo test
```

ローカルに `samples/TEST#*.KS3` と対応するCSVがある場合、無視テストで参照CSVとの完全一致を確認できます。

```bash
KS3_SAMPLE_DIR=samples cargo test local_samples_match_reference_csv -- --ignored
```

カバレッジ目標は80%以上です。

```bash
cargo llvm-cov --workspace --all-features --fail-under-lines 80
```

## 制約

- v1対象は `.KS3` のアナログデータです。`.KC3` / CANデータの本格変換は未対応です。
- `samples/` はローカル検証用で、リポジトリには含めません。
