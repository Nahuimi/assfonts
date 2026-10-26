use std::{fs, path::Path, process::Command};

use super::{MuxResult, job::MuxJob};
use crate::error::{AssfontsError, Result};

pub struct MkvmergeMuxer {
    pub program: String,
}

/// Build the argument list written into the mkvmerge JSON option file.
/// The `-o <output>` argument is passed separately on the command line.
pub fn build_option_args(job: &MuxJob) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();

    if let Some(media) = &job.media {
        if !job.keep_subs {
            args.push("-S".to_string());
        }
        if !job.keep_attachments {
            args.push("-M".to_string());
        }
        args.extend(["(", media.to_string_lossy().as_ref(), ")"].map(String::from));
    }

    for track in &job.subtitles {
        if let Some(title) = &track.title {
            args.push("--track-name".to_string());
            args.push(format!("0:{title}"));
        }
        args.push("--language".to_string());
        args.push(format!("0:{}", track.language.as_deref().unwrap_or("und")));
        args.push("--default-track-flag".to_string());
        args.push(format!("0:{}", if track.is_default { 1 } else { 0 }));
        args.extend(["(", track.path.to_string_lossy().as_ref(), ")"].map(String::from));
    }

    for attachment in &job.attachments {
        args.push("--attachment-name".to_string());
        args.push(
            attachment
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default(),
        );
        args.push("--attach-file".to_string());
        args.push(attachment.to_string_lossy().to_string());
    }

    if let Some(title) = &job.title {
        args.push("--title".to_string());
        args.push(title.clone());
    }

    args
}

fn combine_log(output: &std::process::Output) -> String {
    let mut log = String::from_utf8_lossy(&output.stdout).to_string();
    log.push_str(&String::from_utf8_lossy(&output.stderr));
    log
}

fn log_tail(log: &str) -> String {
    let tail: Vec<&str> = log.lines().rev().take(20).collect();
    tail.into_iter().rev().collect::<Vec<_>>().join("\n")
}

impl super::Muxer for MkvmergeMuxer {
    fn kind(&self) -> super::MuxerKind {
        super::MuxerKind::Mkvmerge
    }

    fn mux(&self, job: &MuxJob, scratch_dir: &Path) -> Result<MuxResult> {
        let option_name = format!(
            "{}.mkvmerge.json",
            job.output
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("mux")
        );
        let option_path = scratch_dir.join(option_name);
        let option_args = serde_json::to_vec_pretty(&build_option_args(job))?;
        fs::write(&option_path, option_args)?;

        let run = Command::new(&self.program)
            .arg(format!("@{}", option_path.display()))
            .arg("-o")
            .arg(&job.output)
            .output()
            .map_err(|err| AssfontsError::Mux(format!("failed to spawn mkvmerge: {err}")))?;

        let exit_code = run.status.code().unwrap_or(-1);
        // mkvmerge: 0 = success, 1 = warnings (treated as success), >= 2 = failure.
        let log = combine_log(&run);
        if exit_code > 1 {
            return Err(AssfontsError::Mux(format!(
                "mkvmerge failed with exit code {exit_code}:\n{}",
                log_tail(&log)
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

    use super::build_option_args;
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
                title: Some("tc".to_string()),
                language: None,
                is_default: false,
            },
        ];
        job.attachments = vec![PathBuf::from("tmp/Arial[0].ttf")];
        job.title = Some("show".to_string());

        let args = build_option_args(&job);
        let expected = vec![
            "-S",
            "-M",
            "(",
            "in/show.mkv",
            ")",
            "--track-name",
            "0:sc",
            "--language",
            "0:chi",
            "--default-track-flag",
            "0:1",
            "(",
            "subs/show.sc.ass",
            ")",
            "--track-name",
            "0:tc",
            "--language",
            "0:und",
            "--default-track-flag",
            "0:0",
            "(",
            "subs/show.tc.ass",
            ")",
            "--attachment-name",
            "Arial[0].ttf",
            "--attach-file",
            "tmp/Arial[0].ttf",
            "--title",
            "show",
        ];
        assert_eq!(args, expected);
    }

    #[test]
    fn builds_args_for_mks_job_without_media() {
        let mut job = MuxJob::new(PathBuf::from("out/show.mks"));
        job.subtitles = vec![SubTrack {
            path: PathBuf::from("show.ass"),
            title: None,
            language: None,
            is_default: true,
        }];

        let args = build_option_args(&job);
        let expected = vec![
            "--language",
            "0:und",
            "--default-track-flag",
            "0:1",
            "(",
            "show.ass",
            ")",
        ];
        assert_eq!(args, expected);
    }

    #[test]
    fn keeps_original_tracks_and_attachments_when_requested() {
        let mut job = MuxJob::new(PathBuf::from("out/show.muxed.mkv"));
        job.media = Some(PathBuf::from("in/show.mkv"));
        job.keep_subs = true;
        job.keep_attachments = true;

        let args = build_option_args(&job);
        assert!(!args.contains(&"-S".to_string()));
        assert!(!args.contains(&"-M".to_string()));
    }
}
