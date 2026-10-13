# Meeting Recorder

Windows 用の会議録音アプリ（Tauri 2 + React + TypeScript）。
最終形は「録音 → whisper.cpp で文字起こし → `claude -p` で要約」の振り返りアプリ。現段階は **録音のみ**。

## 構成

```
crates/recorder/   GUI 非依存の録音モジュール (cpal + hound)
  src/devices.rs   既定マイク / 既定出力(ループバック)の取得、デバイス一覧、probe
  src/recorder.rs  Recorder::start / stop、レベルメーター、WAV 書き出し、meta.json
  src/timeline.rs  2 トラックの開始時刻合わせ・無音区間の補完
  src/dsp.rs       ダウンミックス、16 kHz へのリサンプリング（窓関数付き sinc）
  examples/        手順 1・2 用の CLI
src-tauri/         Tauri シェル（コマンドとイベントのみ）
src/               React フロントエンド（メイン画面 1 枚）
```

## 出力

```
<ドキュメント>\MeetingRecorder\meetings\<YYYY-MM-DD_HHMM>\
  mic.wav      既定の入力デバイス
  system.wav   既定の出力デバイス（WASAPI ループバック）
  meta.json    開始時刻(RFC3339)・デバイス・ネイティブ形式・補完した無音秒数
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

ユニットテスト（リサンプラと時刻合わせ）：

```powershell
cargo test -p recorder
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
| command | `stop_recording` | 停止・ファイル確定。保存先フォルダと録音秒数を返す |
| event | `recording-progress` | 100 ms ごと。`elapsed_ms`, `mic_level`, `system_level`（直近のピーク 0〜1）, `errors` |

録音中にウィンドウを閉じた場合も、終了時に WAV を確定させる。

## 既知の制限

- システム音声の録音は Windows (WASAPI) のみ。他 OS では開始時にエラーになる。
- 録音中に既定デバイスを切り替えても追従しない（録音開始時のデバイスのまま）。
- デバイスのクロックと PC のクロックのずれ（数十 ppm 程度）は補正していない。1 時間で 0.1〜0.2 秒程度のずれになりうる。
