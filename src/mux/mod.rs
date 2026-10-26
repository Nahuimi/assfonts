pub mod ffmpeg;
pub mod job;
pub mod mkvmerge;

use std::path::Path;

pub use job::{MuxJob, MuxResult, SubTrack};

use crate::error::{AssfontsError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuxerKind {
    Mkvmerge,
    Ffmpeg,
}

impl MuxerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mkvmerge => "mkvmerge",
            Self::Ffmpeg => "ffmpeg",
        }
    }
}

pub trait Muxer {
    fn kind(&self) -> MuxerKind;
    /// Run the mux. `scratch_dir` receives intermediate files (option file)
    /// and is expected to be cleaned up by the caller.
    fn mux(&self, job: &MuxJob, scratch_dir: &Path) -> Result<MuxResult>;
}

/// Locate `program` on PATH by filesystem lookup only — no subprocess is
/// spawned (GUI-subsystem parents would otherwise flash a console window).
/// Windows resolves via PATHEXT (`.exe` fallback on other platforms).
pub fn find_on_path(program: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    let extensions: Vec<String> = std::env::var_os("PATHEXT")
        .map(|value| {
            value
                .to_string_lossy()
                .split(';')
                .filter(|ext| !ext.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_else(|| vec![".exe".to_string()]);

    for dir in std::env::split_paths(&path) {
        let bare = dir.join(program);
        if bare.is_file() {
            return Some(bare);
        }
        for extension in &extensions {
            let candidate = dir.join(format!("{program}{extension}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

pub fn create_muxer(choice: MuxerKind) -> Result<Box<dyn Muxer>> {
    create_muxer_with(choice, None, None)
}

/// Create a muxer, resolving executables at the given custom paths when
/// provided (`None` resolves via PATH filesystem lookup). No subprocess is
/// spawned here — a missing tool is reported up front and a broken one
/// surfaces through the mux run itself.
pub fn create_muxer_with(
    choice: MuxerKind,
    muxer_path: Option<&std::path::Path>,
    ffprobe_path: Option<&std::path::Path>,
) -> Result<Box<dyn Muxer>> {
    let validate = |path: &std::path::Path, name: &str| -> Result<String> {
        if path.is_file() {
            Ok(path.to_string_lossy().into_owned())
        } else {
            Err(AssfontsError::MissingExternalTool(format!(
                "{name} ({}): not an existing file",
                path.display()
            )))
        }
    };
    let resolve = |name: &str| -> Result<String> {
        find_on_path(name).map(|path| path.to_string_lossy().into_owned()).ok_or_else(|| {
            AssfontsError::MissingExternalTool(format!("{name} not found on PATH"))
        })
    };

    match choice {
        MuxerKind::Mkvmerge => {
            let program = match muxer_path {
                Some(path) => validate(path, "mkvmerge")?,
                None => resolve("mkvmerge")?,
            };
            Ok(Box::new(mkvmerge::MkvmergeMuxer { program }))
        }
        MuxerKind::Ffmpeg => {
            let program = match muxer_path {
                Some(path) => validate(path, "ffmpeg")?,
                None => resolve("ffmpeg")?,
            };
            let ffprobe = match ffprobe_path {
                Some(path) => validate(path, "ffprobe")?,
                None => resolve("ffprobe")?,
            };
            Ok(Box::new(ffmpeg::FfmpegMuxer {
                program,
                ffprobe: Some(ffprobe),
            }))
        }
    }
}

/// Media container extensions accepted for `--video`, mirroring the Python tool defaults.
pub const MEDIA_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "mts", "mpg", "flv", "mpeg", "m2ts", "avi", "webm", "rm", "rmvb", "mov", "mk3d",
    "vob",
];

/// File extension for a subsetted standalone sfnt, inferred from its flavor bytes.
pub fn sfnt_extension(bytes: &[u8]) -> &'static str {
    if bytes.len() >= 4 && &bytes[0..4] == b"OTTO" {
        "otf"
    } else {
        "ttf"
    }
}

#[cfg(test)]
mod tests {
    use super::sfnt_extension;

    #[test]
    fn sniffs_sfnt_flavor() {
        assert_eq!(sfnt_extension(b"\x00\x01\x00\x00xx"), "ttf");
        assert_eq!(sfnt_extension(b"OTTOxxxx"), "otf");
        assert_eq!(sfnt_extension(b"truexxxx"), "ttf");
        assert_eq!(sfnt_extension(b"OT"), "ttf");
    }
}
