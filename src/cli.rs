use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Embed subsetted fonts into the ASS file (original behavior)
    Embed,
    /// Mux each subtitle with its subsetted fonts into a standalone .mks
    Mks,
    /// Mux media + subtitles + subsetted fonts into .mkv
    Mkv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MuxerChoice {
    Mkvmerge,
    Ffmpeg,
}

#[derive(Debug, Parser)]
#[command(
    name = "assfonts",
    version,
    about = "ASS font utility rewritten in Rust",
    subcommand_negates_reqs = true,
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    #[command(flatten)]
    pub run: RunOptions,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    Build(BuildOptions),
}

#[derive(Debug, Args, Clone)]
pub struct BuildOptions {
    #[arg(short = 'f', long = "fontpath", required = true, num_args = 1.., help = "Path to font files")]
    pub fontpaths: Vec<PathBuf>,

    #[arg(
        short = 'o',
        long = "output",
        default_value = ".",
        help = "Directory to write fonts.db"
    )]
    pub output: PathBuf,
}

#[derive(Debug, Args, Clone)]
pub struct RunOptions {
    #[arg(short = 'i', long = "input", required = true, num_args = 1.., help = "Input ASS files or directories")]
    pub inputs: Vec<PathBuf>,

    #[arg(
        short = 'o',
        long = "output",
        default_value = ".",
        help = "Output directory"
    )]
    pub output: PathBuf,

    #[arg(short = 'f', long = "fontpath", num_args = 1.., help = "Path to font files")]
    pub fontpaths: Option<Vec<PathBuf>>,

    #[arg(
        short = 'd',
        long = "dbpath",
        default_value = ".",
        help = "Directory containing fonts.db"
    )]
    pub dbpath: PathBuf,

    #[arg(
        short = 's',
        long = "strict",
        action = ArgAction::Set,
        default_value_t = true,
        value_name = "BOOL",
        help = "Fail on font usage errors"
    )]
    pub strict: bool,

    #[arg(
        long = "allow-missing-sample",
        help = "Allow missing character samples"
    )]
    pub allow_missing_sample: bool,

    #[arg(long = "allow-missing-fonts", help = "Allow missing fonts")]
    pub allow_error_fonts: bool,

    #[arg(
        short,
        long = "report",
        help = "Write run-report.json after processing"
    )]
    pub report: bool,

    #[arg(long = "force", help = "Overwrite existing output files")]
    pub force: bool,

    #[arg(
        long = "format",
        value_enum,
        default_value_t = OutputFormat::Embed,
        help = "Output format: embed fonts in ASS, mux to .mks, or mux into .mkv"
    )]
    pub format: OutputFormat,

    #[arg(
        long = "video",
        num_args = 1..,
        value_name = "PATH",
        help = "Media files or directories for --format mkv"
    )]
    pub videos: Option<Vec<PathBuf>>,

    #[arg(
        long = "muxer",
        value_enum,
        default_value_t = MuxerChoice::Mkvmerge,
        help = "External muxer used by --format mks/mkv"
    )]
    pub muxer: MuxerChoice,

    #[arg(
        long = "language",
        value_name = "SPEC",
        help = "Subtitle language: a fixed code (e.g. chi) or comment mappings '.sc=chi;.tc=chi'"
    )]
    pub language: Option<String>,

    #[arg(
        long = "default-track",
        value_name = "COMMENT",
        help = "Subtitle comment (filename part after the media name) to mark as the default track"
    )]
    pub default_track: Option<String>,

    #[arg(
        long = "title",
        value_name = "TITLE",
        help = "Override the container title (default: media file name)"
    )]
    pub title: Option<String>,

    #[arg(
        long = "keep-subs",
        help = "Keep original subtitle tracks of the input media (mkv mode)"
    )]
    pub keep_subs: bool,

    #[arg(
        long = "keep-attachments",
        help = "Keep original attachments of the input media (mkv mode)"
    )]
    pub keep_attachments: bool,

    #[arg(
        long = "utf8-sig",
        help = "Write output ASS files with a UTF-8 BOM (utf-8-sig)"
    )]
    pub utf8_sig: bool,

    #[arg(
        long = "char-compatible",
        help = "Keep all ASCII letters (and all digits when any digit is used) in subsetted fonts for renderer compatibility"
    )]
    pub char_compatible: bool,

    #[arg(
        long = "muxer-path",
        value_name = "PATH",
        help = "Custom path to the mkvmerge/ffmpeg executable (default: PATH lookup)"
    )]
    pub muxer_path: Option<PathBuf>,

    #[arg(
        long = "ffprobe-path",
        value_name = "PATH",
        help = "Custom path to ffprobe, used by the ffmpeg muxer (default: PATH lookup)"
    )]
    pub ffprobe_path: Option<PathBuf>,

    #[arg(
        long = "use-embedded-fonts",
        action = ArgAction::Set,
        default_value_t = true,
        value_name = "BOOL",
        help = "Use fonts embedded in the input ASS [Fonts] section as a font source (highest priority)"
    )]
    pub use_embedded_fonts: bool,

    #[arg(
        long = "rename-fonts",
        action = ArgAction::Set,
        default_value_t = true,
        value_name = "BOOL",
        help = "Rename subsetted fonts with a CRC suffix and rewrite ASS references (mks/mkv mode)"
    )]
    pub rename_fonts: bool,

    #[arg(
        long = "keep-temp",
        help = "Keep intermediate subsetted fonts after muxing"
    )]
    pub keep_temp: bool,
}
