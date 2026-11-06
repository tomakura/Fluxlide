//! How loud a file sounds, measured once so that its sound can be brought
//! to the same loudness as the others ("音量をそろえる").
//!
//! `ffmpeg`'s `ebur128` filter reads the whole sound stream and reports its
//! integrated loudness (EBU R128, in LUFS) and its true peak. Playback then
//! applies one fixed gain per file ([`Loudness::gain`]): the sound is not
//! compressed or changed while it plays, only made louder or quieter.

use std::path::Path;

use super::video::{Ffmpeg, command};
use crate::EngineError;
use crate::show::Loudness;

impl Ffmpeg {
    /// Measures the first sound stream of `path`. Takes about a hundredth of
    /// the file's length.
    pub fn measure_loudness(&self, path: &Path) -> Result<Loudness, EngineError> {
        let output = command(self.ffmpeg_path())
            .args(["-nostdin", "-hide_banner", "-nostats", "-i"])
            .arg(path)
            .args(["-map", "0:a:0", "-vn", "-sn", "-dn"])
            .args(["-af", "ebur128=peak=true:framelog=quiet"])
            .args(["-f", "null", "-"])
            .output()
            .map_err(|err| EngineError::Media(format!("could not run ffmpeg: {err}")))?;
        let log = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            return Err(EngineError::Media(format!(
                "ffmpeg could not measure the sound of {}: {}",
                path.display(),
                log.lines().last().unwrap_or_default().trim()
            )));
        }
        parse_summary(&log).ok_or_else(|| {
            EngineError::Media(format!(
                "ffmpeg did not report the loudness of {}",
                path.display()
            ))
        })
    }
}

/// Reads the integrated loudness and the true peak from the summary the
/// filter prints at the end.
fn parse_summary(log: &str) -> Option<Loudness> {
    let summary = &log[log.rfind("Summary:")?..];
    let value = |label: &str| -> Option<&str> {
        summary
            .lines()
            .map(str::trim)
            .find_map(|line| line.strip_prefix(label))?
            .split_whitespace()
            .next()
    };
    let integrated_lufs: f32 = value("I:")?.parse().ok()?;
    let peak = value("Peak:")?;
    let true_peak_db = match peak {
        "-inf" => None,
        peak => Some(peak.parse::<f32>().ok()?),
    };
    Some(Loudness {
        integrated_lufs,
        true_peak_db,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUMMARY: &str = "\
size=N/A time=00:00:02.90 bitrate=N/A speed= 149x
[Parsed_ebur128_0 @ 0x5ed61d5a2d00] Summary:

  Integrated loudness:
    I:         -32.2 LUFS
    Threshold: -42.2 LUFS

  Loudness range:
    LRA:        20.0 LU
    Threshold: -52.2 LUFS
    LRA low:   -52.2 LUFS
    LRA high:  -32.2 LUFS

  True peak:
    Peak:      -28.5 dBFS
";

    #[test]
    fn the_summary_gives_the_loudness_and_the_peak() {
        assert_eq!(
            parse_summary(SUMMARY),
            Some(Loudness {
                integrated_lufs: -32.2,
                true_peak_db: Some(-28.5),
            })
        );
        let silent = SUMMARY
            .replace(
                "-32.2 LUFS\n    Threshold: -42.2",
                "-70.0 LUFS\n    Threshold: -70.0",
            )
            .replace("-28.5 dBFS", "-inf dBFS");
        assert_eq!(
            parse_summary(&silent),
            Some(Loudness {
                integrated_lufs: -70.0,
                true_peak_db: None,
            })
        );
        assert_eq!(parse_summary("no summary here"), None);
    }

    #[test]
    fn a_tone_is_measured_by_ffmpeg() {
        let Ok(ffmpeg) = Ffmpeg::locate() else {
            eprintln!("skipped: no ffmpeg");
            return;
        };
        let dir = std::env::temp_dir().join(format!("fluxlide-loudness-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.wav");
        // FFmpeg's sine is at 1/8 of full scale: a peak of -18 dBFS.
        let made = command(ffmpeg.ffmpeg_path())
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=f=440:d=3"])
            .arg(&path)
            .status()
            .unwrap();
        assert!(made.success());
        let loudness = ffmpeg.measure_loudness(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let peak = loudness.true_peak_db.unwrap();
        assert!((peak + 18.1).abs() < 0.5, "{loudness:?}");
        assert!(
            (-25.0..-15.0).contains(&loudness.integrated_lufs),
            "{loudness:?}"
        );
    }

    #[test]
    fn a_file_without_sound_is_an_error() {
        let Ok(ffmpeg) = Ffmpeg::locate() else {
            eprintln!("skipped: no ffmpeg");
            return;
        };
        let missing = std::env::temp_dir().join("fluxlide-no-such-file.wav");
        assert!(ffmpeg.measure_loudness(&missing).is_err());
    }
}
