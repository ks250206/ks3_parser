# ks2_parser

`ks2` バイナリから 4ch 分の `i32` レコードを読み取り、`index,ch1,ch2,ch3,ch4` 形式の CSV を出力する Rust ツールです。通常は TUI で設定を確認・編集し、`--cli` を付けると一発実行モードで動きます。

## できること

- `ks2` ファイルを読み込み、4ch 固定で CSV を生成
- 各値を次の式でスケーリング

```text
raw / ADConverterScale * ADRangeCoefficient * ADCoefficient * coefficient.CHx
```

- `variable_header_byte` / `data_header_byte` / `footer_byte` の自動検出
- `ratatui` + `crossterm` による 4 ペイン TUI
- 設定編集、保存、変換実行、CSV プレビュー、ログ表示
- 変更の undo / redo、行選択コピー

## 構成

- `src/lib.rs`: 設定読み書き、オフセット自動検出、バイナリ解析、CSV 出力
- `src/main.rs`: CLI/TUI の起動分岐
- `src/tui/app.rs`: TUI 状態管理とキー処理
- `src/tui/ui.rs`: TUI レイアウトと描画
- `src/tui/clipboard.rs`: クリップボード連携

## 起動方法

### TUI

デフォルトでは `config.toml` を読み込んで TUI を起動します。

```bash
cargo run
```

別の設定ファイルを使う場合:

```bash
cargo run -- --config my.toml
```

### CLI

TUI を使わず、設定を読み込んで変換だけ実行します。

```bash
cargo run -- --cli
cargo run -- --cli --config my.toml
```

CLI 実行時は標準出力に `records` や最終的に使われたオフセット値を表示します。CSV 自体は `output_dir/output_file_name` に書き出されます。

## TUI 操作

### 画面構成

| ペイン | 内容 |
| --- | --- |
| `1` Config | 設定項目一覧 |
| `2` Detail | 選択中フィールドの説明、許容値、状態 |
| `3` CSV | 出力 CSV の先頭プレビュー |
| `4` Log | 保存・変換・undo/redo などのログ |

### 主要キー

| キー | 動作 |
| --- | --- |
| `1`-`4` | ペイン切り替え |
| `j` / `k`, `↑` / `↓` | 移動またはスクロール |
| `Enter`, `a` | 一覧ペインで編集開始、編集中は確定 |
| `i` | 一覧ペインで編集開始、カーソルを先頭に置く |
| `Esc` | 編集取消、選択解除、ヘルプを閉じる |
| `Space` | `auto_detect_offsets` または `endianness` を切り替え |
| `g` `g` | フォーカス中ペインの先頭へ |
| `G` | フォーカス中ペインの末尾へ |
| `v` | 行選択開始 / 解除 |
| `y` | 選択範囲を OS クリップボードへコピー |
| `u` | 設定変更の undo |
| `Ctrl+R` | redo |
| `s` | 設定ファイル保存 |
| `r` | 変換実行 |
| `?` | キーヘルプ表示 |
| `q` | 終了 |

一覧ペインはマウス左クリックでも選択できます。CSV プレビューは `output_dir/output_file_name` を直接読み込むので、変換前は「まだ CSV がありません」という案内が表示されます。

## 設定ファイル

例:

```toml
input_path = "Test0029.ks2"
output_dir = "out"
output_file_name = "result.csv"
auto_detect_offsets = true

header_byte = 256
variable_header_byte = 2890
data_header_byte = 13452
data_skip_byte = 12
footer_byte = 0

values_per_record = 4
endianness = "little"

ADConverterScale = 2099200002.0
ADRangeCoefficient = 5000.0
ADCoefficient = 256.0

[coefficient]
CH1 = 1.05
CH2 = 1.05
CH3 = 1.04
CH4 = 1.12
```

主な項目:

- `input_path`: 入力 `.ks2` ファイル
- `output_dir`: CSV 出力先ディレクトリ
- `output_file_name`: 出力 CSV 名。省略時は `output.csv`
- `auto_detect_offsets`: `true` なら 3 つのオフセットを入力ファイルから自動検出
- `header_byte`: データ開始位置計算の基準オフセット
- `variable_header_byte`, `data_header_byte`, `data_skip_byte`, `footer_byte`: データ領域切り出しに使う各種バイト数
- `values_per_record`: 現状 `4` 固定
- `endianness`: `little` または `big`
- `ADConverterScale`, `ADRangeCoefficient`, `ADCoefficient`: スケーリング係数
- `coefficient.CH1`-`CH4`: チャンネルごとの補正係数

## 自動検出

`auto_detect_offsets = true` のとき、入力ファイル内の `CRLF` を数え、以下の位置から 14 バイトを読み取って数値として解釈します。

- 12 個目の `CRLF` 直後: `variable_header_byte`
- 13 個目の `CRLF` 直後: `data_header_byte`
- 14 個目の `CRLF` 直後: `footer_byte`

`header_byte` と `data_skip_byte` は自動検出されません。

## 出力

CSV ヘッダは固定です。

```csv
index,ch1,ch2,ch3,ch4
```

各行は 1 レコード分で、`index` は 0 始まりです。

## テスト

ユニットテストは `src/lib.rs` と `src/main.rs` にあり、`tempfile` を使って実ファイルベースで検証しています。

```bash
cargo test
```

## 制約と注意

- 4ch 専用です
- `values_per_record != 4` はエラーになります
- `ADConverterScale = 0` はエラーになります
- TUI で保存すると `toml::to_string_pretty` による再生成になるため、元のコメントや並び順は保持されません
- undo / redo は TUI 上の設定状態に対して働きます。出力済み CSV ファイル自体は巻き戻しません
- クリップボード機能は OS のクリップボード API に依存し、環境によっては失敗します
