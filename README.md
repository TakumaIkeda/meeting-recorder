# Meeting Recorder

Windows 用の会議録音アプリ（Tauri 2 + React + TypeScript）。
最終形は「録音 → whisper.cpp で文字起こし → `claude -p` で要約」の振り返りアプリ。
現段階は **録音 → 文字起こし（全文表示）** まで。要約は枠だけ用意してある。

## 構成

```
crates/recorder/   GUI 非依存の録音モジュール (cpal + hound)
  src/devices.rs   既定マイク / 既定出力(ループバック)の取得、デバイス一覧、probe
  src/recorder.rs  Recorder::start / stop、レベルメーター、WAV 書き出し、meta.json
  src/timeline.rs  2 トラックの開始時刻合わせ・無音区間の補完
  src/dsp.rs       ダウンミックス、16 kHz へのリサンプリング（窓関数付き sinc）
  examples/        手順 1・2 用の CLI
crates/transcriber/ GUI 非依存の文字起こしモジュール
  src/whisper.rs   whisper-cli の実行、進捗の読み取り、JSON(-oj) の解析
  src/transcript.rs 2 トラックを時刻順に統合（自分 / 相手）、transcript.json の保存
  src/config.rs    whisper-cli / モデルの設定 (config.json)
  examples/        既存フォルダを文字起こしする CLI
src-tauri/         Tauri シェル（コマンド・イベント・文字起こしキュー）
src/               React フロントエンド
  MainView.tsx     録音と「今回の録音」の状態表示
  DetailView.tsx   会議の詳細（要約の枠 + 全文）
design/            画面デザインの元データ（デザインツール独自形式。参照用で、そのままは動かない）
```

## 出力

```
<ドキュメント>\MeetingRecorder\meetings\<YYYY-MM-DD_HHMM>\
  mic.wav      既定の入力デバイス
  system.wav   既定の出力デバイス（WASAPI ループバック）
  meta.json    開始時刻(RFC3339)・デバイス・ネイティブ形式・補完した無音秒数
  mic.json / system.json   whisper-cli の生出力
  transcript.json          統合済みの全文 {lines: [{start_ms, end_ms, speaker: "me"|"them", text}]}
```

- WAV は 16 kHz / モノラル / 16 bit PCM（whisper.cpp にそのまま渡せる）。
  デバイスのネイティブ形式（例: 48 kHz・2ch・f32）で受け、ダウンミックス → リサンプリングして保存。
- **2 ファイルの sample 0 は同じ瞬間**。両ストリームを開始した直後の時刻を t0 とし、
  各ブロックの WASAPI キャプチャ時刻（QPC）から位置を決めて書き込む。
  - t0 より前にキャプチャされた分は捨てる
  - ループバックは何も再生されていない間データが来ないので、その区間は無音で埋める
  - 停止時は停止時刻まで無音で埋めるので、2 ファイルの長さはほぼ一致する
- 約 1 秒ごとに WAV ヘッダを更新するので、強制終了しても途中までは再生できる。
- 同じ分に 2 回録音した場合はフォルダ名に `_2`, `_3` … が付く。

## 文字起こし (whisper.cpp)

録音を停止すると自動で文字起こしが始まる（1 件ずつ順番に処理）。
`mic.wav` と `system.wav` を別々に whisper-cli にかけ、セグメントの開始時刻で並べて
「自分（マイク）」「相手（システム音声）」の発言として統合する。

- 状態：`文字起こし待ち` → `文字起こし中 n%` → `完了`。失敗時はエラー内容と「再実行」ボタンを表示。
- 完全に無音のトラック（ピーク約 -50 dBFS 未満）は whisper にかけない（無音で「ご視聴ありがとうございました」等が出るのを防ぐ）。
- `[BLANK_AUDIO]`・`（拍手）` のような括弧だけのセグメントは除外。同じ話者の 1.5 秒以内の連続セグメントは 1 行にまとめる。

### セットアップ

1. [whisper.cpp](https://github.com/ggml-org/whisper.cpp) の Windows 版バイナリ（`whisper-cli.exe`）を用意する。
2. ggml モデル（例: `ggml-large-v3-turbo.bin`）をダウンロードする。
3. 初回起動時（または初回の文字起こし時）に `<ドキュメント>\MeetingRecorder\config.json` が作られるので編集する：

```json
{
  "whisper_cli": "C:\\tools\\whisper\\whisper-cli.exe",
  "model": "C:\\Users\\you\\Documents\\MeetingRecorder\\models\\ggml-large-v3-turbo.bin",
  "language": "ja",
  "threads": 0
}
```

`whisper_cli` はファイル名だけなら PATH から探す。`threads: 0` は論理 CPU 数。
設定ファイルは文字起こしのたびに読み直すので、失敗したら直して「再実行」を押せばよい。

GUI なしで確認する場合：

```powershell
cargo run -p transcriber --example transcribe -- <会議フォルダ> <モデル.bin> [whisper-cli.exe]
```

## 開発環境 (Windows)

- Rust (MSVC toolchain), Node.js 20+
- WebView2（Windows 10/11 は標準で入っている）

```powershell
npm install
```

## 進め方と確認方法

### 1. デバイス一覧とループバックの確認

```powershell
cargo run -p recorder --example devices
```

全デバイスと既定の入出力・ネイティブ形式を表示した後、マイクとループバックをそれぞれ 3 秒キャプチャして
受信秒数とピークを表示する。**実行中に YouTube などで音を流しておく**こと
（ループバックは無音中はデータが来ないため、受信 0 秒になる）。

### 2. 録音モジュール単体での録音

```powershell
cargo run -p recorder --example record -- 15            # 15 秒録音して ./meetings に保存
cargo run -p recorder --example record -- 15 D:\rec     # 保存先ルートを指定
```

経過時間と両入力のレベルを表示し、終了後に各 WAV の長さ・形式・補完した無音秒数を表示する。
確認ポイント：
- mic.wav / system.wav がともに 16000 Hz / 1 ch / 16 bit で、長さがほぼ同じ
- 途中で再生を止めた区間が system.wav で無音になり、その後の音がずれていない
- 手を叩く等、両方に入る音の位置が 2 ファイルで一致している

ユニットテスト（リサンプラ・時刻合わせ・whisper 出力の解析と統合。偽の whisper-cli を使う結合テストは Unix のみ）：

```powershell
cargo test -p recorder -p transcriber
```

### 3. アプリ

```powershell
npm run tauri dev       # 開発
npm run tauri build     # インストーラ (NSIS) 作成
```

## フロントエンドとのインターフェース

| 種類 | 名前 | 内容 |
|---|---|---|
| command | `get_devices` | 既定マイク / 既定出力の名前・ネイティブ形式 |
| command | `start_recording` | 録音開始。保存先フォルダとデバイスを返す |
| command | `stop_recording` | 停止・ファイル確定し、文字起こしキューに追加。会議情報を返す |
| command | `retry_transcription` | 失敗した会議の文字起こしを再実行 |
| command | `list_meetings` | この起動中に録音した会議と状態 |
| command | `get_meeting` | 会議情報と全文 (transcript.json) |
| command | `open_folder` | 会議フォルダをエクスプローラーで開く |
| command | `get_paths` | 保存先・設定ファイルのパス |
| event | `recording-progress` | 100 ms ごと。`elapsed_ms`, `mic_level`, `system_level`（直近のピーク 0〜1）, `errors` |
| event | `transcription-status` | 文字起こしの状態変化。`status.state` = `queued` / `transcribing`(+`progress`) / `done` / `failed`(+`error`) |

録音中にウィンドウを閉じた場合も、終了時に WAV を確定させる。

## 既知の制限

- メイン画面の一覧は「この起動中に録音した会議」のみ。meetings フォルダ全体からの会議一覧は未実装。
- 会議タイトルは仮に「10月13日 10:30 の会議」と表示している（要約の実装時にタイトルを生成する想定）。
- 要約（自分の TODO・決定事項・持ち越し）は枠のみ。
- フォントは IBM Plex Sans JP / Mono をアプリに同梱している（オフラインでも表示が崩れないように）。

- システム音声の録音は Windows (WASAPI) のみ。他 OS では開始時にエラーになる。
- 録音中に既定デバイスを切り替えても追従しない（録音開始時のデバイスのまま）。
- デバイスのクロックと PC のクロックのずれ（数十 ppm 程度）は補正していない。1 時間で 0.1〜0.2 秒程度のずれになりうる。
