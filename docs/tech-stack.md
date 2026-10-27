# Fluxlide — 技術構成・ビルド／リリース方針

> 2026年10月時点の技術選定。2026-10-21に段階1の技術検証の結果を反映し、方式を確定した（6章）。実機での確認は、macOSの一部とLinuxの仮想ディスプレイだけで、Windowsと実際のGPUでの性能は未確認。

## 1. 結論

**Tauri 2 + Svelte 5 + TypeScript + Vite（管理UI）と、Rust + wgpu / WGSL（独立したネイティブ映像エンジン）** を正式な採用方針とする。

Windows / Linux / macOSを初期の対象にしつつ、メモリ・配布容量・UI開発効率のバランスを優先する。映像描画はWebView内のWebGL2ではなく、初期段階からRustのwgpuレンダラーへ集約する。独立出力ウィンドウとGitHub Actionsによるビルド・配布を前提とする。

## 2. 選定理由と比較

| 候補 | 主な利点 | 注意点 | 判断 |
| --- | --- | --- | --- |
| **Tauri 2 + Svelte 5 + Rust/wgpu** | 軽量なWeb UIとOSネイティブのGPU描画を分離できる。レンダラーの共通化が容易 | GPU上の動画テクスチャ処理、ネイティブウィンドウ連携には追加実装が必要 | **採用** |
| Tauri 2 + React | 豊富なコンポーネント・ライブラリ | Svelteと比較しUIランタイムの負荷が増える可能性 | 代替候補 |
| Electron | Chromium統一による描画・メディア動作の予測可能性、巨大なエコシステム | Chromiumを同梱するため配布容量・RAM面で不利 | 今回は非優先 |
| Qt 6 + QML | Qt Multimedia、ネイティブ描画・映像処理との親和性 | UI・配布・ライセンス管理の学習負荷、Qtライブラリ同梱 | **動画性能・互換性が不足する場合の再検討先** |
| Flutter | まとまったUI基盤、3OS対応 | 高度な動画・GPU処理はネイティブプラグイン等が必要 | 今回は非優先 |

※ ◎／○などの定量的な優劣ではなく、Fluxlideの要件に対する定性的な設計判断。

**なぜSvelteか:** UIをコンパイルして小さなJavaScriptへ変換でき、素材管理画面・タイムライン・プロパティ編集を比較的少ないボイラープレートで作れる。SvelteKitのサーバー機能は不要なため、フロントエンドは**Svelte + Vite**の単一ページアプリを想定する。

## 3. 責務分離

| 層 | 役割 |
| --- | --- |
| UI（Svelte / TypeScript / Tauri 2） | メディア管理、キャプション入力、取り込みキュー、プレビュー操作、編集画面 |
| アプリ制御（Tauri / Rust） | UIとエンジンの通信、ファイル取り込み、キュー、プロジェクト保存、モニター管理 |
| プロジェクトデータ（SQLite候補） | メディア情報、順序、タイムライン、エフェクト・音声設定 |
| **ネイティブ映像エンジン（独立Rustクレート）** | シーン・時間軸・描画命令を管理し、UIと独立して実行できる構造 |
| **GPUレンダラー（wgpu / WGSL）** | 写真・動画フレーム・キャプション・ウォーターマークのGPU合成、トランジション。上映・プレビュー・書き出しが同じ `Compositor` で描く。検証結果は[キャプション、ウォーターマーク、トランジション](spike-captions-transitions.md) |
| 上映用ウィンドウ（ネイティブ） | Tauriとの直接統合を先に検証。制約があればwinit製の独立プロセス。macOSでの検証結果は[上映ウィンドウの技術検証](spike-presentation-window.md) |
| 音声制御（Rust） | BGM、動画音声、フェード、映像と音声のタイミング補正。動画音声のmacOSでの検証結果は[動画の音声と映像の同期の技術検証](spike-audio-sync.md)、BGMと音ズレ補正は[BGMの技術検証](spike-bgm.md) |
| メディア処理／書き出し（FFmpeg） | デコード・エンコード。同梱するLGPL版の `ffmpeg` を別プロセスで動かし、パイプでやり取りする。動画デコードのmacOSでの検証結果は[写真と動画の連続再生の技術検証](spike-media-playback.md)、書き出しは[動画への書き出しの技術検証](spike-export.md) |

編集ウィンドウと上映用ウィンドウは分離し、**上映中は管理UIを映像に重ねない**。レンダラーには共通のシーン定義と再生時刻を与え、リアルタイム表示ではGPUサーフェス、動画書き出しではオフスクリーンテクスチャへ描画する。

**レンダラーの責務を分離する理由:** UIのWebViewやTauriの表示ウィンドウを変更しても、シーン解釈・GPU合成・トランジションを作り直さずに済むため。エンコーダにはRustレンダラーが生成したフレームを渡す。FFmpegを呼ぶだけで画面と同じ結果になるわけではないため、GPUからのフレーム読み戻し、ピクセル形式、色管理、音声同期を初期に検証する。

**ウィンドウ統合:** Tauriのネイティブウィンドウ機能やイベントループとの併用には実装上の制約があり得る。まず統合可能性を試し、難しければwinitを使った専用の上映プロセスとTauri間をIPCで接続する。どちらでも描画コアの独立性を維持する。

## 4. パフォーマンスと互換性の注意点

- **WebGL2 / GLSLは映像エンジンとして採用しない。** UI用WebViewがOSごとに異なることは許容し、再生・GPU合成はRust + wgpuへ集約する。
- wgpuの主なバックエンドはWindowsのDirect3D 12 / Vulkan、macOSのMetal、LinuxのVulkanなど。実際の利用可否、GPU要件、パフォーマンスはOS・GPU・ドライバで検証する。
- GPU描画と動画デコード／音声再生は別工程。FFmpeg等との連携、テクスチャ転送、フレーム同期が必要になる。
- UI画面と上映画面のウィンドウ管理はTauriのみで完結できるとは限らないため、winit製の独立プロセスも設計上の選択肢とする。
- Linuxでは特にディストリビューションのメディア依存関係、Wayland/X11のウィンドウ位置制御・全画面化・マルチモニターを確認する。
- Bluetooth音声の同期補正は、正方向の音声先行を魔法のように実現する機能ではない。映像のバッファリング／遅延とサウンドの再生時刻を合わせる設計が必要。
- 素材コピーはバックグラウンドで管理し、コピー完了を確認してから参照を確定する。元データを削除しない。
- エクスポートの負荷、メモリ消費、素材先読みの制限は実測値で調整する。「Tauriだから動画処理も必ず軽い」という前提は置かない。
- FFmpegの同梱、GPL/LGPL条件、使用するエンコーダや特許・配布条件は[FFmpegの同梱とライセンスの技術検証](spike-ffmpeg-bundling.md)にまとめた。LGPLのビルドを `externalBin` で同梱する。特許については最終判断が残っている。

## 5. GitHub Actions 方針

[公式 Tauri GitHub Action](https://github.com/tauri-apps/tauri-action) を使用する。

| 環境 | 初期の対象 |
| --- | --- |
| Windows runner | x86_64 Windows |
| Linux runner | x86_64 Linux（Ubuntu 22.04ベース） |
| macOS runner | Apple Silicon（aarch64）とIntel（x86_64） |

- **開発用ビルド:** アプリの雛形が入った後に、`main`へのpushとPull Requestで3OSビルドを検証するワークフローを追加する。
- **リリース用ビルド:** `v0.1.0`のようなバージョンタグのpush時に3OSをビルドし、GitHub Releasesに成果物をアップロードする。`.github/workflows/release.yml` に設定する。
- **対象配布形式:** Windowsのインストーラー、macOSのDMG等、LinuxのAppImage／deb等（実際に生成できる形式はTauri設定で確定）。
- **署名・公証:** Windowsコード署名、macOS署名・notarizationは別途設定が必要。未署名の初期配布ではOSの警告が出る場合がある。
- **現状:** アプリのソース、パッケージ設定、依存関係がまだないため**ビルド／リリースは未実行・未検証**。リリースタグを付けるのは初期アプリが構築されてから。

## 6. 技術検証の結果と確定した方式

段階1で、次の5つの技術課題を検証した。

1. Windows / Linux / macOSでメディア素材を読み込み、動画を連続再生できるか。
2. メイン画面操作中に外部モニターへ出力を続けられるか。
3. カスタムトランジション・キャプション・透かしを両モードで表示できるか。
4. 音声同期とBGMフェードを破綻なく扱えるか。
5. リアルタイムと動画書き出しの表示差、CPU/GPU/RAMの実測。

**レンダラーはRust + wgpuで確定**した。各課題について確定した方式は次の通り。

| 課題 | 確定した方式 | 確認できたこと | 残っている確認 |
| --- | --- | --- | --- |
| 1. 連続再生 | 写真は `image` で読み、動画は `ffmpeg` で生のYUVに変換してパイプで受け取り、GPUでRGBに変換する。次の素材は裏で先読みする | macOSの上映ウィンドウ、Linuxの仮想ディスプレイで、写真と動画をつないで再生できた（[連続再生](spike-media-playback.md)、[Linux](check-linux.md)） | Windows。実際のGPUを持つLinux。HEVC・HDRなどの素材（段階6） |
| 2. 上映ウィンドウ | webviewのないTauriのウィンドウに、wgpuの描画面を直接作る。winitの別プロセスは使わない | macOS（Sidecar）、Linux（X11の仮想ディスプレイ）で全画面表示とEscでの終了ができた（[上映ウィンドウ](spike-presentation-window.md)） | HDMIの外部ディスプレイ、Windows、Wayland（Escの代わり、GLES） |
| 3. 演出 | 写真の配置、トランジション（利用者がWGSLで書ける）、キャプションとウォーターマークの重ね描きを `Compositor` にまとめる。文字はcosmic-textと同梱のIBM Plex Sans JPでCPUで描き、GPUで合成する | 自動テストと見本画像で、トランジション、キャプション、ウォーターマーク、日本語の描画を確かめた（[演出](spike-captions-transitions.md)） | 実際のGPUでの速さと見た目 |
| 4. 音声 | cpalで出力し、映像の時計に合わせて混ぜる。BGMはffmpegでデコードし、音のある動画の前に下げる。Bluetooth向けに、利用者が決めるミリ秒で音を早める | macOSで動画の音のずれが最大0.3ms（[音声同期](spike-audio-sync.md)）。BGMと補正は自動テストで確かめた（[BGM](spike-bgm.md)） | 実際の出力機器（特にBluetooth）での耳による確認。Windows・Linuxの出力 |
| 5. 書き出し | 上映と同じ描画を画面外に描き、1フレームずつ進めて、RGBAを `ffmpeg` にパイプで渡す。音は書き出し用のミキサーで混ぜて最後にまとめる | 上映の画面との差はPSNR 42.8dB・SSIM 0.988（libx264）。読み込みが遅れても、ファイルに遅れが残らない（[書き出し](spike-export.md)） | 実際のGPUでの速さ。OSのH.264エンコーダ（VideoToolbox、Media Foundation）での画質。LinuxのH.264の方式 |
| プレビュー | 管理画面のプレビューは、画面外に描いた画素をコマンドの結果として送り、`<canvas>` に描く | Linuxで640×360を約55fpsで表示できた（[プレビュー](spike-preview.md)） | 実際のGPUとWKWebView・WebView2での速さ |

FFmpegは、公式のソースからLGPLの最小構成でビルドして `externalBin` で同梱する（[FFmpegの同梱](spike-ffmpeg-bundling.md)）。ライセンスと特許の最終判断は、リリース前（段階7）に行う。

残っている確認は、どれも方式を変えるほどの問題が見つかる可能性は低いと考え、段階2以降と並行して、実機を持つ人が確かめる。ただし、Windowsで上映ウィンドウに描画面を作れない場合や、実際のGPUで書き出しが実時間より大きく遅い場合は、方式を見直す。

## 7. 参考資料

- [Tauri公式: GitHub Actions](https://v2.tauri.app/distribute/pipelines/github/)
- [Tauri公式: WebViewの違い](https://v2.tauri.app/reference/webview-versions/)
- [Tauri公式: 外部バイナリ同梱](https://tauri.app/develop/sidecar/)
- [Tauri公式: ウィンドウAPI](https://v2.tauri.app/reference/javascript/api/namespacewebviewwindow/)
- [wgpu公式ドキュメント](https://wgpu.rs/)
- [winit公式ドキュメント](https://docs.rs/winit/latest/winit/)
- [Svelte公式ドキュメント](https://svelte.dev/docs/svelte/overview)
- [FFmpegライセンス](https://www.ffmpeg.org/legal.html)
- [Qt Multimedia](https://doc.qt.io/qt-6/qtmultimedia-index.html)
