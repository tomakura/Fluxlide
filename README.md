# Fluxlide

**写真・動画を、自由な演出で。**  
**Slideshow Creation & Playback Software**

Fluxlide（フラックスライド）は、写真・動画・キャプション・ウォーターマーク・カスタムトランジションを組み合わせ、**外部ディスプレイへのリアルタイム全画面再生**と**動画ファイルへの書き出し**を両立することを目指す、クロスプラットフォームのスライドショー制作・再生ソフトウェアです。

> **開発ステータス: 実装初期**
>
> アプリの雛形と描画エンジンの土台ができた段階で、スライドショーの機能はまだ実装していません。以下は実装済み機能の一覧ではなく、開発予定の内容です。

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
- [技術検証: 上映ウィンドウ](docs/spike-presentation-window.md)

## 開発・ビルドについて

### 必要なもの

- Node.js（LTS）と npm
- Rust（stable）
- [Tauri 2 の前提パッケージ](https://tauri.app/start/prerequisites/)（LinuxではWebKitGTKなど）

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
npm run tauri build  # インストーラーの作成
```

アプリアイコンは `assets/brand/fluxlide-app-icon.svg` から生成しています。ロゴを変えたときは `npx tauri icon assets/brand/fluxlide-app-icon.svg` で作り直し、モバイル用の `src-tauri/icons/android` と `src-tauri/icons/ios` は削除します。

### CI・リリース

- `main` へのpushとPull Requestで、Windows / Linux / macOSのチェックとテストを実行します（`.github/workflows/ci.yml`）。
- `v*` タグのpushで3OSのビルドとGitHub Releasesへのアップロードを行います（`.github/workflows/release.yml`、未検証）。一般配布には各OSの署名・公証等の整備が必要になる場合があります。

## ライセンス

Fluxlideのリポジトリは [MIT License](LICENSE) のもとで公開されています。

FFmpegを含む外部ライブラリ・コーデック等には、それぞれ別のライセンスや配布上の条件が適用されます。
