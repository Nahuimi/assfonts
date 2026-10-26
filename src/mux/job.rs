use std::path::PathBuf;

/// One subtitle track inside a mux job.
#[derive(Debug, Clone)]
pub struct SubTrack {
    pub path: PathBuf,
    pub title: Option<String>,
    pub language: Option<String>,
    pub is_default: bool,
}

/// A single mux operation: optional media + subtitle tracks + font attachments.
#[derive(Debug, Clone)]
pub struct MuxJob {
    pub media: Option<PathBuf>,
    pub subtitles: Vec<SubTrack>,
    pub attachments: Vec<PathBuf>,
    pub output: PathBuf,
    pub title: Option<String>,
    pub keep_subs: bool,
    pub keep_attachments: bool,
}

impl MuxJob {
    pub fn new(output: PathBuf) -> Self {
        Self {
            media: None,
            subtitles: Vec::new(),
            attachments: Vec::new(),
            output,
            title: None,
            keep_subs: false,
            keep_attachments: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MuxResult {
    pub output: PathBuf,
    pub exit_code: i32,
    pub log: String,
}

/// Whether a subtitle stem belongs to a media stem (case-insensitive ASCII prefix,
/// an exact-name subtitle without comment also matches).
pub fn matches_media_prefix(sub_stem: &str, media_stem: &str) -> bool {
    if sub_stem.len() < media_stem.len() || !sub_stem.is_char_boundary(media_stem.len()) {
        return false;
    }
    let (prefix, _) = sub_stem.split_at(media_stem.len());
    prefix.eq_ignore_ascii_case(media_stem)
}

/// Comment part of a subtitle stem relative to a media stem.
///
/// Leading dots and a trailing `.subset` marker are stripped.
/// `video.sc` -> `sc`, `Video.SC` (media `video`) -> `SC`, `other` -> None.
pub fn subtitle_comment(sub_stem: &str, media_stem: &str) -> Option<String> {
    if !matches_media_prefix(sub_stem, media_stem) {
        return None;
    }
    let rest = &sub_stem[media_stem.len()..];
    rest_comment(rest)
}

/// Comment when there is no media: the text after the first dot of the stem.
/// `[Group] Show - 01 [1080p].sc` -> `sc`, `subtitle` -> None.
pub fn standalone_comment(sub_stem: &str) -> Option<String> {
    let (_, rest) = sub_stem.split_once('.')?;
    rest_comment(rest)
}

fn rest_comment(rest: &str) -> Option<String> {
    let comment = rest.trim_start_matches('.');
    let comment = comment.strip_suffix(".subset").unwrap_or(comment);
    if comment.is_empty() {
        None
    } else {
        Some(comment.to_string())
    }
}

/// Pair media files with subtitles by case-insensitive stem prefix.
///
/// Returns groups for media that have at least one subtitle plus the
/// subtitles that matched no media at all.
pub fn match_media_groups(
    videos: &[PathBuf],
    subs: &[PathBuf],
) -> (Vec<(PathBuf, Vec<PathBuf>)>, Vec<PathBuf>) {
    let mut groups = Vec::new();
    let mut matched = vec![false; subs.len()];

    for video in videos {
        let Some(video_stem) = video.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let mut group = Vec::new();
        for (index, sub) in subs.iter().enumerate() {
            let Some(sub_stem) = sub.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if matches_media_prefix(sub_stem, video_stem) {
                group.push(sub.clone());
                matched[index] = true;
            }
        }
        if !group.is_empty() {
            groups.push((video.clone(), group));
        }
    }

    let unmatched = subs
        .iter()
        .enumerate()
        .filter(|(index, _)| !matched[*index])
        .map(|(_, sub)| sub.clone())
        .collect();

    (groups, unmatched)
}

/// Resolve an output path that does not exist yet by appending `#1`, `#2`, ...
pub fn resolve_output_path(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }

    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output")
        .to_string();
    let extension = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|ext| format!(".{ext}"))
        .unwrap_or_default();
    let parent = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();

    for counter in 1u32.. {
        let candidate = parent.join(format!("{stem}#{counter}{extension}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("output path collision loop");
}

/// Parsed `--language` spec: either a fixed code for every track
/// or `comment=code` mappings separated by `;`.
#[derive(Debug, Clone, Default)]
pub struct LanguageSpec {
    pub fixed: Option<String>,
    pub map: Vec<(String, String)>,
}

/// Builtin comment -> ISO-639 mappings, mirroring the Python tool defaults.
pub const BUILTIN_LANGUAGE_MAP: &[(&str, &str)] = &[
    ("sc", "chi"),
    ("tc", "chi"),
    ("chs", "chi"),
    ("cht", "chi"),
    ("jap", "jpn"),
    ("简体中文", "chi"),
    ("繁體中文", "chi"),
    ("繁体中文", "chi"),
    ("zh-hans", "chi"),
    ("zh-hant", "chi"),
];

pub fn parse_language_spec(spec: &str) -> LanguageSpec {
    let mut parsed = LanguageSpec::default();
    for chunk in spec.split(';') {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        match chunk.split_once('=') {
            Some((key, value)) => {
                let key = normalize_language_key(key);
                let value = value.trim().to_string();
                if !key.is_empty() && !value.is_empty() {
                    parsed.map.push((key, value));
                }
            }
            None => {
                if parsed.fixed.is_none() {
                    parsed.fixed = Some(chunk.to_string());
                }
            }
        }
    }
    parsed
}

fn normalize_language_key(key: &str) -> String {
    key.trim()
        .trim_start_matches('.')
        .to_lowercase()
}

/// Resolve the language for one track comment.
/// The user spec wins over the builtin map; a fixed spec applies to all tracks.
pub fn language_for_comment(comment: Option<&str>, spec: &LanguageSpec) -> Option<String> {
    if let Some(fixed) = &spec.fixed {
        return Some(fixed.clone());
    }
    let comment = comment?;
    let key = comment.to_lowercase();
    for (mapped, language) in &spec.map {
        if *mapped == key {
            return Some(language.clone());
        }
    }
    BUILTIN_LANGUAGE_MAP
        .iter()
        .find(|(mapped, _)| *mapped == key)
        .map(|(_, language)| language.to_string())
}

/// Subtitle "base name": the part before the first `.` within the last 15
/// characters (i.e. the name minus a trailing comment like `.sc.ass`), as
/// SubRenamer groups multi-language subtitle sets. Falls back to the whole
/// stem when no `.` is found in the window.
pub fn subtitle_base_name(stem: &str) -> String {
    let window_start = stem.len().saturating_sub(15);
    if let Some(window) = stem.get(window_start..)
        && let Some(dot) = window.find('.')
    {
        return stem[..window_start + dot].to_string();
    }
    stem.to_string()
}

/// Pair media and subtitles **by sorted order** (SubRenamer-style): videos
/// sorted by name; subtitles sorted by base name and grouped per base name
/// (multi-language sets share one row); the i-th group pairs with the i-th
/// video. Leftovers on either side go to `unmatched` (extra videos produce
/// no unmatched entries — they are simply reported in `groups` only when
/// they have subs; extra subtitle groups are unmatched).
pub fn match_media_groups_by_order(
    videos: &[PathBuf],
    subs: &[PathBuf],
) -> (Vec<(PathBuf, Vec<PathBuf>)>, Vec<PathBuf>) {
    let mut sorted_videos: Vec<PathBuf> = videos.to_vec();
    sorted_videos.sort();

    let mut sub_stems: Vec<(String, PathBuf)> = subs
        .iter()
        .map(|sub| {
            let stem = sub
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_lowercase();
            (stem, sub.clone())
        })
        .collect();
    sub_stems.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    // Group consecutive entries sharing a base name.
    let mut groups: Vec<Vec<PathBuf>> = Vec::new();
    let mut current_base: Option<String> = None;
    for (stem, path) in sub_stems {
        let base = subtitle_base_name(&stem);
        if current_base.as_deref() != Some(base.as_str()) {
            groups.push(Vec::new());
            current_base = Some(base);
        }
        groups.last_mut().expect("group just pushed").push(path);
    }

    let unmatched = groups
        .iter()
        .enumerate()
        .filter(|(index, _)| *index >= sorted_videos.len())
        .flat_map(|(_, group)| group.iter().cloned())
        .collect();

    let paired = sorted_videos
        .into_iter()
        .zip(groups)
        .filter(|(_, group)| !group.is_empty())
        .collect();

    (paired, unmatched)
}

/// Rename suffix rule (one input, three semantics — see PLAN-MATCH.md §B):
/// `Keep` keeps each subtitle's own comment suffix, `Fixed` applies one
/// suffix to all, `Map` swaps the suffix per original comment
/// (`chs|sc.Haruhana;cht|tc.Haruhana`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuffixRule {
    Keep,
    Fixed(String),
    Map(Vec<(String, String)>),
}

/// Normalize a suffix fragment: strip leading/trailing dots, collapse `..`.
fn normalize_suffix(suffix: &str) -> String {
    let collapsed = suffix.replace("..", ".");
    let trimmed = collapsed.trim_matches('.');
    trimmed.to_string()
}

pub fn parse_suffix_rule(spec: &str) -> SuffixRule {
    let spec = spec.trim();
    if spec.is_empty() {
        return SuffixRule::Keep;
    }
    if spec.contains('|') {
        let mut map = Vec::new();
        for pair in spec.split(';') {
            let Some((from, to)) = pair.split_once('|') else {
                continue;
            };
            let from = normalize_suffix(from).to_lowercase();
            let to = normalize_suffix(to);
            if !from.is_empty() && !to.is_empty() {
                map.push((from, to));
            }
        }
        if map.is_empty() {
            return SuffixRule::Keep;
        }
        return SuffixRule::Map(map);
    }
    SuffixRule::Fixed(normalize_suffix(spec))
}

impl SuffixRule {
    /// Resolve the rename suffix for one subtitle given its comment.
    /// Returns `None` to keep the current behavior (original comment kept).
    fn suffix_for(&self, comment: Option<&str>) -> Option<String> {
        match self {
            Self::Keep => None,
            Self::Fixed(suffix) => Some(suffix.clone()),
            Self::Map(map) => {
                let comment = comment?.to_lowercase();
                map.iter()
                    .find(|(from, _)| *from == comment)
                    .map(|(_, to)| to.clone())
            }
        }
    }
}

/// Propose renames that align subtitle file names with their media stem:
/// the subtitle becomes `<video_stem>[.<suffix>].<ext>` in its own
/// directory. The suffix comes from `suffix_rule` (comment semantics above);
/// already-correct names, existing targets, and in-list conflicts are
/// skipped.
pub fn rename_proposals(
    groups: &[(PathBuf, Vec<PathBuf>)],
    suffix_rule: &SuffixRule,
) -> Vec<(PathBuf, PathBuf)> {
    let mut out: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut proposed_targets: Vec<PathBuf> = Vec::new();

    for (video, subs) in groups {
        let Some(video_stem) = video.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        for sub in subs {
            let Some(sub_stem) = sub.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some(ext) = sub.extension().and_then(|ext| ext.to_str()) else {
                continue;
            };
            let comment = subtitle_comment(sub_stem, video_stem)
                .or_else(|| standalone_comment(sub_stem));
            let new_stem = match suffix_rule.suffix_for(comment.as_deref()) {
                Some(suffix) => format!("{video_stem}.{suffix}"),
                None => match comment {
                    Some(comment) => format!("{video_stem}.{comment}"),
                    None => video_stem.to_string(),
                },
            };
            let target = sub.with_file_name(format!("{new_stem}.{ext}"));
            if target == *sub
                || target.exists()
                || proposed_targets.iter().any(|seen| seen == &target)
            {
                continue;
            }
            proposed_targets.push(target.clone());
            out.push((sub.clone(), target));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        language_for_comment, match_media_groups, matches_media_prefix, parse_language_spec,
        resolve_output_path, standalone_comment, subtitle_comment, LanguageSpec,
    };
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn subtitle_comment_extracts_and_strips_markers() {
        assert_eq!(subtitle_comment("video.sc", "video").as_deref(), Some("sc"));
        assert_eq!(subtitle_comment("Video.SC", "video").as_deref(), Some("SC"));
        assert_eq!(
            subtitle_comment("video.tc.subset", "video").as_deref(),
            Some("tc")
        );
        assert_eq!(subtitle_comment("video", "video"), None);
        assert_eq!(subtitle_comment("other.sc", "video"), None);
        assert_eq!(subtitle_comment("vide", "video"), None);
    }

    #[test]
    fn media_prefix_match_accepts_exact_name() {
        assert!(matches_media_prefix("show", "show"));
        assert!(matches_media_prefix("Show", "show"));
        assert!(matches_media_prefix("show.sc", "show"));
        assert!(!matches_media_prefix("other", "show"));
        assert!(!matches_media_prefix("show", "show.sc"));
    }

    #[test]
    fn standalone_comment_uses_first_dot() {
        assert_eq!(
            standalone_comment("[Group] Show - 01 [1080p].sc").as_deref(),
            Some("sc")
        );
        assert_eq!(standalone_comment("subtitle"), None);
        assert_eq!(standalone_comment("name.sc.subset").as_deref(), Some("sc"));
    }

    #[test]
    fn match_media_groups_pairs_by_prefix() {
        let videos = vec![PathBuf::from("dir/show01.mkv")];
        let subs = vec![
            PathBuf::from("dir/show01.sc.ass"),
            PathBuf::from("dir/show01.tc.ass"),
            PathBuf::from("dir/other.ass"),
        ];
        let (groups, unmatched) = match_media_groups(&videos, &subs);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].1.len(), 2);
        assert_eq!(unmatched, vec![PathBuf::from("dir/other.ass")]);
    }

    #[test]
    fn match_media_groups_drops_media_without_subs() {
        let videos = vec![PathBuf::from("a.mkv"), PathBuf::from("b.mkv")];
        let subs = vec![PathBuf::from("a.ass")];
        let (groups, unmatched) = match_media_groups(&videos, &subs);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, PathBuf::from("a.mkv"));
        assert!(unmatched.is_empty());
    }

    #[test]
    fn resolve_output_path_appends_counter() {
        let unique = format!(
            "assfonts-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.mks");
        assert_eq!(resolve_output_path(target.clone()), target);

        fs::write(&target, b"").unwrap();
        let resolved = resolve_output_path(target.clone());
        assert_eq!(resolved, dir.join("out#1.mks"));

        fs::write(&resolved, b"").unwrap();
        assert_eq!(
            resolve_output_path(target.clone()),
            dir.join("out#2.mks")
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn language_spec_parses_fixed_and_mappings() {
        let fixed = parse_language_spec("chi");
        assert_eq!(fixed.fixed.as_deref(), Some("chi"));
        assert_eq!(language_for_comment(Some("sc"), &fixed).as_deref(), Some("chi"));
        assert_eq!(language_for_comment(None, &fixed).as_deref(), Some("chi"));

        let mapped = parse_language_spec(".sc = chi ; .tc = zho ");
        assert!(mapped.fixed.is_none());
        assert_eq!(
            language_for_comment(Some("sc"), &mapped).as_deref(),
            Some("chi")
        );
        assert_eq!(
            language_for_comment(Some("tc"), &mapped).as_deref(),
            Some("zho")
        );
    }

    #[test]
    fn rename_proposals_align_names_and_skip_conflicts() {
        use super::{SuffixRule, rename_proposals};
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = format!(
            "assfonts-rename-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("show.mkv"), b"").unwrap();
        fs::write(dir.join("show01.sc.ass"), b"").unwrap();
        fs::write(dir.join("show01.tc.ass"), b"").unwrap();
        fs::write(dir.join("show.01.tc.ass"), b"").unwrap();
        fs::write(dir.join("other.ass"), b"").unwrap();

        let videos = vec![dir.join("show.mkv")];
        let subs = vec![
            dir.join("show01.sc.ass"),
            dir.join("show01.tc.ass"),
            dir.join("show.01.tc.ass"),
            dir.join("other.ass"),
        ];
        let (groups, unmatched) = match_media_groups(&videos, &subs);
        assert_eq!(groups.len(), 1);
        assert_eq!(unmatched, vec![dir.join("other.ass")]);

        let proposals = rename_proposals(&groups, &SuffixRule::Keep);
        let (from, to) = &proposals[0];
        assert_eq!(*from, dir.join("show01.sc.ass"));
        assert_eq!(*to, dir.join("show.01.sc.ass"));
        // show01.tc.ass -> show.01.tc.ass collides with the existing file; skipped.
        assert_eq!(proposals.len(), 1);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn suffix_rule_three_semantics() {
        use super::{SuffixRule, parse_suffix_rule};

        assert_eq!(parse_suffix_rule(""), SuffixRule::Keep);
        assert_eq!(parse_suffix_rule("   "), SuffixRule::Keep);
        assert_eq!(parse_suffix_rule("sc.ass"), SuffixRule::Fixed("sc.ass".into()));
        // Dots normalized.
        assert_eq!(
            parse_suffix_rule(".sc..ass"),
            SuffixRule::Fixed("sc.ass".into())
        );

        let map = parse_suffix_rule("chs|sc.Haruhana;CHT|tc.Haruhana");
        assert_eq!(
            map,
            SuffixRule::Map(vec![
                ("chs".to_string(), "sc.Haruhana".to_string()),
                ("cht".to_string(), "tc.Haruhana".to_string()),
            ])
        );
        // Case-insensitive lookup; miss falls back to keep.
        assert_eq!(map.suffix_for(Some("CHS")), Some("sc.Haruhana".into()));
        assert_eq!(map.suffix_for(Some("cht")), Some("tc.Haruhana".into()));
        assert_eq!(map.suffix_for(Some("eng")), None);
        assert_eq!(map.suffix_for(None), None);

        assert_eq!(SuffixRule::Keep.suffix_for(Some("sc")), None);
        assert_eq!(
            SuffixRule::Fixed("x.ass".into()).suffix_for(None),
            Some("x.ass".into())
        );
    }

    #[test]
    fn order_matching_pairs_by_sorted_position() {
        use super::{match_media_groups_by_order, rename_proposals, subtitle_base_name};

        assert_eq!(subtitle_base_name("[Haruhana] Show 01"), "[Haruhana] Show 01");
        assert_eq!(subtitle_base_name("Show 01.sc"), "Show 01");
        // Heuristic grouping key (SubRenamer semantics: first dot in the
        // trailing window) — same-series multi-language subs share one key.
        assert_eq!(subtitle_base_name("a.tiny.01.tc"), "a");

        // Differently-structured names pair purely by order.
        let videos = vec![
            PathBuf::from("Show - 02.mkv"),
            PathBuf::from("Show - 01.mkv"),
        ];
        let subs = vec![
            PathBuf::from("[Group]_02.chs.ass"),
            PathBuf::from("[Group]_02.cht.ass"),
            PathBuf::from("[Group]_01.chs.ass"),
        ];
        let (groups, unmatched) = match_media_groups_by_order(&videos, &subs);
        assert_eq!(groups.len(), 2);
        // Sorted videos: 01 first; sorted sub groups: [Group]_01 then [Group]_02 (two langs).
        assert_eq!(groups[0].0, PathBuf::from("Show - 01.mkv"));
        assert_eq!(groups[0].1, vec![PathBuf::from("[Group]_01.chs.ass")]);
        assert_eq!(groups[1].0, PathBuf::from("Show - 02.mkv"));
        assert_eq!(groups[1].1.len(), 2);
        assert!(unmatched.is_empty());

        // Map rule rewrites suffixes per original comment.
        let rule = super::parse_suffix_rule("chs|sc.Haruhana;cht|tc.Haruhana");
        let proposals = rename_proposals(&groups, &rule);
        let targets: Vec<String> = proposals
            .iter()
            .map(|(_, to)| to.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(targets.contains(&"Show - 01.sc.Haruhana.ass".to_string()));
        assert!(targets.contains(&"Show - 02.sc.Haruhana.ass".to_string()));
        assert!(targets.contains(&"Show - 02.tc.Haruhana.ass".to_string()));
    }

    #[test]
    fn order_matching_extra_subs_unmatched() {
        use super::match_media_groups_by_order;

        let videos = vec![PathBuf::from("v1.mkv")];
        let subs = vec![
            PathBuf::from("s1.ass"),
            PathBuf::from("s2.ass"),
        ];
        let (groups, unmatched) = match_media_groups_by_order(&videos, &subs);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, PathBuf::from("v1.mkv"));
        assert_eq!(unmatched, vec![PathBuf::from("s2.ass")]);
    }

    #[test]
    fn language_falls_back_to_builtin_map() {
        let spec = LanguageSpec::default();
        assert_eq!(
            language_for_comment(Some("cht"), &spec).as_deref(),
            Some("chi")
        );
        assert_eq!(
            language_for_comment(Some("jap"), &spec).as_deref(),
            Some("jpn")
        );
        assert_eq!(language_for_comment(Some("eng"), &spec), None);
        assert_eq!(language_for_comment(None, &spec), None);
    }
}
