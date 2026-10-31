# リリースの手順

> 作成日: 2026-10-22。[完成までの手順](roadmap.md)の段階7。リリースは `.github/workflows/release.yml` で作り、いつも**下書き**の GitHub Release になる。公開は、下書きのインストーラーを確かめてから手で行う。

## 1. 手順

1. 版を上げる。`src-tauri/tauri.conf.json`、`package.json`、ルートの `Cargo.toml`（`[workspace.package]`）の `version` を同じ値にし、`cargo update -w` と `npm install --package-lock-only` で lock を合わせる
2. `main` にマージする
3. 同じmainコミットの3OS CI成功を確認し、GitHubのActions →「Release」→「Run workflow」で版（`1.0.1`のように`v`を付けない）を入れる。タグ起動もできる。全マニフェスト・lock・タグの版一致と、対象が現在のmainであることを必須にする
4. 4つのビルド（Windows、Linux、macOS Apple Silicon、macOS Intel）が終わると、下書きのリリースにインストーラーが付く
5. 下書きのインストーラーを各 OS で入れて、[手動の確認項目](manual-checks.md)を確かめる
6. 下書きの説明を見直し、「Publish release」で公開する。このときタグが作られる

ワークフローがすること:

- 同梱する FFmpeg を、`scripts/build-ffmpeg.sh` で LGPL の構成でビルドする
- `npm run licenses`（`scripts/third-party-notices.mjs`）で、Rust と npm の依存のライセンス文を `target/notices/THIRD_PARTY_NOTICES.txt` に集める
- `src-tauri/tauri.bundle.conf.json` を重ねて、FFmpeg とライセンス文をパッケージに入れる
- 署名の secrets があれば署名する（2章）
- 版ごとに実行を直列化し、全ターゲットで1つの下書きを共有する。同名の公開版や別コミットの下書きがあれば止める
- `docs/releases/<版>.md`を説明に使い、対応するFFmpeg・アプリのソースとビルド手順を添付する
- Linuxはdebのインストールと仮想画面での起動、WindowsはMSI展開後の同梱FFmpeg起動、macOSはバンドル配置を検査する
- 9インストーラーと4ソース・手順ファイルを照合し、`SHA256SUMS`と`release-provenance.json`を追加する。すべて成功してから公開する

## 2. 署名

証明書がなくてもリリースは作れる。そのときのインストーラーは署名なしで、初めて開くときに OS が警告を出す（Windows の SmartScreen、macOS の Gatekeeper）。

### macOS（署名と公証）

リポジトリの secrets に次を入れると、tauri のバンドラが署名し、Apple の公証に出す。`APPLE_CERTIFICATE` がなければ署名しない。

| secret | 中身 |
| --- | --- |
| `APPLE_CERTIFICATE` | Developer ID Application の証明書を書き出した `.p12` を base64 にしたもの |
| `APPLE_CERTIFICATE_PASSWORD` | `.p12` のパスワード |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: 名前 (TEAMID)` |
| `APPLE_ID` | 公証に使う Apple ID |
| `APPLE_PASSWORD` | その Apple ID のアプリ用パスワード |
| `APPLE_TEAM_ID` | チーム ID |

同梱の `fluxlide-ffmpeg`、`fluxlide-ffprobe` もバンドラが署名する。hardened runtime で FFmpeg が動くかは、署名した版でまだ確かめていない。

### Windows（コード署名）

| secret | 中身 |
| --- | --- |
| `WINDOWS_CERTIFICATE` | コード署名の証明書（`.pfx`）を base64 にしたもの |
| `WINDOWS_CERTIFICATE_PASSWORD` | `.pfx` のパスワード |

ワークフローが証明書を取り込み、拇印を `src-tauri/tauri.windows-sign.conf.json` に書いてバンドラに渡す。タイムスタンプは DigiCert のサーバーを使う。クラウドの署名（Azure Trusted Signing など）を使うときは、`bundle.windows.signCommand` に変える。

## 3. 配布で決めたこと

| 項目 | 決定 |
| --- | --- |
| FFmpeg の LGPL | ライブラリとしてはつながず、改変しない FFmpeg を別のプログラム（`fluxlide-ffmpeg`、`fluxlide-ffprobe`）として同梱する。利用者は自分でビルドしたものに差し替えられる。ライセンス文、ビルドの記録（`BUILD-<target>.txt`）を `licenses/ffmpeg/` に入れ、アプリの「このアプリについて」にも出す |
| FFmpeg のソース | バイナリと同じGitHub Releaseに、ビルドに対応するソースtarball、SHA-256、`build-ffmpeg.sh`を添付する。Fluxlideのソースも添付し、GLibの修正を含めて再現できるようにする |
| 特許 | H.264、HEVC、AAC のデコーダを含む FFmpeg をそのまま配布する。リリースの説明、README、THIRD_PARTY_LICENSES.md、アプリの「このアプリについて」に、特許の許諾を含まず、事業で使うときは各自で確かめるよう書く |
| 依存のライセンス | アプリが使う Rust のクレートと、画面に入る npm のパッケージのライセンスファイルを、リリースのたびに集めて同梱する |

## 4. 確かめたこと

2026-10-22 に「Run workflow」で v1.0.0 を作った。4つのビルドが通り、下書きに9つのインストーラーが付いた（Windows の `.msi`・`-setup.exe`、macOS の `.dmg`・`.app.tar.gz` を Apple Silicon と Intel で、Linux の `.AppImage`・`.deb`・`.rpm`。大きさは 28〜37 MiB、AppImage だけ 105 MiB）。

- `.deb` には `usr/bin/` に `fluxlide`、`fluxlide-ffmpeg`、`fluxlide-ffprobe` が並び、`usr/lib/Fluxlide/` に `licenses/` と `sample/` が入っていた
- macOS の `.app` には `Contents/MacOS/` に同じ3つ、`Contents/Resources/` に `licenses/` と `sample/` が入っていた
- アプリは FFmpeg を自分の実行ファイルと同じフォルダで探すので、どちらでも見つかる

## 5. v1.0.1以降の検証範囲

v1.0.0は2026-10-28（日本時間）に公開済み。v1.0.1では上記の配布物検査を追加した。CIの仮想環境と物理機器の確認を分け、[手動の確認記録](manual-checks.md)に結果を残す。AppImage/RPM・NSISの個別インストール、Intel Macでの起動、機器切替、署名・公証された版の動作は、この検査だけでは確認できない。

現在は証明書を追加せず、未署名配布を継続する。OSの警告が出ないとは案内しない。macOSの警告を解除して起動する操作は実機未確認であり、利用者のセキュリティ設定を一括で無効化する手順は案内しない。
