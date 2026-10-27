# Third-party licenses

This file lists the third-party software that Fluxlide releases include or
use. The license notices of the Rust crates and npm packages built into the
application are in `licenses/THIRD_PARTY_NOTICES.txt` next to it, written
for each release by `scripts/third-party-notices.mjs`.

## FFmpeg

Fluxlide uses FFmpeg to decode videos, audio and HEIC photos and to write
video files. The release packages contain two
unmodified FFmpeg command-line programs, `fluxlide-ffmpeg` and
`fluxlide-ffprobe` (FFmpeg's `ffmpeg` and `ffprobe`). Fluxlide starts them as
separate processes; it does not link FFmpeg's libraries into its own
executable.

- Project: FFmpeg, <https://ffmpeg.org/>. The name is spelled "FFmpeg".
- Version: 9.0.2
- License: GNU Lesser General Public License, version 2.1 or later
  (LGPLv2.1+). The programs are built without `--enable-gpl`,
  `--enable-nonfree` and `--enable-version3`, and without any external codec
  library such as libx264 or libx265. The license texts are in
  `licenses/ffmpeg/` next to the application (`COPYING.LGPLv2.1`,
  `COPYING.LGPLv3`, `LICENSE.md`).
- Source code: the unmodified release tarball
  <https://ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz>
  (SHA-256 `8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e`).
  The Fluxlide repository's `scripts/build-ffmpeg.sh` is the exact build
  procedure, and `licenses/ffmpeg/BUILD-<target>.txt` records the `configure`
  options used for each package. You can build the same programs from that
  source and put them next to the application in place of the bundled ones.
- Hardware encoders: on macOS the programs use Apple's VideoToolbox and
  AudioToolbox, and on Windows Media Foundation. These are parts of the
  operating system and are not distributed with Fluxlide.

Fluxlide itself is not a product of the FFmpeg developers and they do not own
Fluxlide. The FFmpeg developers are listed at
<https://ffmpeg.org/developer.html>. FFmpeg is a trademark of Fabrice Bellard,
originator of the FFmpeg project.

Video and audio formats such as H.264, HEVC and AAC may be covered by patents
in some countries. Neither Fluxlide nor this license file grants any patent
license; anyone using Fluxlide in a business should check which licenses
they need.

## IBM Plex Sans JP

Fluxlide draws captions, text watermarks and the title card with IBM Plex
Sans JP, which is embedded in the application's executable. The font files
are unmodified.

- Project: IBM Plex, <https://github.com/IBM/plex>
- Files: `IBMPlexSansJP-Light.ttf`, `IBMPlexSansJP-Medium.ttf`,
  `IBMPlexSansJP-Bold.ttf`
- Copyright © 2017 IBM Corp. with Reserved Font Name "Plex".
- License: SIL Open Font License, Version 1.1. The license text is in
  `licenses/ibm-plex-sans-jp/OFL.txt` next to the application (from
  `assets/fonts/OFL.txt` in the Fluxlide repository).

## Jost

The Fluxlide logotype is set in Jost (SIL Open Font License 1.1). The logo
files have the letters converted to outlines, and the font itself is not
included in the application.
