use std::{
    cmp::Ordering,
    collections::HashSet,
    iter::Peekable,
    path::{Path, PathBuf},
    str::Chars,
};

use walkdir::WalkDir;

/// Video container extensions recognized by default (`gui.toml` can override).
/// Mirrors the CLI media list plus the common extras a dropped folder contains.
pub const DEFAULT_VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "m4v", "mts", "m2ts", "ts", "mpg", "mpeg", "mpe", "avi", "webm", "flv", "wmv",
    "mov", "rm", "rmvb", "vob", "mk3d", "asf", "3gp", "3g2", "ogv", "f4v", "mxf", "divx",
];

/// Subtitle extensions recognized by default (`gui.toml` can override).
pub const DEFAULT_SUBTITLE_EXTENSIONS: &[&str] = &[
    "ass", "ssa", "srt", "sub", "vtt", "smi", "sami", "idx", "sup", "mpl2", "ttml", "dfxp", "sbv",
];

/// Normalized extension lists used to classify dropped files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtConfig {
    video: Vec<String>,
    subtitle: Vec<String>,
}

impl Default for ExtConfig {
    fn default() -> Self {
        Self {
            video: normalize_extensions(DEFAULT_VIDEO_EXTENSIONS.iter().copied()),
            subtitle: normalize_extensions(DEFAULT_SUBTITLE_EXTENSIONS.iter().copied()),
        }
    }
}

impl ExtConfig {
    /// Build a config from user-provided lists. Entries are trimmed, lowercased
    /// and stripped of a leading `.`; an empty list falls back to the defaults
    /// so a broken `gui.toml` cannot disable recognition entirely.
    pub fn new(
        video: impl IntoIterator<Item = String>,
        subtitle: impl IntoIterator<Item = String>,
    ) -> Self {
        let mut config = Self {
            video: normalize_extensions(video),
            subtitle: normalize_extensions(subtitle),
        };
        if config.video.is_empty() {
            config.video = normalize_extensions(DEFAULT_VIDEO_EXTENSIONS.iter().copied());
        }
        if config.subtitle.is_empty() {
            config.subtitle = normalize_extensions(DEFAULT_SUBTITLE_EXTENSIONS.iter().copied());
        }
        config
    }

    pub fn video(&self) -> &[String] {
        &self.video
    }

    pub fn subtitle(&self) -> &[String] {
        &self.subtitle
    }

    fn has_video(&self, extension: &str) -> bool {
        self.video.iter().any(|known| known == extension)
    }

    fn has_subtitle(&self, extension: &str) -> bool {
        self.subtitle.iter().any(|known| known == extension)
    }
}

fn normalize_extensions<I, S>(extensions: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: Vec<String> = Vec::new();
    for extension in extensions {
        let normalized = extension
            .as_ref()
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase();
        if !normalized.is_empty() && !out.contains(&normalized) {
            out.push(normalized);
        }
    }
    out
}

/// Classification of a dropped file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Video,
    Subtitle,
    Other,
}

/// Classify one file by its (case-insensitive) extension.
pub fn file_kind(path: &Path, config: &ExtConfig) -> FileKind {
    let Some(extension) = path.extension().and_then(|ext| ext.to_str()) else {
        return FileKind::Other;
    };
    let extension = extension.to_ascii_lowercase();
    if config.has_video(&extension) {
        FileKind::Video
    } else if config.has_subtitle(&extension) {
        FileKind::Subtitle
    } else {
        FileKind::Other
    }
}

/// Result of expanding a drop into files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanResult {
    pub videos: Vec<PathBuf>,
    pub subtitles: Vec<PathBuf>,
    /// Files that were found but are neither video nor subtitle (archives,
    /// images, …). Reported to the user; nothing is extracted.
    pub ignored: usize,
}

/// Expand the given paths (files and/or directories), classify every file and
/// return clearly separated, naturally sorted lists. Duplicates (same file
/// reached twice, e.g. a folder and one of its files) are counted once;
/// hidden entries and `__MACOSX` folders are skipped.
pub fn scan_paths(paths: &[PathBuf], config: &ExtConfig, recursive: bool) -> ScanResult {
    let mut result = ScanResult::default();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    for path in paths {
        if path.is_dir() {
            let mut walker = WalkDir::new(path);
            if !recursive {
                walker = walker.max_depth(1);
            }
            for entry in walker
                .into_iter()
                .filter_entry(|entry| entry.depth() == 0 || keep_entry(entry))
                .flatten()
            {
                if entry.file_type().is_file() {
                    record(entry.path(), config, &mut seen, &mut result);
                }
            }
        } else if path.is_file() {
            record(path, config, &mut seen, &mut result);
        }
    }

    sort_natural(&mut result.videos);
    sort_natural(&mut result.subtitles);
    result
}

fn keep_entry(entry: &walkdir::DirEntry) -> bool {
    let Some(name) = entry.file_name().to_str() else {
        return true;
    };
    if name.starts_with('.') {
        return false;
    }
    !name.eq_ignore_ascii_case("__MACOSX")
}

fn record(path: &Path, config: &ExtConfig, seen: &mut HashSet<PathBuf>, result: &mut ScanResult) {
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !seen.insert(key) {
        return;
    }
    match file_kind(path, config) {
        FileKind::Video => result.videos.push(path.to_path_buf()),
        FileKind::Subtitle => result.subtitles.push(path.to_path_buf()),
        FileKind::Other => result.ignored += 1,
    }
}

/// Sort paths by file name using [`natural_cmp`], with the full path as the
/// tiebreak. `ep2` sorts before `ep10`.
pub fn sort_natural(paths: &mut [PathBuf]) {
    let mut keyed: Vec<(PathBuf, String)> = paths
        .iter()
        .map(|path| {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            (path.clone(), name)
        })
        .collect();
    keyed.sort_by(|left, right| {
        natural_cmp(&left.1, &right.1)
            .then_with(|| natural_cmp(&left.0.to_string_lossy(), &right.0.to_string_lossy()))
    });
    for (slot, (path, _)) in paths.iter_mut().zip(keyed) {
        *slot = path;
    }
}

/// Natural ("human") string comparison: runs of ASCII digits compare by
/// numeric value (`ep2 < ep10`), other characters compare case-insensitively,
/// and the raw strings break exact ties.
pub fn natural_cmp(left: &str, right: &str) -> Ordering {
    let mut left_chars = left.chars().peekable();
    let mut right_chars = right.chars().peekable();

    loop {
        match (left_chars.peek().copied(), right_chars.peek().copied()) {
            (None, None) => return left.cmp(right),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left_char), Some(right_char)) => {
                if left_char.is_ascii_digit() && right_char.is_ascii_digit() {
                    let left_run = take_digits(&mut left_chars);
                    let right_run = take_digits(&mut right_chars);
                    let ordering = cmp_digit_runs(&left_run, &right_run);
                    if ordering != Ordering::Equal {
                        return ordering;
                    }
                } else {
                    left_chars.next();
                    right_chars.next();
                    let ordering = lower(left_char).cmp(&lower(right_char));
                    if ordering != Ordering::Equal {
                        return ordering;
                    }
                }
            }
        }
    }
}

fn lower(character: char) -> char {
    character.to_lowercase().next().unwrap_or(character)
}

fn take_digits(chars: &mut Peekable<Chars<'_>>) -> String {
    let mut run = String::new();
    while let Some(character) = chars.peek().copied() {
        if !character.is_ascii_digit() {
            break;
        }
        run.push(character);
        chars.next();
    }
    run
}

fn cmp_digit_runs(left: &str, right: &str) -> Ordering {
    let left_value = left.trim_start_matches('0');
    let right_value = right.trim_start_matches('0');
    let by_value = left_value
        .len()
        .cmp(&right_value.len())
        .then_with(|| left_value.cmp(right_value));
    if by_value != Ordering::Equal {
        return by_value;
    }
    // Same value: the run with fewer leading zeros comes first (`1` < `01`).
    left.len().cmp(&right.len())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        DEFAULT_VIDEO_EXTENSIONS, ExtConfig, FileKind, file_kind, natural_cmp, scan_paths,
        sort_natural,
    };

    fn unique_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("assfonts-scan-{tag}-{nanos}"));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn natural_cmp_orders_episode_numbers() {
        assert_eq!(natural_cmp("ep2", "ep10"), std::cmp::Ordering::Less);
        assert_eq!(natural_cmp("ep10", "ep2"), std::cmp::Ordering::Greater);
        assert_eq!(natural_cmp("ep2", "ep2"), std::cmp::Ordering::Equal);
        assert!(!natural_cmp("show 01 [1080p]", "show 1 [1080p]").is_lt());
    }

    #[test]
    fn natural_cmp_breaks_exact_ties_by_raw_string() {
        // Equal ignoring case, then the raw string decides ('A' < 'a').
        assert_eq!(natural_cmp("A.b", "a.b"), std::cmp::Ordering::Less);
        assert_eq!(natural_cmp("a.b", "A.b"), std::cmp::Ordering::Greater);
        assert_eq!(natural_cmp("1", "01"), std::cmp::Ordering::Less);
        assert_eq!(natural_cmp("01", "1"), std::cmp::Ordering::Greater);
    }

    #[test]
    fn sort_natural_orders_paths() {
        let mut paths = vec![
            PathBuf::from("dir/ep10.ass"),
            PathBuf::from("dir/ep2.ass"),
            PathBuf::from("dir/ep1.ass"),
        ];
        sort_natural(&mut paths);
        assert_eq!(
            paths,
            vec![
                PathBuf::from("dir/ep1.ass"),
                PathBuf::from("dir/ep2.ass"),
                PathBuf::from("dir/ep10.ass"),
            ]
        );
    }

    #[test]
    fn file_kind_uses_extension_lists() {
        let config = ExtConfig::default();
        assert_eq!(file_kind(&PathBuf::from("a.MKV"), &config), FileKind::Video);
        assert_eq!(
            file_kind(&PathBuf::from("a.ass"), &config),
            FileKind::Subtitle
        );
        assert_eq!(file_kind(&PathBuf::from("a.zip"), &config), FileKind::Other);
        assert_eq!(
            file_kind(&PathBuf::from("no_extension"), &config),
            FileKind::Other
        );
        assert!(config.video().contains(&"mkv".to_string()));
        assert_eq!(config.video().len(), DEFAULT_VIDEO_EXTENSIONS.len());
    }

    #[test]
    fn custom_extensions_override_defaults() {
        let config = ExtConfig::new(
            vec![" MKV ".to_string(), ".MP4".to_string(), "mkv".to_string()],
            vec!["SRT".to_string()],
        );
        assert_eq!(config.video(), ["mkv", "mp4"]);
        assert_eq!(config.subtitle(), ["srt"]);
        assert_eq!(
            file_kind(&PathBuf::from("a.srt"), &config),
            FileKind::Subtitle
        );
        assert_eq!(file_kind(&PathBuf::from("a.ass"), &config), FileKind::Other);
    }

    #[test]
    fn empty_extension_lists_fall_back_to_defaults() {
        let config = ExtConfig::new(Vec::<String>::new(), Vec::<String>::new());
        assert_eq!(file_kind(&PathBuf::from("a.mkv"), &config), FileKind::Video);
        assert_eq!(
            file_kind(&PathBuf::from("a.ass"), &config),
            FileKind::Subtitle
        );
    }

    #[test]
    fn scan_paths_expands_directories_and_counts_ignored() {
        let dir = unique_dir("expand");
        let nested = dir.join("sub");
        fs::create_dir_all(&nested).expect("create nested dir");
        for name in [
            "ep10.mkv",
            "ep2.mkv",
            "ep1.mkv",
            "ep1.chs.ass",
            "ep1.cht.ass",
        ] {
            fs::write(dir.join(name), b"x").expect("write file");
        }
        fs::write(nested.join("ep3.mkv"), b"x").expect("write nested file");
        fs::write(dir.join("cover.jpg"), b"x").expect("write ignored file");
        fs::write(dir.join("subs.zip"), b"x").expect("write archive");
        fs::write(dir.join(".hidden.ass"), b"x").expect("write hidden file");

        let result = scan_paths(std::slice::from_ref(&dir), &ExtConfig::default(), true);
        assert_eq!(
            result.videos,
            vec![
                dir.join("ep1.mkv"),
                dir.join("ep2.mkv"),
                nested.join("ep3.mkv"),
                dir.join("ep10.mkv"),
            ]
        );
        assert_eq!(
            result.subtitles,
            vec![dir.join("ep1.chs.ass"), dir.join("ep1.cht.ass")]
        );
        assert_eq!(result.ignored, 2, "cover.jpg and subs.zip are ignored");

        fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }

    #[test]
    fn scan_paths_without_recursion_stays_at_top_level() {
        let dir = unique_dir("flat");
        let nested = dir.join("season 1");
        fs::create_dir_all(&nested).expect("create nested dir");
        fs::write(dir.join("ep1.mkv"), b"x").expect("write file");
        fs::write(nested.join("ep2.mkv"), b"x").expect("write nested file");

        let result = scan_paths(std::slice::from_ref(&dir), &ExtConfig::default(), false);
        assert_eq!(result.videos, vec![dir.join("ep1.mkv")]);

        fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }

    #[test]
    fn scan_paths_dedupes_overlapping_inputs() {
        let dir = unique_dir("dedupe");
        let file = dir.join("ep1.mkv");
        fs::write(&file, b"x").expect("write file");

        let result = scan_paths(
            &[dir.clone(), file.clone(), dir.clone()],
            &ExtConfig::default(),
            true,
        );
        assert_eq!(result.videos, vec![file]);

        fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }

    #[test]
    fn scan_paths_skips_macosx_metadata_directory() {
        let dir = unique_dir("macosx");
        let junk = dir.join("__MACOSX");
        fs::create_dir_all(&junk).expect("create junk dir");
        fs::write(junk.join("._ep1.ass"), b"x").expect("write junk file");

        let result = scan_paths(std::slice::from_ref(&dir), &ExtConfig::default(), true);
        assert!(result.subtitles.is_empty());
        assert_eq!(result.ignored, 0);

        fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }
}
