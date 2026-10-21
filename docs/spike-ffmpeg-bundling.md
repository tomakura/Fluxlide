# 技術検証: FFmpegの同梱とライセンス

> 実施日: 2026-10-21。macOS（Apple Silicon）で、スクリプトによるビルド、エンジンのテスト、.appへの同梱と起動を確かめた。WindowsとLinuxは、CIで動く形にしたが未確認。ライセンスと特許の項は、公式の文書を調べた結果をまとめたもので、法的な助言ではない。

## 1. 目的

[完成までの手順](roadmap.md)の段階1の6番として、次の4点を決める。

- FFmpegに求めるもの（デコードと、段階5の書き出し）
- LGPLのFFmpegを3OS分用意する方法
- Tauriの `externalBin` での同梱のしかたと、アプリが同梱品を見つける仕組み
- LGPLの義務と、H.264などの特許について、配布者に関係しうること

## 2. 方式

### FFmpegに求めるもの

| 用途 | 必要なもの |
| --- | --- |
| 動画のデコード | H.264、HEVC、VP9など、スマホやカメラの一般的な動画。映像は `ffmpeg` が標準出力へ流し、長さと音の有無は `ffprobe` で調べる（`Ffmpeg::locate` は両方を探す） |
| 音声のデコード | 動画の音声と、BGM用の音声ファイル。AAC、MP3、FLAC、Opus、Vorbis、WAV（PCM） |
| 書き出し（段階5） | MP4（H.264 + AAC）。LGPLでは `libx264` が使えないため、AACはFFmpeg内蔵のエンコーダ、H.264はOSのエンコーダ（macOSの `h264_videotoolbox`、Windowsの `h264_mf`）か `libopenh264` になる |
| エンジンのテスト | `lavfi` の入力（`testsrc2`、`sine`）、内蔵の `mpeg4` と `aac` のエンコーダ、`yuv4mpegpipe` と `f32le` の出力 |

内蔵のデコーダで足りるため、デコードのために外部ライブラリは要らない。ただしAV1は、FFmpeg内蔵のデコーダがハードウェア支援専用で、ソフトウェアで復号するには外部ライブラリ（dav1dなど）が要る。今回の構成には入れていない。

### 入手方法の候補

| 候補 | 内容 | 比べた結果 |
| --- | --- | --- |
| BtbN/FFmpeg-Builds の `lgpl` | WindowsとLinuxの配布物がある。日付つきのリリース（`autobuild-YYYY-MM-DD-HH-MM`）を使えば版を固定でき、毎月の最後のビルドは2年間残る。`checksums.sha256` がある。`latest` は毎日動く | 後述の理由で採用しなかった |
| **ffmpeg.orgの公式ソースから、最小の構成でビルドする** | 版とSHA-256を固定した公式のtarballを、外部ライブラリなしで3OSのCIでビルドする | **採用** |
| macOS用の配布物を使う | LGPLの公式ビルドはない。Homebrewの版は `--enable-gpl`（x264、x265）つき | 使えない |

BtbNの `lgpl` 版（`autobuild-2026-09-30-13-08` のLinux x86_64、FFmpeg 9.0.2）を実際に取得して、中身を調べた。SHA-256は `checksums.sha256` と一致した。

- `ffmpeg`、`ffprobe`、`ffplay` が各約140MBある（静的リンク）。同梱する2本で約285MBになる
- configureには、`--enable-gpl` と `--enable-nonfree` はない。一方で `--enable-version3` があり、LGPLv3になる。また、libopenh264、libvpx、libass、libplacebo、libvmaf、librav1e、libsvtav1、libssh、libsrt、openal、libpulse、libxcb、vulkan、opencl、libvpl など数十の外部ライブラリが入る
- 実行ファイルの文字列には、libva、libX11、libxcb、libdrm、libvulkan、libcuda などの名前がある。実行時に必要な共有ライブラリは、この調べ方では確定できていない
- libopenh264がBtbNの手でビルドされている。後述のCiscoの特許ライセンスは、Ciscoが配布したバイナリにしか及ばないため、このH.264エンコーダを書き出しに使うと特許の扱いが変わる

そのため、BtbNは使わず、自分でビルドする。理由は次のとおり。

- 同梱物が小さくなる（後述の結果では1本約21MB）
- 入れる外部ライブラリがなく、配布時に表記する第三者のライセンスがFFmpegだけになる
- 3OSで同じ手順になり、ライセンスの記録（configureの引数）を自分で持てる
- macOSはどのみちソースからのビルドが要る

欠点は、WindowsとLinuxのビルドを自分のCIで保守すること、AV1のソフトウェアデコードとLinuxのH.264エンコーダがないことである。

### ビルドの構成

`scripts/build-ffmpeg.sh [ターゲット]` の1本のbashで、3OSとも動かす。WindowsではMSYS2の `MINGW64` シェルで動かす（FFmpegのconfigureがbashとmakeを要るため、PowerShellとの二本立てにするより手順が少ない）。取得するのは `https://ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz` だけで、SHA-256が合わなければ止まる。このSHA-256は、公式サイトのtarballから求めた値と、Homebrewのformulaに書かれた値が一致することを確かめた。署名（`.asc`）の検証はしていない。

共通のconfigureの引数は `--disable-autodetect --disable-debug --disable-doc --disable-ffplay --disable-network --enable-zlib`。`--enable-gpl`、`--enable-nonfree`、`--enable-version3`、`--enable-lib*` は付けない。

| ターゲット | 追加の引数 | H.264エンコーダ |
| --- | --- | --- |
| `aarch64-apple-darwin` | `--enable-videotoolbox --enable-audiotoolbox`、最低macOS 11.0 | `h264_videotoolbox` |
| `x86_64-apple-darwin` | 同上、最低macOS 10.15。Apple Siliconのrunnerでクロスビルドする（`nasm` が要る） | `h264_videotoolbox` |
| `x86_64-pc-windows-msvc` | MSYS2のgccで `--enable-mediafoundation --enable-d3d11va`（`h264_mf` がDirect3D 11の型を使うため）、`-static` でリンク | `h264_mf` |
| `x86_64-unknown-linux-gnu` | なし | なし |

Linuxにエンコーダを入れなかったのは、`--enable-vaapi` で作るとlibvaを実行時に要求し、ない環境では `ffmpeg` と `ffprobe` が起動しなくなるためである。`libopenh264` を自分でビルドするとCiscoの特許ライセンスが及ばない。LinuxのH.264書き出しの方式は、段階5で決める。

ビルドが終わると、スクリプトは次を検査し、満たさなければ止まる。

- configureにGPL、nonfree、version3、外部ライブラリがない（実行できないクロスビルドは、実行ファイルに埋め込まれたconfigureの文字列で調べる）
- `ffmpeg -L` がLGPLを示す
- デコーダ（h264、hevc、vp9、aac、mp3、flac、opus、vorbis、pcm_s16le）とAACエンコーダ、OSごとのH.264エンコーダがある

### 同梱のしかた

Tauriの `externalBin` は、`binaries/<名前>-<ターゲットのtriple>[.exe]` というファイルを探す（[公式](https://v2.tauri.app/develop/sidecar/)）。Tauriのソースで次のことを確かめた。

- `tauri-build` は、`externalBin` に書かれたファイルがないと `cargo build` の時点で失敗する（`resource path ... doesn't exist`）。実際に存在しない名前を指定して、`cargo check` が失敗することも確かめた。つまり、通常の設定に入れると、`tauri dev` やCIのclippyの前にも毎回FFmpegの取得が要る
- そのため、`externalBin` は通常の `tauri.conf.json` には入れず、`src-tauri/tauri.bundle.conf.json` に分けた。リリースのときだけ `--config` で重ねる（`npm run tauri:release`）
- 同梱時の置き場所は、Tauriのバンドラのソースでは、macOSが `Contents/MacOS/`、debが `/usr/bin/`、NSISが `$INSTDIR`（アプリ本体と同じフォルダ）、AppImageがバンドルのバイナリ用テンプレートに従う場所である。いずれもtriple（`-aarch64-apple-darwin` など）を外した名前になる

debは `/usr/bin/` に置くため、素の `ffmpeg` という名前だと、Linuxのffmpegパッケージとファイルが衝突する。そこで同梱品は `fluxlide-ffmpeg`、`fluxlide-ffprobe` と名付け、`Ffmpeg::locate` は実行ファイルと同じフォルダで、この名前を探すようにした。環境変数 `FLUXLIDE_FFMPEG_DIR` と `PATH` は、これまでどおり素の名前を探す。探す順は、環境変数、実行ファイルと同じフォルダ、`PATH`。

リリースのワークフロー（`.github/workflows/release.yml`）は、`.github/actions/build-ffmpeg` でFFmpegをビルドしてから `tauri-action` を動かす。ビルド結果は、スクリプトのハッシュをキーにキャッシュする。`.github/workflows/ffmpeg.yml` は、スクリプトが変わったときに4ターゲットのビルドだけを確かめる。

## 3. 確認環境

| 項目 | 内容 |
| --- | --- |
| OS | macOS 27.0.1（Apple Silicon） |
| 機種 | MacBook Pro、Apple M4 Pro |
| FFmpeg | 9.0.2（ffmpeg.orgのtarball。Apple clang 21.0.0でビルド） |
| Tauri | tauri 2.12.4、tauri-build 2.7.3 |

## 4. 結果

macOS（`aarch64-apple-darwin`）での確認。

| 確認項目 | 結果 |
| --- | --- |
| ビルド | スクリプトが約1分で完了した。`ffmpeg` と `ffprobe` が各約21MB |
| configure | `--disable-autodetect --disable-debug --disable-doc --disable-ffplay --disable-network --enable-zlib --enable-videotoolbox --enable-audiotoolbox` とdeployment targetの指定のみ。GPL、nonfree、version3、外部ライブラリなし。`ffmpeg -L` はLGPLを示す |
| 動的リンク | macOSのシステムのフレームワークと、`libSystem`、`libz` だけ（`otool -L`） |
| エンコーダ | `h264_videotoolbox`、`hevc_videotoolbox`、`aac`（内蔵）、`aac_at`。`libx264` はない |
| デコーダ | h264、hevc、vp9、aac、mp3、flac、opus、vorbisがある。`av1` はあるが、ハードウェア支援専用 |
| 書き出しの試し | `h264_videotoolbox` と `aac` でMP4（640×360、2秒）を作り、`ffprobe` でh264とaacを確認し、`ffmpeg` でデコードして成功 |
| エンジンのテスト | `FLUXLIDE_FFMPEG_DIR` で指定して `cargo test -p fluxlide-engine`。14件が通った（動画と音のテストを含む。Homebrewの `ffmpeg` が見つからない `PATH` で実行） |
| .appへの同梱 | `npm run tauri:release -- --bundles app` で、`Fluxlide.app/Contents/MacOS/` に `fluxlide-ffmpeg` と `fluxlide-ffprobe` が置かれ（アドホック署名つき）、`Contents/Resources/licenses/` にライセンス文とビルドの記録が入った。.appは48.7MiB |
| アプリからの検出 | .appを起動すると、管理画面に「動画デコーダ: ffmpeg version 9.0.2 …（`.app/Contents/MacOS/fluxlide-ffmpeg`）」と表示された |

## 5. ライセンスと特許

法的な助言ではなく、調べた事実と、プロジェクトとしての判断の候補である。最終判断が要る点は、5.4にまとめた。

### 5.1 FFmpegのLGPL

一次情報は[FFmpegのlegalページ](https://ffmpeg.org/legal.html)とそのチェックリストである。FFmpegはLGPL v2.1以降で、GPLの部分（`--enable-gpl`、libx264など）を使うと全体がGPLになる。今回のビルドはLGPL v2.1以降になる。チェックリストと、Fluxlideでの対応は次のとおり。

| 項目 | 対応 |
| --- | --- |
| `--enable-gpl`、`--enable-nonfree`、GPLのライブラリを使わない | 使っていない。スクリプトが検査する |
| FFmpegのライブラリを動的にリンクする | ライブラリはリンクしておらず、FFmpegのコマンドを別のプロセスとして起動している。同梱の実行ファイルは単体のLGPLのプログラムで、アプリのフォルダ内の `fluxlide-ffmpeg` を自分でビルドしたものに差し替えられる。この解釈は、5.4の1番に挙げた |
| 対応するソースを配布する。ソースが配布物と一致する | 改変していない公式のtarballを使う。ダウンロードページとリリースの説明に、tarballのURLとSHA-256を書く必要がある（リリース作成時の作業） |
| 改変の記録（`git diff`） | 改変なし。`licenses/ffmpeg/BUILD-<target>.txt` に「改変なし」と版を記録している |
| configureの記録を書いたテキストを付ける | `BUILD-<target>.txt` に引数を記録し、パッケージの `licenses/ffmpeg/` に入れる |
| ソースを、バイナリと同じサーバーに置く | 未対応。同じGitHub Releasesにtarballを添付するか、`scripts/build-ffmpeg.sh` へのリンクにするか、決める必要がある |
| ダウンロードページ、アバウト画面、EULAで、LGPLv2.1でFFmpegを使うことを書く | `THIRD_PARTY_LICENSES.md` の下書きがある。アバウト画面とダウンロードページには未反映 |
| リバースエンジニアリングの禁止をEULAに入れない。名前を正しく書く（FFmpeg） | EULAはまだない。表記は `FFmpeg` |
| 実行ファイルの名前を隠さない（接頭辞は可） | `fluxlide-` の接頭辞のみ |

### 5.2 特許

FFmpegのlegalページは、FFmpegが特許のあるアルゴリズムを使うかどうかは分からないとしたうえで、H.264やMPEG-4の規格の適合実装は一部の国で特許の対象になりうること、私的な利用での懸念は小さいが、商用の製品では使用料を求められる可能性があることを述べている。以下は、各プールの公開ページから読み取れた範囲である。

| 規格 | 一次情報 | 読み取れたこと |
| --- | --- | --- |
| H.264/AVC | [Via LA](https://www.via-la.com/licensing-2/avc-h-264/) | PC向けの製品（エンコーダ、デコーダのソフトウェアを含む）は、最初の10万台まで0ドル、それ以降は1台0.20ドル（500万台超は0.10ドル）、企業ごとの年間の上限は975万ドル。12分以下の配信タイトルは0ドル。OSに組み込まれたコーデックは、エンドユーザーの個人・消費者としての利用に限って無償。各社が自分で必要な許諾を判断するとも書かれている |
| AAC | [Via LA](https://www.via-la.com/licensing-2/aac/) | エンドユーザー向けのエンコーダまたはデコーダの製品の製造者・開発者がライセンスを要する。1台目から0.98ドル（年間）、初期費用1.5万ドル（小規模事業者は1,000ドル）。AACのビットストリームの配布には特許料がかからない。ソフトウェアの扱いはページに明記がない |
| HEVC/H.265 | [Access Advance](https://www.accessadvance.com/licensing-programs/hevc-advance/) | ページには料率や無償の枠がなく、ソフトウェアのデコーダが含まれるかも書かれていない。要問い合わせ |

配布者に関係しうることは、次のように整理できる。

- デコード: Fluxlideは、ユーザーの手元の動画を再生するために、同梱のFFmpegでH.264、HEVC、AACをデコードする。ソフトウェアのデコーダを配布する者は、上記のプールの「製品」にあたりうる。ただしH.264は、PCの製品は10万台まで無償と読める。HEVCとAACの扱いは、公開ページだけでは判断できない
- エンコード（段階5の書き出し）: OSのエンコーダ（VideoToolbox、Media Foundation）を呼ぶ場合、エンコードの処理は、アプリが同梱するコードではなく、OSの機能が行う。Via LAのAVCのページは「OSに組み込まれたコーデックは、個人・消費者としての利用に限り無償」としている。これが、OSのエンコーダを呼ぶアプリの配布者にどう及ぶか、また個人の作品でなく事業の用途（展示やサイネージ）での利用がどうなるかは、公開情報では確かめられていない
- `libopenh264`（BSD）: [Ciscoのバイナリライセンス](https://www.openh264.org/BINARY_LICENSE.txt)により、CiscoがMPEG LAのAVCの特許ライセンスを持つのは、Ciscoが配布したバイナリである。条件は、バイナリをエンドユーザーの機器に別にダウンロードさせること（第三者のソフトウェアに事前に組み込まない）、利用者が有効・無効を選べること、「OpenH264 Video Codec provided by Cisco Systems, Inc.」の表示とライセンス文の掲載。対象は個人・非営利の用途で、それ以外の用途には許諾がない。ソースから自分でビルドした場合や、BtbNのようなビルドを同梱する場合は、このライセンスの外になる。なお、ページは、特許のライセンスをCiscoのバイナリに限って述べており、ソースや他者のビルドには触れていない
- LinuxにはOSのH.264エンコーダがない。段階5では、(1) VAAPI（libvaが要る）、(2) 初回にCiscoのバイナリを別にダウンロードさせる、(3) H.264を使わない書き出し（VP9のWebMなど）のいずれかになる

### 5.3 同梱するライセンス表記

リポジトリの `THIRD_PARTY_LICENSES.md` にFFmpegの項の下書きを置き、リリースのパッケージの `licenses/` に入れる。他の依存（フォント、Rust、npm）は段階7で追記する。

### 5.4 最終判断が要る点

1. 別のプロセスとして同梱するFFmpegの、LGPLの義務の範囲（5.1の2番目の項目の解釈）。このまま進めてよいかの確認
2. 対応するソースの提示の方法（リリースにtarballを添付するか、URLの記載だけにするか）
3. H.264、HEVC、AACのソフトウェアのデコーダとして配布することの、Via LAとAccess Advanceへの対応。ライセンスを取る、デコードの対象を絞る、OSのデコーダだけを使う（HEVCなど）のいずれにするか
4. 書き出しのH.264エンコーダ（OS、openh264、VAAPI）と、事業の用途で使わせることの許諾の確認

## 6. 判断

FFmpegは、ffmpeg.orgの公式ソース（9.0.2）を `scripts/build-ffmpeg.sh` でLGPL v2.1以降の最小構成でビルドして、`externalBin` で同梱する方式で進める。同梱品は `fluxlide-ffmpeg`、`fluxlide-ffprobe` の名前にする。`externalBin` はリリースのときだけ、追加の設定ファイルで足す。macOSでは、ビルド、エンジンのテスト、.appへの同梱と起動までを確かめた。2026-10-21には、CI（`.github/workflows/ffmpeg.yml`）で4つのターゲットすべてのビルドが通った。WindowsではFFmpeg 9.0.2の `h264_mf` がDirect3D 11の型を使うため、`--enable-d3d11va` を加えた。ビルドの最後に、LGPLであること、必要なデコーダとエンコーダ（Windowsは `h264_mf`）があることを確かめている。5.4の点は、リリース前に決める。

## 7. 未確認の項目

- Windows（NSIS・MSIの中での配置、`h264_mf` での実際の書き出し）とLinux（deb・AppImageの中での配置）。ビルドはCIで通ったが、できた実行ファイルをアプリに同梱して動かすところは確かめていない
- `x86_64-apple-darwin` のクロスビルド（`nasm` が要る。この環境にはない）。最低macOSの10.15で動くか
- macOSの署名と公証。バンドラは同梱のバイナリにも署名するが、Developer IDとhardened runtimeでの動作は未確認
- AppImageでの `fluxlide-ffmpeg` の置き場所。ソースの調べでは、アプリ本体と同じ `usr/bin` と読めるが、実物は未確認
- Windowsのハードウェアエンコーダ `h264_mf` の実際の動作と、Linuxの書き出し方式
- AV1のソフトウェアデコード。ffmpeg.org以外のソース（dav1dなど）が要る
- ライセンス文を下書きどおりにリリースの説明やアバウト画面へ反映すること
- ソースのGPG署名の検証
- 同梱のサイズ。macOSの.appでは48.7MiB。WindowsとLinuxは未計測
