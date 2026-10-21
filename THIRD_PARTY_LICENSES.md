# Third-party licenses

This file lists the third-party software that Fluxlide releases include or
use. It is a draft; the fonts and the Rust and npm dependencies are still to
be added (see docs/roadmap.md, stage 7).

## FFmpeg

Fluxlide uses FFmpeg to decode videos and audio and, in a
later version, to write video files. The release packages contain two
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
in some countries. This license file does not grant any patent license.
