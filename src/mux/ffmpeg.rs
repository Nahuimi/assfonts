use std::{path::Path, process::Command};

use super::{MuxResult, job::MuxJob};
use crate::error::{AssfontsError, Result};

pub struct FfmpegMuxer {
    pub program: String,
    pub ffprobe: Option<String>,
}

/// Stream counts of the input media needed to compute output stream indices.
///
/// ffmpeg stream specifiers differ per option family (verified against
/// ffmpeg 9.0): `-metadata:s:s:N` addresses the Nth subtitle stream,
/// `-metadata:s:t:N` the Nth attachment, and `-disposition:s:N` the
/// Nth output stream by absolute index.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProbeInfo {
    pub videos: usize,
    pub audios: usize,
    pub attachments: usize,
    pub subtitles: usize,
}

/// Probe the input media with ffprobe to count original streams per type.
pub fn probe_media(ffprobe: &str, media: &Path) -> Result<ProbeInfo> {
    let run = Command::new(ffprobe)
        .args(["-v", "0", "-print_format", "json", "-show_streams"])
        .arg(media)
        .output()
        .map_err(|err| AssfontsError::Mux(format!("failed to spawn ffprobe: {err}")))?;

    if !run.status.success() {
        return Err(AssfontsError::Mux(format!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&run.stderr)
        )));
    }

    let value: serde_json::Value = serde_json::from_slice(&run.stdout)
        .map_err(|err| AssfontsError::Mux(format!("invalid ffprobe output: {err}")))?;

    let mut info = ProbeInfo::default();
    if let Some(streams) = value.get("streams").and_then(|streams| streams.as_array()) {
        for stream in streams {
            match stream.get("codec_type").and_then(|t| t.as_str()) {
                Some("video") => info.videos += 1,
                Some("audio") => info.audios += 1,
                Some("attachment") => info.attachments += 1,
                Some("subtitle") => info.subtitles += 1,
                _ => {}
            }
        }
    }
    Ok(info)
}

fn attachment_mime(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("otf") => "font/otf",
        _ => "font/ttf",
    }
}

/// Build the ffmpeg argument list. `probe` is required when a media file is present.
///
/// ffmpeg binds options to the file they precede, so every `-i` input comes
/// first and all output options (maps, metadata, attachments) follow.
pub fn build_args(job: &MuxJob, probe: Option<&ProbeInfo>) -> Vec<String> {
    let empty = ProbeInfo::default();
    let probe = if job.media.is_some() {
        Some(probe.unwrap_or(&empty))
    } else {
        None
    };
    let count = |field: fn(&ProbeInfo) -> usize| -> usize { probe.map(field).unwrap_or(0) };

    let mut args: Vec<String> = Vec::new();

    // Inputs: media first, then one input per subtitle track.
    if let Some(media) = &job.media {
        args.push("-i".to_string());
        args.push(media.to_string_lossy().to_string());
    }
    for track in &job.subtitles {
        args.push("-i".to_string());
        args.push(track.path.to_string_lossy().to_string());
    }

    let mut first_sub_input = 0usize;
    let mut sub_meta_base = 0usize;
    let mut sub_absolute_base = 0usize;
    let mut attachment_base = 0usize;

    if job.media.is_some() {
        first_sub_input = 1;

        args.extend(["-map", "0:v?"].map(String::from));
        args.extend(["-c:v", "copy"].map(String::from));
        sub_absolute_base += count(|info| info.videos);
        args.extend(["-map", "0:a?"].map(String::from));
        args.extend(["-c:a", "copy"].map(String::from));
        sub_absolute_base += count(|info| info.audios);
        if job.keep_attachments {
            args.extend(["-map", "0:t?"].map(String::from));
            args.extend(["-c:t", "copy"].map(String::from));
            attachment_base = count(|info| info.attachments);
            sub_absolute_base += attachment_base;
        }
        if job.keep_subs {
            args.extend(["-map", "0:s?"].map(String::from));
            sub_meta_base = count(|info| info.subtitles);
            sub_absolute_base += sub_meta_base;
        }
    }

    for (index, _) in job.subtitles.iter().enumerate() {
        args.push("-map".to_string());
        args.push(format!("{}:s", first_sub_input + index));
    }
    args.extend(["-c:s", "copy"].map(String::from));

    for (index, track) in job.subtitles.iter().enumerate() {
        let meta = format!("-metadata:s:s:{}", sub_meta_base + index);
        if let Some(title) = &track.title {
            args.extend([meta.clone(), format!("title={title}")]);
        }
        if let Some(language) = &track.language {
            args.extend([meta.clone(), format!("language={language}")]);
        }
        args.push(format!("-disposition:s:{}", sub_absolute_base + index));
        args.push(if track.is_default {
            "default".to_string()
        } else {
            "0".to_string()
        });
    }

    for (index, attachment) in job.attachments.iter().enumerate() {
        args.push("-attach".to_string());
        args.push(attachment.to_string_lossy().to_string());
        args.push(format!("-metadata:s:t:{}", attachment_base + index));
        args.push(format!("mimetype={}", attachment_mime(attachment)));
    }

    if let Some(title) = &job.title {
        args.extend(["-metadata".to_string(), format!("title={title}")]);
    }

    // Output container: plain .mks output needs an explicit matroska muxer;
    // for .mkv the output extension selects it.
    if job.media.is_none() {
        args.extend(["-f", "matroska"].map(String::from));
    }

    args.push("-n".to_string());
    args.push(job.output.to_string_lossy().to_string());
    args
}

impl super::Muxer for FfmpegMuxer {
    fn kind(&self) -> super::MuxerKind {
        super::MuxerKind::Ffmpeg
    }

    fn mux(&self, job: &MuxJob, _scratch_dir: &Path) -> Result<MuxResult> {
        let probe = match &job.media {
            Some(media) => {
                let ffprobe = self
                    .ffprobe
                    .as_deref()
                    .ok_or_else(|| AssfontsError::MissingExternalTool("ffprobe".to_string()))?;
                Some(probe_media(ffprobe, media)?)
            }
            None => None,
        };

        let run = Command::new(&self.program)
            .args(build_args(job, probe.as_ref()))
            .output()
            .map_err(|err| AssfontsError::Mux(format!("failed to spawn ffmpeg: {err}")))?;

        let exit_code = run.status.code().unwrap_or(-1);
        let log = format!(
            "{}{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        if exit_code != 0 {
            let tail: Vec<&str> = log.lines().rev().take(20).collect();
            return Err(AssfontsError::Mux(format!(
                "ffmpeg failed with exit code {exit_code}:\n{}",
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            )));
        }

        Ok(MuxResult {
            output: job.output.clone(),
            exit_code,
            log,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{ProbeInfo, build_args};
    use crate::mux::{MuxJob, SubTrack};

    #[test]
    fn builds_args_for_mkv_job() {
        let mut job = MuxJob::new(PathBuf::from("out/show.muxed.mkv"));
        job.media = Some(PathBuf::from("in/show.mkv"));
        job.subtitles = vec![
            SubTrack {
                path: PathBuf::from("subs/show.sc.ass"),
                title: Some("sc".to_string()),
                language: Some("chi".to_string()),
                is_default: true,
            },
            SubTrack {
                path: PathBuf::from("subs/show.tc.ass"),
                title: None,
                language: None,
                is_default: false,
            },
        ];
        job.attachments = vec![PathBuf::from("tmp/Arial[0].ttf")];
        job.title = Some("show".to_string());
        let probe = ProbeInfo {
            videos: 1,
            audios: 1,
            attachments: 0,
            subtitles: 0,
        };

        let args = build_args(&job, Some(&probe));
        let expected = vec![
            "-i",
            "in/show.mkv",
            "-i",
            "subs/show.sc.ass",
            "-i",
            "subs/show.tc.ass",
            "-map",
            "0:v?",
            "-c:v",
            "copy",
            "-map",
            "0:a?",
            "-c:a",
            "copy",
            "-map",
            "1:s",
            "-map",
            "2:s",
            "-c:s",
            "copy",
            "-metadata:s:s:0",
            "title=sc",
            "-metadata:s:s:0",
            "language=chi",
            "-disposition:s:2",
            "default",
            "-disposition:s:3",
            "0",
            "-attach",
            "tmp/Arial[0].ttf",
            "-metadata:s:t:0",
            "mimetype=font/ttf",
            "-metadata",
            "title=show",
            "-n",
            "out/show.muxed.mkv",
        ];
        assert_eq!(args, expected);
    }

    #[test]
    fn builds_args_for_mks_job() {
        let mut job = MuxJob::new(PathBuf::from("out/show.mks"));
        job.subtitles = vec![SubTrack {
            path: PathBuf::from("show.ass"),
            title: None,
            language: Some("chi".to_string()),
            is_default: true,
        }];

        let args = build_args(&job, None);
        let expected = vec![
            "-i",
            "show.ass",
            "-map",
            "0:s",
            "-c:s",
            "copy",
            "-metadata:s:s:0",
            "language=chi",
            "-disposition:s:0",
            "default",
            "-f",
            "matroska",
            "-n",
            "out/show.mks",
        ];
        assert_eq!(args, expected);
    }

    #[test]
    fn offsets_stream_indices_when_keeping_originals() {
        let mut job = MuxJob::new(PathBuf::from("out/show.muxed.mkv"));
        job.media = Some(PathBuf::from("in/show.mkv"));
        job.keep_subs = true;
        job.keep_attachments = true;
        job.subtitles = vec![SubTrack {
            path: PathBuf::from("subs/show.sc.ass"),
            title: Some("sc".to_string()),
            language: Some("chi".to_string()),
            is_default: false,
        }];
        job.attachments = vec![PathBuf::from("tmp/Arial[0].ttf")];

        let probe = ProbeInfo {
            videos: 1,
            audios: 2,
            attachments: 1,
            subtitles: 2,
        };
        let args = build_args(&job, Some(&probe));
        assert!(args.contains(&"0:s?".to_string()));
        assert!(args.contains(&"0:t?".to_string()));
        // subtitle metadata is relative to subtitle streams: after 2 originals
        assert!(args.contains(&"-metadata:s:s:2".to_string()));
        // disposition is an absolute output index: 1 video + 2 audio + 1 attachment + 2 original subs
        assert!(args.contains(&"-disposition:s:6".to_string()));
        // attachment metadata is relative to attachment streams: after 1 original
        assert!(args.contains(&"-metadata:s:t:1".to_string()));
    }
}
