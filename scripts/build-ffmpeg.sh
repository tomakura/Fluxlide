#!/usr/bin/env bash
# Builds the LGPL FFmpeg that Fluxlide bundles and puts it where Tauri's
# `externalBin` expects it:
#
#   src-tauri/binaries/fluxlide-ffmpeg-<target-triple>[.exe]
#   src-tauri/binaries/fluxlide-ffprobe-<target-triple>[.exe]
#   src-tauri/binaries/ffmpeg-licenses/   (license texts and build record)
#
# The source is the unmodified release tarball from ffmpeg.org, pinned by
# version and SHA-256. No external codec library is linked, so the result is
# LGPL v2.1 or later. H.264 encoding uses the operating system's encoder
# (VideoToolbox, Media Foundation). See docs/spike-ffmpeg-bundling.md.
#
# Usage: scripts/build-ffmpeg.sh [target-triple]
#   The default is the host triple reported by `rustc`.
#   On Windows, run it from an MSYS2 MINGW64 shell with these packages:
#   make, diffutils, tar, xz, nasm, mingw-w64-x86_64-gcc, mingw-w64-x86_64-zlib.
#   On Linux: build-essential, nasm, zlib1g-dev. On macOS: Xcode command line
#   tools, and nasm for x86_64-apple-darwin.
set -euo pipefail

FFMPEG_VERSION=9.0.2
FFMPEG_SHA256=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e
FFMPEG_URL="https://ffmpeg.org/releases/ffmpeg-${FFMPEG_VERSION}.tar.xz"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work="$root/target/ffmpeg-build"
out="$root/src-tauri/binaries"

target="${1:-$(rustc -vV | sed -n 's/^host: //p')}"
exe=""
configure_args=(
  --disable-autodetect --disable-debug --disable-doc --disable-ffplay
  --disable-network --enable-zlib
)
host_os="$(uname -s)"
case "$target" in
  aarch64-apple-darwin | x86_64-apple-darwin)
    [ "$host_os" = Darwin ] || { echo "$target must be built on macOS" >&2; exit 1; }
    arch="${target%%-*}"
    if [ "$arch" = aarch64 ]; then
      arch_flag=arm64
      min_macos=11.0
    else
      arch_flag=x86_64
      min_macos=10.15
    fi
    configure_args+=(
      --enable-videotoolbox --enable-audiotoolbox
      --extra-cflags="-mmacosx-version-min=$min_macos"
      --extra-ldflags="-mmacosx-version-min=$min_macos"
    )
    if [ "$arch_flag" != "$(uname -m)" ]; then
      configure_args+=(
        --enable-cross-compile --arch="$arch" --target-os=darwin
        --cc="clang -arch $arch_flag"
      )
    fi
    encoder=h264_videotoolbox
    ;;
  x86_64-unknown-linux-gnu)
    [ "$host_os" = Linux ] || { echo "$target must be built on Linux" >&2; exit 1; }
    encoder=""
    ;;
  x86_64-pc-windows-msvc)
    case "$host_os" in MINGW* | MSYS*) ;; *)
      echo "$target must be built in an MSYS2 MINGW64 shell" >&2; exit 1 ;;
    esac
    exe=.exe
    configure_args+=(
      --arch=x86_64 --target-os=mingw32 --enable-mediafoundation
      --extra-ldexeflags=-static
    )
    encoder=h264_mf
    ;;
  *)
    echo "unsupported target: $target" >&2
    exit 1
    ;;
esac

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

mkdir -p "$work"
tarball="$work/ffmpeg-${FFMPEG_VERSION}.tar.xz"
if [ ! -f "$tarball" ]; then
  curl --fail --location --silent --show-error --output "$tarball.part" "$FFMPEG_URL"
  mv "$tarball.part" "$tarball"
fi
actual="$(sha256 "$tarball")"
if [ "$actual" != "$FFMPEG_SHA256" ]; then
  rm -f "$tarball"
  echo "SHA-256 of $FFMPEG_URL does not match: got $actual, expected $FFMPEG_SHA256" >&2
  exit 1
fi

src="$work/ffmpeg-${FFMPEG_VERSION}"
build="$work/build-$target"
rm -rf "$src" "$build"
tar -xf "$tarball" -C "$work"
mkdir -p "$build"

jobs="$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu)"
(
  cd "$build"
  "$src/configure" "${configure_args[@]}"
  make -j"$jobs" ffmpeg"$exe" ffprobe"$exe"
)

mkdir -p "$out/ffmpeg-licenses"
cp "$build/ffmpeg$exe" "$out/fluxlide-ffmpeg-$target$exe"
cp "$build/ffprobe$exe" "$out/fluxlide-ffprobe-$target$exe"
strip "$out/fluxlide-ffmpeg-$target$exe" "$out/fluxlide-ffprobe-$target$exe" 2>/dev/null || true
cp "$src/COPYING.LGPLv2.1" "$src/COPYING.LGPLv3" "$src/LICENSE.md" "$out/ffmpeg-licenses/"
{
  echo "FFmpeg $FFMPEG_VERSION (unmodified source)"
  echo "Source: $FFMPEG_URL"
  echo "SHA-256: $FFMPEG_SHA256"
  echo "Target: $target"
  echo "configure ${configure_args[*]}"
} > "$out/ffmpeg-licenses/BUILD-$target.txt"

# Checks on the result. A binary that cannot run on this machine (a
# cross-compiled one) is checked through the configuration string that FFmpeg
# embeds in the executable.
ffmpeg_bin="$out/fluxlide-ffmpeg-$target$exe"
if "$ffmpeg_bin" -version >/dev/null 2>&1; then
  config="$("$ffmpeg_bin" -version | grep '^configuration:')"
  encoders="$("$ffmpeg_bin" -hide_banner -encoders)"
  decoders="$("$ffmpeg_bin" -hide_banner -decoders)"
  "$ffmpeg_bin" -L | grep -q 'GNU Lesser General Public' ||
    { echo "ffmpeg -L does not report the LGPL" >&2; exit 1; }
  for name in h264 hevc vp9 aac mp3 flac opus vorbis pcm_s16le; do
    grep -qE "^ [A-Z.]{6} $name " <<<"$decoders" ||
      { echo "decoder $name is missing" >&2; exit 1; }
  done
  grep -qE '^ [A-Z.]{6} aac ' <<<"$encoders" || { echo "encoder aac is missing" >&2; exit 1; }
  if [ -n "$encoder" ]; then
    grep -qE "^ [A-Z.]{6} $encoder " <<<"$encoders" ||
      { echo "encoder $encoder is missing" >&2; exit 1; }
  fi
else
  echo "note: $target cannot run on this machine; checking the embedded configuration only" >&2
  config="$(strings -a "$ffmpeg_bin" | grep -- '--disable-autodetect' || true)"
  [ -n "$config" ] || { echo "no configuration string in $ffmpeg_bin" >&2; exit 1; }
fi
if grep -qE -- '--enable-(gpl|nonfree|version3|lib[a-z0-9_]+)' <<<"$config"; then
  echo "unexpected license-relevant option in: $config" >&2
  exit 1
fi
echo "ok: $ffmpeg_bin"
echo "$config"
