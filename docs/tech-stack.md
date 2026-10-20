# Fluxlide — 技術構成・ビルド／リリース方針

> 2026年10月時点の技術選定。実装前の設計案であり、各OSでの動作・再生品質は未検証。

## 1. 結論

**Tauri 2 + Svelte 5 + TypeScript + Vite + Rust** を基本構成として採用する。

Windows / Linux / macOSを初期の対象にしつつ、メモリ・配布容量・UI開発効率のバランスを優先する。独立出力ウィンドウとGitHub Actionsによるビルド・配布を前提とする。

## 2. 選定理由と比較

| 候補 | 主な利点 | 注意点 | 判断 |
| --- | --- | --- | --- |
| **Tauri 2 + Svelte 5** | OS標準WebViewを活用し配布物を小さくしやすい。UIをWeb技術で開発できる | OSごとにWebView・コーデックの差がある。Rustとの連携が必要 | **採用案** |
| Tauri 2 + React | 豊富なコンポーネント・ライブラリ | Svelteと比較しUIランタイムの負荷が増える可能性 | 代替候補 |
| Electron | Chromium統一による描画・メディア動作の予測可能性、巨大なエコシステム | Chromiumを同梱するため配布容量・RAM面で不利 | 今回は非優先 |
| Qt 6 + QML | Qt Multimedia、ネイティブ描画・映像処理との親和性 | UI・配布・ライセンス管理の学習負荷、Qtライブラリ同梱 | **動画性能・互換性が不足する場合の再検討先** |
| Flutter | まとまったUI基盤、3OS対応 | 高度な動画・GPU処理はネイティブプラグイン等が必要 | 今回は非優先 |

※ ◎／○などの定量的な優劣ではなく、Fluxlideの要件に対する定性的な設計判断。

**なぜSvelteか:** UIをコンパイルして小さなJavaScriptへ変換でき、素材管理画面・タイムライン・プロパティ編集を比較的少ないボイラープレートで作れる。SvelteKitのサーバー機能は不要なため、フロントエンドは**Svelte + Vite**の単一ページアプリを想定する。

## 3. 責務分離

| 層 | 役割 |
| --- | --- |
| UI（Svelte / TypeScript） | メディア管理、キャプション入力、取り込みキュー、プレビュー操作、編集画面 |
| Tauri / Rust | ウィンドウとモニター制御、ファイル取り込み、キュー、プロジェクト保存、外部ツール起動 |
| プロジェクトデータ（SQLite候補） | メディア情報、順序、タイムライン、エフェクト・音声設定 |
| レンダラー（WebGL2候補） | 写真・動画・キャプション・ウォーターマークの合成、トランジション |
| メディア処理／書き出し（FFmpeg候補） | 動画のデコード・エンコード、音声処理。必要に応じ専用プロセス化 |

編集ウィンドウと上映用ウィンドウは分離し、**上映中は管理UIを映像に重ねない**。レンダラーに共通シーン定義と時刻を与え、リアルタイム出力とオフライン書き出しで表現を共有する。

**注意:** FFmpegを呼ぶだけではWebGL描画と一致した書き出しにはならない。映像フレームを共通レンダラーから供給する仕組みが必要になるため、実装初期に検証する。

## 4. パフォーマンスと互換性の注意点

- Tauriが使うWebViewはWindowsでWebView2、macOSでWKWebView、LinuxでWebKitGTK。動画のコーデック・ハードウェアデコード・WebGL実装が同一ではない。
- まずWebGL2による共通シーン描画を試し、実機で要件を満たせない場合はRust + ネイティブ描画（例: wgpu）やQt系を再評価する。
- Linuxでは特にディストリビューションのメディア依存関係、Wayland/X11のウィンドウ位置制御・全画面化・マルチモニターを確認する。
- Bluetooth音声の同期補正は、正方向の音声先行を魔法のように実現する機能ではない。映像のバッファリング／遅延とサウンドの再生時刻を合わせる設計が必要。
- 素材コピーはバックグラウンドで管理し、コピー完了を確認してから参照を確定する。元データを削除しない。
- エクスポートの負荷、メモリ消費、素材先読みの制限は実測値で調整する。「Tauriだから動画処理も必ず軽い」という前提は置かない。
- FFmpegの同梱、GPL/LGPL条件、使用するエンコーダや特許・配布条件はリリース前に確認する。

## 5. GitHub Actions 方針

[公式 Tauri GitHub Action](https://github.com/tauri-apps/tauri-action) を使用する。

| 環境 | 初期の対象 |
| --- | --- |
| Windows runner | x86_64 Windows |
| Linux runner | x86_64 Linux（Ubuntu 22.04ベース） |
| macOS runner | Apple Silicon（aarch64）とIntel（x86_64） |

- **開発用ビルド:** アプリの雛形が入った後に、`main`へのpushとPull Requestで3OSビルドを検証するワークフローを追加する。
- **リリース用ビルド:** `v0.1.0`のようなバージョンタグのpush時に3OSをビルドし、GitHub Releasesに成果物をアップロードする。` .github/workflows/release.yml` に設定する。
- **対象配布形式:** Windowsのインストーラー、macOSのDMG等、LinuxのAppImage／deb等（実際に生成できる形式はTauri設定で確定）。
- **署名・公証:** Windowsコード署名、macOS署名・notarizationは別途設定が必要。未署名の初期配布ではOSの警告が出る場合がある。
- **現状:** アプリのソース、パッケージ設定、依存関係がまだないため**ビルド／リリースは未実行・未検証**。リリースタグを付けるのは初期アプリが構築されてから。

## 6. 最初に検証する技術課題

1. Windows / Linux / macOSでメディア素材を読み込み、動画を連続再生できるか。
2. メイン画面操作中に外部モニターへ出力を続けられるか。
3. カスタムトランジション・キャプション・透かしを両モードで表示できるか。
4. 音声同期とBGMフェードを破綻なく扱えるか。
5. リアルタイムと動画書き出しの表示差、CPU/GPU/RAMの実測。

上記を通過してから映像処理エンジンを固定する。

## 7. 参考資料

- [Tauri公式: GitHub Actions](https://v2.tauri.app/distribute/pipelines/github/)
- [Tauri公式: WebViewの違い](https://v2.tauri.app/reference/webview-versions/)
- [Tauri公式: 外部バイナリ同梱](https://tauri.app/develop/sidecar/)
- [Tauri公式: ウィンドウAPI](https://v2.tauri.app/reference/javascript/api/namespacewebviewwindow/)
- [Svelte公式ドキュメント](https://svelte.dev/docs/svelte/overview)
- [FFmpegライセンス](https://www.ffmpeg.org/legal.html)
- [Qt Multimedia](https://doc.qt.io/qt-6/qtmultimedia-index.html)
