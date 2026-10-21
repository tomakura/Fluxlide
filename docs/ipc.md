# 管理画面と描画エンジンのやり取り

> 決定日: 2026-10-22。[完成までの手順](roadmap.md)の段階2の4番。コマンドの一覧は [`src-tauri/src/lib.rs`](../src-tauri/src/lib.rs) の `generate_handler!`、画面側から呼ぶ関数は [`src/lib/api.ts`](../src/lib/api.ts) にある。

## 1. 決めたこと

| 項目 | 決定 | 理由 |
| --- | --- | --- |
| 定義を持つ側 | Rust 側（`Session`）が持つ。画面は返ってきた状態を表示するだけにする | 取り込み、自動保存、元に戻す、上映が同じ定義を見る。画面と Rust で2つの定義が食い違わない |
| 変更の送り方 | 変更した定義を**丸ごと**送る（`edit_show`）。返事は変更後の `ShowState` | 定義は写真の画素を含まず小さい。変更の種類ごとにコマンドを作るより少なく済み、元に戻すも丸ごとの定義で扱える |
| 古い定義からの変更 | `ShowState` は変更のたびに増える `revision` を持つ。`edit_show` には変更の元にした `revision` を一緒に送り、今の値と違えば適用しない（`applied: false` と今の状態を返す）。画面は今の状態に同じ変更をやり直して送り直す | 取り込みで足したスライドなど、画面がまだ知らない変更を、古い定義で上書きして消さないようにする |
| 元に戻すのまとめ方 | `edit_show` の `group` に、変えている入力欄の名前を渡す（例: `meta:3:title`）。同じ名前の変更が続く間は1回にまとめる | 入力欄やスライダーの1回の操作を、1回で戻せるようにする |
| 頼まれていない変更 | 取り込みの完了などで定義が変わったら、`show-changed` イベントで `ShowState` を送る | 画面が状態を問い合わせ続けなくて済む |
| 途中経過 | 取り込みの進み具合は `import-progress` で送る。定義には入れない | 保存しない、取り込みの間だけの情報（[保存と取り込み](project-storage.md)） |
| 上映の様子 | 今までどおり、画面が `presentation_status` を0.5秒ごとに問い合わせる | 毎フレーム変わる数字を、画面が使う頻度に合わせて取れる |
| 型 | Rust の型から ts-rs で TypeScript の型を作る（`src/lib/bindings/`）。手では書かない | Rust と画面の型が食い違わない。JSON の名前（camelCase、`kind`）も serde の指定から作られる |
| 大きな整数 | `u64` と `i64` は TypeScript の `number` にする | 番号や時刻は `Number.MAX_SAFE_INTEGER` より十分小さい。`bigint` は JSON で送れない |

## 2. 型の作り直し

`cargo test --workspace` を実行すると、`#[ts(export)]` を付けた型が `src/lib/bindings/` に書き出される（設定は `.cargo/config.toml`）。Rust の型を変えたら、試験を実行してできたファイルも一緒にコミットする。CI は試験のあとに `src/lib/bindings/` に差分がないかを確かめ、作り直し忘れを見つける。

## 3. コマンドとイベント

| 種類 | 名前 | 内容 |
| --- | --- | --- |
| 作品 | `list_projects`、`create_project`、`open_project`、`close_project`、`show_state` | [保存と取り込み](project-storage.md)の5章 |
| 編集 | `edit_show`、`undo_show`、`redo_show` | 同上 |
| 取り込み | `import_files`、`replace_asset_file` | 同上 |
| 上映 | `list_monitors`、`start_presentation`、`stop_presentation`、`presentation_status` | 上映ウィンドウ |
| プレビュー | `start_preview`、`stop_preview`、`preview_frame` | 画面外に描いた1枚を返す（[プレビューの技術検証](spike-preview.md)） |
| 状態 | `engine_status`、`video_decoder_status` | GPU と FFmpeg が見つかったか |
| イベント | `show-changed` | 頼まれていない定義の変更。`ShowState` |
| イベント | `import-progress` | 取り込みの進み具合。`ImportProgress` |

上映とプレビューは今のところ、画面から定義を受け取っている。段階3で開いている作品の定義を読む形に変える。
