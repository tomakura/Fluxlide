# Fluxlide

**写真・動画を、自由な演出で。**  
**Slideshow Creation & Playback Software**

Fluxlide（フラックスライド）は、写真・動画・キャプション・ウォーターマーク・カスタムトランジションを組み合わせ、**外部ディスプレイへのリアルタイム全画面再生**と**動画ファイルへの書き出し**を両立することを目指す、クロスプラットフォームのスライドショー制作・再生ソフトウェアです。

> **開発ステータス: 実装初期**
>
> アプリの雛形と描画エンジンの土台ができ、方式を確かめる技術検証（[段階1](docs/roadmap.md)）を終えた段階です。スライドショーとして使える機能はまだ実装していません。以下は実装済み機能の一覧ではなく、開発予定の内容です。

## 主な機能（開発予定）

- **写真・動画をまとめて管理** — メディアごとにタイトル・作者・詳細を登録して、表示順や表示時間を設定
- **安全な素材の取り込み** — 追加したファイルをアプリ管理フォルダに自動コピー。取り込みキュー表示中でもキャプションを入力可能
- **自由な演出** — キャプション、常時表示ウォーターマーク、カスタムトランジション、アニメーション
- **BGMと音声調整** — BGM再生と動画中の自動フェードアウト／終了後のフェードイン、Bluetooth向けミリ秒単位の同期補正
- **独立した全画面出力** — OBS不要。管理ウィンドウで操作しながら、指定した外部ディスプレイへ映像のみ出力
- **書き出しと再利用** — MP4等への動画書き出し、プロジェクトの保存と再編集
- **再生制御** — 自動再生、ループ、シャッフル、手動切り替え

展示会、イベント、デジタルサイネージ、プレゼンテーションなどでの利用を想定しています。

## 対応予定プラットフォーム

| OS | 対応方針 |
| --- | --- |
| Windows | x86_64 |
| Linux | x86_64（一般的なデスクトップ環境） |
| macOS | Apple Silicon / Intel |

3OSを初期対応の目標とします。現時点では各OSでの動作を保証するビルドはありません。

## 技術スタック（採用予定）

| 役割 | 技術 |
| --- | --- |
| デスクトップアプリ | Tauri 2 |
| フロントエンド | Svelte 5 / TypeScript / Vite |
| バックエンド | Rust |
| 映像描画エンジン | **Rust + wgpu / WGSL**（ネイティブGPU描画） |
| エンジン構成 | **UIから独立したRustクレート** |
| 上映ウィンドウ | 専用ネイティブウィンドウ（Tauriとの直接連携を先に検証。必要に応じてwinit） |
| メディア処理・書き出し | FFmpeg（同梱条件を含め検討） |
| プロジェクトデータ | SQLite（候補） |
| ビルド・配布 | GitHub Actions / GitHub Releases |

**Tauri / Svelteは管理・編集UI、Rust / wgpuは映像レンダリング専用**に分離します。GPU描画はWebGL2ではなく最初からネイティブ実装とし、リアルタイム表示と動画書き出しで同じシーン定義・wgpuレンダラーを共有する方針です。映像ウィンドウの方式、動画デコード、音声同期などのOS固有部分は初期の技術検証で確認します。

## ドキュメント

- [ソフトウェア概要・機能要件](docs/overview.md)
- [技術選定・アーキテクチャ・自動ビルド／リリース方針](docs/tech-stack.md)
- [画面草案・ロゴ](docs/ui-draft.md)
- [完成までの手順](docs/roadmap.md)
- [進捗の記録](docs/progress.md)
- [スライドショーのデータ構造](docs/scene-model.md)
- [技術検証: 上映ウィンドウ](docs/spike-presentation-window.md)
- [技術検証: 写真と動画の連続再生](docs/spike-media-playback.md)
- [技術検証: 動画の音声と映像の同期](docs/spike-audio-sync.md)
- [技術検証: FFmpegの同梱とライセンス](docs/spike-ffmpeg-bundling.md)
- [技術検証: キャプション、ウォーターマーク、トランジション、日本語の文字描画](docs/spike-captions-transitions.md)
- [技術検証: BGM、動画中のBGMの下げ、音ズレ補正](docs/spike-bgm.md)
- [技術検証: 動画への書き出し](docs/spike-export.md)
- [技術検証: 管理画面の中のプレビュー](docs/spike-preview.md)
- [Linuxでの動作の確認（仮想ディスプレイ）](docs/check-linux.md)
- [第三者のライセンス表記（下書き）](THIRD_PARTY_LICENSES.md)

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

### よく使うコマンド

```bash
npm install          # 依存関係のインストール
npm run tauri dev    # 開発用に起動
npm run check        # フロントエンドの型チェック
cargo test --workspace
npm run ffmpeg:build # 同梱用のFFmpegをビルド（初回と、スクリプトの変更後）
npm run tauri:release # FFmpegを同梱したインストーラーの作成
npm run tauri build  # FFmpegなしのインストーラーの作成
```

アプリアイコンは `assets/brand/fluxlide-app-icon.svg` から生成しています。ロゴを変えたときは `npx tauri icon assets/brand/fluxlide-app-icon.svg` で作り直し、モバイル用の `src-tauri/icons/android` と `src-tauri/icons/ios` は削除します。

### CI・リリース

- `main` へのpushとPull Requestで、Windows / Linux / macOSのチェックとテストを実行します（`.github/workflows/ci.yml`）。
- `scripts/build-ffmpeg.sh` や関連するワークフローを変えたときに、3OS（macOSはAppleとIntel）で同梱用のFFmpegのビルドを確かめます（`.github/workflows/ffmpeg.yml`、未検証）。
- `v*` タグのpushで、FFmpegのビルド、3OSのビルド、GitHub Releasesへのアップロードを行います（`.github/workflows/release.yml`、未検証）。同梱のFFmpegは `src-tauri/tauri.bundle.conf.json` を重ねて追加します。一般配布には各OSの署名・公証等の整備が必要になる場合があります。

## ライセンス

Fluxlideのリポジトリは [MIT License](LICENSE) のもとで公開されています。

FFmpegを含む外部ライブラリ・コーデック等には、それぞれ別のライセンスや配布上の条件が適用されます。同梱するFFmpegはLGPL v2.1以降です（[第三者のライセンス表記](THIRD_PARTY_LICENSES.md)）。H.264などの特許の扱いは[FFmpegの同梱とライセンス](docs/spike-ffmpeg-bundling.md)にまとめています。
