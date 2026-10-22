# Fluxlide

**写真・動画を、自由な演出で。**  
**Slideshow Creation & Playback Software**

Fluxlide（フラックスライド）は、写真・動画・キャプション・ウォーターマーク・カスタムトランジションを組み合わせたスライドショーを、**外部ディスプレイに全画面で映し**、**動画ファイルに書き出せる**、クロスプラットフォームのアプリです。展示会、イベント、デジタルサイネージ、プレゼンテーションなどで使えます。

## できること

- **写真・動画を並べる** — ドロップで取り込み、番号の部分をつかんで並べ替え。写真は JPEG・PNG・HEIC、動画は MP4・MOV（H.264・HEVC）。取り込んだファイルは作品のフォルダにコピーするので、元のファイルを動かしても消えません
- **キャプションとウォーターマーク** — タイトル・作者・説明を3つのデザインから選んで表示。文字やロゴのウォーターマーク
- **演出** — フェード、スライド、ズームなどのトランジション、写真のゆっくりズーム、WGSL で書いたカスタムトランジション
- **BGM** — フォルダや曲を選んで流し、音のある動画の間は自動で下げる。Bluetooth のスピーカー向けに、テスト音を聞きながらミリ秒単位で音ズレを補正
- **上映** — 管理画面で操作しながら、選んだ画面に映像だけを全画面で出す。前へ・次へ・一時停止・暗転、プレゼン用のリモコン、自動・手動の進め方、ループ、シャッフル
- **書き出し** — テレビ、動画サイト、スマホ、高画質の用途を選んで MP4 に
- **自動保存と元に戻す** — 変更はすぐ保存され、開いている間は元に戻せます

はじめて使うときは、ホーム画面の「使い方のサンプルを開く」で、操作の説明を映したスライドショーを見られます。

## ダウンロード

[GitHub Releases](https://github.com/tomakura/Fluxlide/releases) から、使う OS のインストーラーを選びます。

| OS | ファイル |
| --- | --- |
| Windows（x64） | `.msi` または `-setup.exe` |
| macOS（Apple Silicon / Intel） | `aarch64` / `x64` の `.dmg` |
| Linux（x64） | `.AppImage`、`.deb`、`.rpm` |

署名していない版では、初めて開くときに OS の警告が出ます。Windows は「詳細情報」→「実行」、macOS は Finder で右クリック →「開く」で開けます。

## できないこと（v1.0）

- HDR の動画は色を変換しないので、薄く見えます
- 写真の色プロファイル（Display P3 など）は読まず、sRGB として表示します
- 本物の HEIC のファイル、各 OS の実機、画面と音の出口の抜き差しは、まだ一部しか確かめていません（[手動の確認項目](docs/manual-checks.md)）

## 特許について

H.264、HEVC、AAC などの形式は、国によって特許の対象になります。Fluxlide とその配布物は、特許の許諾を含みません。事業で使うときは、必要な許諾を各自で確かめてください。

## ドキュメント

- [ソフトウェア概要・機能要件](docs/overview.md)
- [画面草案・ロゴ](docs/ui-draft.md)、[画面の作り](docs/screens.md)
- [完成までの手順](docs/roadmap.md)、[進捗の記録](docs/progress.md)
- [品質（機器の抜き差し、長時間の再生、素材の対応範囲）](docs/quality.md)、[手動の確認項目](docs/manual-checks.md)
- [リリースの手順](docs/release.md)
- [技術選定・アーキテクチャ](docs/tech-stack.md)、[データ構造](docs/scene-model.md)、[保存と取り込み](docs/project-storage.md)、[管理画面とのやり取り](docs/ipc.md)
- 技術検証: [上映ウィンドウ](docs/spike-presentation-window.md)、[写真と動画の連続再生](docs/spike-media-playback.md)、[動画の音声と映像の同期](docs/spike-audio-sync.md)、[FFmpegの同梱とライセンス](docs/spike-ffmpeg-bundling.md)、[キャプションとトランジション](docs/spike-captions-transitions.md)、[BGM](docs/spike-bgm.md)、[動画への書き出し](docs/spike-export.md)、[プレビュー](docs/spike-preview.md)、[Linuxでの確認](docs/check-linux.md)
- [第三者のライセンス表記](THIRD_PARTY_LICENSES.md)

## 開発・ビルドについて

### 必要なもの

- Node.js（LTS）と npm
- Rust（stable）
- [Tauri 2 の前提パッケージ](https://tauri.app/start/prerequisites/)（LinuxではWebKitGTKなど。音声出力のためにALSAの開発用パッケージ `libasound2-dev` も要ります）
- 開発中に動画の再生を試す場合は FFmpeg（`ffmpeg` と `ffprobe`）。macOSでは `brew install ffmpeg`。`PATH` にない場所に置く場合は、そのフォルダを環境変数 `FLUXLIDE_FFMPEG_DIR` で指定します。リリース用には、`npm run ffmpeg:build`（`scripts/build-ffmpeg.sh`）でLGPL版を自分でビルドします。ビルドにはCコンパイラ、`make`、`nasm`（x86_64のみ）が要ります。詳しくは[FFmpegの同梱](docs/spike-ffmpeg-bundling.md)を参照してください

### 構成

| パス | 内容 |
| --- | --- |
| `src/` | 管理・編集UI（SvelteKit / Svelte 5 / TypeScript、静的出力のSPA） |
| `src-tauri/` | Tauriアプリ本体（Rust） |
| `crates/fluxlide-engine/` | 描画エンジン（Rust + wgpu）。UIに依存しない |
| `assets/brand/` | ロゴとアプリアイコンの元データ（SVG） |
| `assets/sample/` | 使い方のサンプルの絵と説明（絵は `scripts/make-sample.py` で描く） |

### よく使うコマンド

```bash
npm install          # 依存関係のインストール
npm run tauri dev    # 開発用に起動
npm run check        # フロントエンドの型チェック
npm test             # フロントエンドの試験
cargo test --workspace
npm run ffmpeg:build # 同梱用のFFmpegをビルド（初回と、スクリプトの変更後）
npm run tauri:release # FFmpegを同梱したインストーラーの作成
npm run tauri build  # FFmpegなしのインストーラーの作成
```

アプリアイコンは `assets/brand/fluxlide-app-icon.svg` から生成しています。ロゴを変えたときは `npx tauri icon assets/brand/fluxlide-app-icon.svg` で作り直し、モバイル用の `src-tauri/icons/android` と `src-tauri/icons/ios` は削除します。

### CI・リリース

- `main` へのpushとPull Requestで、Windows / Linux / macOSのチェックとテストを実行します（`.github/workflows/ci.yml`）。
- `scripts/build-ffmpeg.sh` や関連するワークフローを変えたときに、3OS（macOSはAppleとIntel）で同梱用のFFmpegのビルドを確かめます（`.github/workflows/ffmpeg.yml`）。
- リリースは `.github/workflows/release.yml` で作ります。手順と署名の設定は[リリースの手順](docs/release.md)にあります。

## ライセンス

Fluxlideのリポジトリは [MIT License](LICENSE) のもとで公開されています。

FFmpegを含む外部ライブラリ・コーデック等には、それぞれ別のライセンスや配布上の条件が適用されます。同梱するFFmpegはLGPL v2.1以降です（[第三者のライセンス表記](THIRD_PARTY_LICENSES.md)）。アプリのホーム画面の「このアプリについて」からも、ライセンスの全文を開けます。
