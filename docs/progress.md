# 進捗の記録

> 最終更新: 2026-10-21。[完成までの手順](roadmap.md)の段階1〜4を進める作業の途中経過。使用量の上限が近いため、段階1の途中で中断した。

## 進め方

- 段階ごとに統合用のブランチを作る。段階1は `feat/stage1-tech-validation`。
- 項目ごとに作業ツリー（`.claude/worktrees/<名前>`、ブランチ `wip/<名前>`）を分け、Sonnet 5.5 のサブエージェントに並列で実装させる。難しさに応じてエフォートを振り分けた（描画と音声は high、FFmpeg同梱は medium）。
- 終わった項目は統合用のブランチに `--no-ff` でマージする。

## 段階1の状況

| 項目 | 状態 | ブランチ |
| --- | --- | --- |
| 1. Windows・Linuxの実機、HDMIの外部ディスプレイでの確認 | 未着手。この環境（macOS）ではできないため、実機での確認が要る | — |
| 2. キャプション、ウォーターマーク、トランジション、日本語の文字描画 | 作業途中で中断。未完成のまま `wip: …` として保存した。fmt・clippy・テストが通るかは未確認 | `wip/s1-render` |
| 3. 動画への書き出しの試作 | 未着手。2番のオフスクリーン描画を使う予定 | — |
| 4. 管理画面の中にプレビューを出す方式 | 未着手。2番のオフスクリーン描画を使う予定 | — |
| 5. BGM、動画中にBGMを下げる処理、音ズレ補正 | 作業途中で中断。コミット済みは2件（音ズレ補正と生成音のミキサーへの組み込み、BGM・動画中の音量の下げ・テスト音）。テストの追加と文書は途中 | `wip/s1-audio` |
| 6. LGPL版FFmpegの同梱 | 済み（macOS aarch64で確認）。統合用のブランチにマージ済み。詳細は[FFmpegの同梱の技術検証](spike-ffmpeg-bundling.md) | マージ済み |
| 7. 技術選定への反映 | 未着手 | — |

### 2番（描画）の途中の内容

`wip/s1-render` には次のファイルがある。設計の途中であり、どこまで動くかは確かめていない。

- 合成の切り出し: `compositor.rs`、`offscreen.rs`（オフスクリーン描画と読み戻し）、`scene.rs`
- トランジション: `transition.rs`、`transition_engine.wgsl`、`transitions/`
- 文字とキャプション: `text.rs`、`caption.rs`、`overlay.rs`、`overlay.wgsl`
- ウォーターマーク: `watermark.rs`
- ぼかし背景: `blur.wgsl`
- テストと見本: `render_tests.rs`、`examples/`
- フォント: `assets/fonts/NotoSansJP-VariableFont_wght.ttf`（可変フォント、約9.6MB）と `OFL.txt`

### 6番（FFmpeg）で決めたこと

- ffmpeg.org公式のソース（9.0.2）を、外部ライブラリなし・LGPLの構成で自前ビルドする（`scripts/build-ffmpeg.sh`）。BtbNの配布物は使わない。
- `externalBin` はリリース時だけ `src-tauri/tauri.bundle.conf.json` を重ねて指定する（`npm run tauri:release`）。通常の設定に入れると開発時のビルドでも実ファイルが要るため。
- 同梱するファイル名は `fluxlide-ffmpeg` / `fluxlide-ffprobe`（debで `/usr/bin` に置かれるときの衝突を避けるため）。
- LinuxのH.264書き出しの方式と、ライセンス・特許の最終判断は未決（spike文書の5.4）。
- Windows、Linux、x86_64のmacOSでのビルドと、CIのワークフローは一度も動かしていない。

## 再開するときの手順

1. `wip/s1-audio` の残り（テスト、実機での計測、`docs/spike-bgm.md`）を仕上げ、チェックを通して統合用のブランチにマージする。
2. `wip/s1-render` の残り（チェックを通す、見本画像で見た目を確認する、`docs/spike-captions-transitions.md`）を仕上げてマージする。`slideshow.rs` は音声側も触っているので、マージ時の衝突に注意する。
3. 3番（書き出し）と4番（プレビュー）を、2番のオフスクリーン描画を使って進める。
4. 7番で技術選定を確定し、段階2へ進む。
