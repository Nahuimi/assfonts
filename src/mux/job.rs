use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use clap::ValueEnum;
use regex::Regex;
use serde::{Deserialize, Serialize};

use super::scan::{natural_cmp, sort_natural};

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

impl LanguageSpec {
    /// 用户是否显式指定了语言（固定值或映射）；显式时优先于自动分类推导。
    pub fn is_explicit(&self) -> bool {
        self.fixed.is_some() || !self.map.is_empty()
    }
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
    key.trim().trim_start_matches('.').to_lowercase()
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

/// mks/mkv 轨道命名方式：英文（`KTXP.TC`）或中文（`极影字幕社.繁体`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum TrackNameStyle {
    /// 字幕组缩写 + 英文语言后缀（`KTXP.TC`）
    #[default]
    English,
    /// 字幕组中文名 + 中文分类名（`极影字幕社.繁体`）
    Chinese,
}

/// 语言分类 → 轨道语言标记：简体系列 `zh-Hans`、繁体系列 `zh-Hant`、
/// 日语 `jpn`、英语 `eng`。
pub fn language_tag_for_category(category: &SuffixCategory) -> Option<&'static str> {
    match category.name.as_str() {
        "简体" | "简日双语" | "简英双语" => Some("zh-Hans"),
        "繁体" | "繁日双语" | "繁英双语" => Some("zh-Hant"),
        "日语" => Some("jpn"),
        "英语" => Some("eng"),
        _ => None,
    }
}

/// 手动后缀文本（右键菜单里的自定义输入）→ 轨道语言标记。
fn language_tag_for_text(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    if lower.contains("hans")
        || lower.contains("简体")
        || lower.contains("简中")
        || lower.starts_with("sc")
    {
        Some("zh-Hans")
    } else if lower.contains("hant")
        || lower.contains("繁体")
        || lower.contains("繁中")
        || lower.starts_with("tc")
    {
        Some("zh-Hant")
    } else if lower == "jpn" || lower == "jp" || lower.ends_with("jp") || lower.contains("日语") {
        Some("jpn")
    } else if lower == "eng" || lower.contains("英语") {
        Some("eng")
    } else {
        None
    }
}

/// 一个 mks/mkv 字幕轨道的命名参数（标题 + 语言标记）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TrackMeta {
    pub title: Option<String>,
    pub language: Option<String>,
}

/// `subtitle_track_meta` 的输入：分类表、字幕组表、手动覆盖、命名方式与两个
/// 后缀开关（对应设置页的「字幕组后缀」「语言后缀」）。
pub struct TrackMetaOptions<'a> {
    pub categories: &'a [SuffixCategory],
    pub groups: &'a [FansubGroup],
    pub overrides: &'a BTreeMap<String, SuffixOverride>,
    pub style: TrackNameStyle,
    pub fansub_enabled: bool,
    pub language_enabled: bool,
}

impl TrackMetaOptions<'_> {
    fn join(&self, fansub: Option<&str>, language: Option<&str>) -> Option<String> {
        let mut parts: Vec<&str> = Vec::new();
        if self.fansub_enabled
            && let Some(value) = fansub
        {
            parts.push(value);
        }
        if self.language_enabled
            && let Some(value) = language
        {
            parts.push(value);
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("."))
        }
    }
}

/// 按「字幕组 + 语言分类」计算轨道标题与语言标记；手动覆盖优先于内置表。
///
/// 例：英文 `KTXP.TC`、中文 `极影字幕社.繁体`；两后缀都关时标题为 `None`。
pub fn subtitle_track_meta(comment: Option<&str>, options: &TrackMetaOptions<'_>) -> TrackMeta {
    let Some(comment) = comment else {
        return TrackMeta::default();
    };
    let key = naming_key(Some(comment), options.categories);
    if let Some(over) = options.overrides.get(&key) {
        let title = options.join(over.fansub.as_deref(), over.language.as_deref());
        let language = over
            .language
            .as_deref()
            .and_then(language_tag_for_text)
            .map(str::to_string);
        return TrackMeta { title, language };
    }

    let category = classify_in(options.categories, comment);
    let group = detect_fansub_group(comment, options.groups);
    let fansub = group.map(|group| match options.style {
        TrackNameStyle::English => group.abbr.as_str(),
        TrackNameStyle::Chinese => group.name.as_str(),
    });
    let language_suffix = category.map(|category| match options.style {
        TrackNameStyle::English => category.suffix.as_str(),
        TrackNameStyle::Chinese => category.name.as_str(),
    });
    TrackMeta {
        title: options.join(fansub, language_suffix),
        language: category
            .and_then(language_tag_for_category)
            .map(str::to_string),
    }
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

/// 后缀分类：每类带一组正则，字幕注释（`sc`/`chs.Haruhana`/`简日`…）命中哪一类，
/// 就套用哪一类的语言后缀。分类与正则都写在 `gui.toml` 的 `[[suffix_categories]]`
/// 里，改配置即可整体替换内置表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuffixCategory {
    pub name: String,
    pub suffix: String,
    #[serde(default)]
    pub patterns: Vec<String>,
}

/// 字幕组：右键菜单里选一个组，文件名会插入它的缩写（`.KTXP.SC.ass`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FansubGroup {
    pub name: String,
    pub abbr: String,
}

fn fansub(name: &str, abbr: &str) -> FansubGroup {
    FansubGroup {
        name: name.to_string(),
        abbr: abbr.to_string(),
    }
}

/// 内置字幕组表。`gui.toml` 的 `[[fansub_groups]]` 会整体替换它。
pub fn default_fansub_groups() -> Vec<FansubGroup> {
    vec![
        fansub("澄空学园字幕组", "SumiSora"),
        fansub("华盟字幕社", "CASO"),
        fansub("动漫国字幕组", "DMG"),
        fansub("诸神字幕组", "Kamigami"),
        fansub("极影字幕社", "KTXP"),
        fansub("SOSG字幕团", "SOSG"),
        fansub("雪飘工作室", "FLSnow"),
        fansub("千夏字幕组", "Airota"),
        fansub("喵萌奶茶屋", "Nekomoe Kissaten"),
        fansub("SweetSub", "SweetSub"),
        fansub("拨雪寻春", "Haruhana"),
        fansub("北宇治字幕组", "KitaujiSub"),
        fansub("霜庭云花", "STYHSub"),
        fansub("天使动漫字幕组", "TSDM"),
        fansub("枫雪字幕组", "OPFansMaplesnow"),
        fansub("悠哈璃羽字幕社", "UHA-WINGS"),
    ]
}

/// 七类语言分类，顺序即匹配优先级：先双语、后单语，避免「简日双语」被
/// 「简体」抢先命中。`gui.toml` 的 `[[suffix_categories]]` 会整体替换它。
pub fn default_suffix_categories() -> Vec<SuffixCategory> {
    fn category(name: &str, suffix: &str, patterns: &[&str]) -> SuffixCategory {
        SuffixCategory {
            name: name.to_string(),
            suffix: suffix.to_string(),
            patterns: patterns.iter().map(|pattern| pattern.to_string()).collect(),
        }
    }

    vec![
        category(
            "简日双语",
            "SCJP",
            &[
                r"(^|[^a-z0-9])(sc|chs|gb|zh-hans|zhhans|hans|简|简体|简中)[^a-z0-9]*(jp|jpn|jap|日|日语)",
                r"(^|[^a-z0-9])(jp|jpn|jap|日|日语)[^a-z0-9]*(sc|chs|gb|zh-hans|zhhans|hans|简|简体|简中)",
                r"(scjp|jpsc|chsjp|jpchs|scjpn)",
            ],
        ),
        category(
            "繁日双语",
            "TCJP",
            &[
                r"(^|[^a-z0-9])(tc|cht|big5|zh-hant|zhhant|hant|繁|繁体|繁中)[^a-z0-9]*(jp|jpn|jap|日|日语)",
                r"(^|[^a-z0-9])(jp|jpn|jap|日|日语)[^a-z0-9]*(tc|cht|big5|zh-hant|zhhant|hant|繁|繁体|繁中)",
                r"(tcjp|jptc|chtjp|jpcht)",
            ],
        ),
        category(
            "简英双语",
            "SCEN",
            &[
                r"(^|[^a-z0-9])(sc|chs|gb|zh-hans|zhhans|hans|简|简体|简中)[^a-z0-9]*(en|eng|english|英|英语)",
                r"(^|[^a-z0-9])(en|eng|english|英|英语)[^a-z0-9]*(sc|chs|gb|zh-hans|zhhans|hans|简|简体|简中)",
                r"(scen|ensc|chsen|engchs|sceng)",
            ],
        ),
        category(
            "繁英双语",
            "TCEN",
            &[
                r"(^|[^a-z0-9])(tc|cht|big5|zh-hant|zhhant|hant|繁|繁体|繁中)[^a-z0-9]*(en|eng|english|英|英语)",
                r"(^|[^a-z0-9])(en|eng|english|英|英语)[^a-z0-9]*(tc|cht|big5|zh-hant|zhhant|hant|繁|繁体|繁中)",
                r"(tcen|entc|chten|engcht|tceng)",
            ],
        ),
        category(
            "简体",
            "SC",
            &[r"(^|[^a-z0-9])(sc|chs|gb|gbk|zh-hans|zhhans|hans|简|简体|简中)($|[^a-z0-9])"],
        ),
        category(
            "繁体",
            "TC",
            &[r"(^|[^a-z0-9])(tc|cht|big5|zh-hant|zhhant|hant|繁|繁体|繁中)($|[^a-z0-9])"],
        ),
        category(
            "日语",
            "JPN",
            &[r"(^|[^a-z0-9])(jp|jpn|jap|japanese|日语)($|[^a-z0-9])"],
        ),
        category(
            "英语",
            "ENG",
            &[r"(^|[^a-z0-9])(eng|english|英语|英)($|[^a-z0-9])"],
        ),
    ]
}

pub fn default_categories() -> &'static [SuffixCategory] {
    use std::sync::LazyLock;

    static DEFAULT: LazyLock<Vec<SuffixCategory>> = LazyLock::new(default_suffix_categories);
    &DEFAULT
}

/// 正则按 pattern 缓存：重建表格时不会把几十条正则反复解析。
fn regex_matches(pattern: &str, text: &str) -> bool {
    use std::{
        collections::HashMap,
        sync::{Arc, LazyLock, Mutex},
    };

    static CACHE: LazyLock<Mutex<HashMap<String, Option<Arc<Regex>>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let cached = {
        let mut cache = match CACHE.lock() {
            Ok(cache) => cache,
            Err(poisoned) => poisoned.into_inner(),
        };
        cache
            .entry(pattern.to_string())
            .or_insert_with(|| Regex::new(pattern).ok().map(Arc::new))
            .clone()
    };
    cached.is_some_and(|regex| regex.is_match(text))
}

fn category_matches(category: &SuffixCategory, lower_text: &str) -> bool {
    category
        .patterns
        .iter()
        .any(|pattern| regex_matches(pattern, lower_text))
}

/// 在给定分类表里归类一段字幕注释（`sc.Haruhana`、`chs`、`简日`…）。
pub fn classify_in<'a>(categories: &'a [SuffixCategory], text: &str) -> Option<&'a SuffixCategory> {
    let lower = text.to_lowercase();
    categories
        .iter()
        .find(|category| category_matches(category, &lower))
}

/// 按内置分类表归类（CLI / 测试用）。
pub fn classify_subtitle_comment(text: &str) -> Option<&'static SuffixCategory> {
    classify_in(default_categories(), text)
}

/// 规则框里显示的文本（`简体|SC;简日双语|SCJP`）。
pub fn suffix_rule_text(categories: &[&SuffixCategory]) -> String {
    categories
        .iter()
        .map(|category| format!("{}|{}", category.name, category.suffix))
        .collect::<Vec<_>>()
        .join(";")
}

/// 从注释里识别字幕组（缩写或全名，大小写不敏感）。
pub fn detect_fansub_group<'a>(
    comment: &str,
    groups: &'a [FansubGroup],
) -> Option<&'a FansubGroup> {
    let lower = comment.to_lowercase();
    groups.iter().find(|group| {
        (!group.abbr.is_empty() && lower.contains(&group.abbr.to_lowercase()))
            || (!group.name.is_empty() && lower.contains(&group.name.to_lowercase()))
    })
}

/// 归类键：优先用分类名（同类字幕共享右键设置），否则退回注释文本。
pub fn naming_key(comment: Option<&str>, categories: &[SuffixCategory]) -> String {
    let Some(comment) = comment else {
        return String::new();
    };
    match classify_in(categories, comment) {
        Some(category) => category.name.to_lowercase(),
        None => comment.to_lowercase(),
    }
}

fn subtitle_comment_of(sub: &Path, video_stem: Option<&str>) -> Option<String> {
    let stem = sub.file_stem().and_then(|stem| stem.to_str())?;
    match video_stem {
        Some(video_stem) => subtitle_comment(stem, video_stem).or_else(|| standalone_comment(stem)),
        None => standalone_comment(stem),
    }
}

/// 扫描命中的分类名（按分类表顺序），用于自动填写后缀规则框。
pub fn detected_rule_text(
    videos: &[PathBuf],
    subs: &[PathBuf],
    categories: &[SuffixCategory],
) -> String {
    let pairs = pair_rename_groups(videos, subs);
    let mut hits: Vec<&SuffixCategory> = Vec::new();
    for (video, group) in &pairs {
        let video_stem = video
            .as_ref()
            .and_then(|video| video.file_stem())
            .and_then(|stem| stem.to_str());
        for sub in group {
            let Some(comment) = subtitle_comment_of(sub, video_stem) else {
                continue;
            };
            let Some(category) = classify_in(categories, &comment) else {
                continue;
            };
            if !hits.iter().any(|seen| seen.name == category.name) {
                hits.push(category);
            }
        }
    }
    suffix_rule_text(&hits)
}

/// 扫描结果里出现的分类（按分类表顺序去重）。保留给旧调用方。
pub fn detect_rename_categories(
    videos: &[PathBuf],
    subs: &[PathBuf],
) -> Vec<&'static SuffixCategory> {
    let pairs = pair_rename_groups(videos, subs);
    let mut found: Vec<&'static SuffixCategory> = Vec::new();
    for (video, group) in &pairs {
        let video_stem = video
            .as_ref()
            .and_then(|video| video.file_stem())
            .and_then(|stem| stem.to_str());
        for sub in group {
            let Some(comment) = subtitle_comment_of(sub, video_stem) else {
                continue;
            };
            let Some(category) = classify_subtitle_comment(&comment) else {
                continue;
            };
            if !found.iter().any(|seen| seen.name == category.name) {
                found.push(category);
            }
        }
    }
    default_categories()
        .iter()
        .filter(|category| found.iter().any(|seen| seen.name == category.name))
        .collect()
}

/// 右键菜单设置的字幕组 / 语言后缀，按 [`naming_key`] 归类后整组生效。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SuffixOverride {
    pub fansub: Option<String>,
    pub language: Option<String>,
}

/// 重命名页的命名参数（库层纯函数入口）。
pub struct RenameNaming<'a> {
    pub options: RenameOptions,
    pub rule: &'a SuffixRule,
    pub categories: &'a [SuffixCategory],
    pub groups: &'a [FansubGroup],
    pub overrides: &'a BTreeMap<String, SuffixOverride>,
}

/// Rename suffix rule (one input, three semantics — see PLAN-MATCH.md §B):
/// `Keep` keeps each subtitle's own comment suffix, `Fixed` applies one suffix
/// to all, `Map` maps a category name (or a literal comment) to a new suffix
/// (`简体|SC;chs|sc.Haruhana`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuffixRule {
    Keep,
    Fixed(String),
    Map(Vec<(String, String)>),
}

/// 配置里的默认规则表：七类分类各给一个默认后缀。重命名页的输入框本身默认
/// 留空（空 = 不生效），实际内容由扫描命中的分类自动填写。
pub const DEFAULT_SUFFIX_RULE: &str =
    "简体|SC;繁体|TC;简日双语|SCJP;繁日双语|TCJP;英语|ENG;简英双语|SCEN;繁英双语|TCEN";

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
    /// Resolve the rename suffix for one subtitle comment. Returns
    /// `(命中来源, 新后缀)` so the UI can show which rule fired; `None` keeps
    /// the original comment.
    fn suffix_for(&self, comment: Option<&str>) -> Option<(String, String)> {
        match self {
            Self::Keep => None,
            Self::Fixed(suffix) => Some((String::new(), suffix.clone())),
            Self::Map(map) => {
                let text = comment?;
                // 先按内置正则归类（`简体`、`简日双语`…），再按分类名查规则。
                if let Some(category) = classify_subtitle_comment(text) {
                    let key = category.name.to_lowercase();
                    if let Some((_, to)) = map.iter().find(|(from, _)| *from == key) {
                        return Some((category.name.to_string(), to.clone()));
                    }
                }
                // 兼容旧写法：直接把注释文本当作规则键（`chs|sc.Haruhana`）。
                let lower = text.to_lowercase();
                map.iter()
                    .find(|(from, _)| *from == lower)
                    .map(|(_, to)| (text.to_string(), to.clone()))
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
            let comment =
                subtitle_comment(sub_stem, video_stem).or_else(|| standalone_comment(sub_stem));
            let new_stem = match suffix_rule.suffix_for(comment.as_deref()) {
                Some((_, suffix)) => format!("{video_stem}.{suffix}"),
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

/// How rename targets are laid out (GUI choice, persisted in `gui.toml`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenameOptions {
    /// Put renamed subtitles next to their video instead of keeping them in the
    /// subtitle's own directory.
    pub move_to_video_dir: bool,
    /// 在文件名里插入字幕组缩写（`.KTXP`）。
    pub fansub_suffix: bool,
    /// 在文件名里插入语言后缀（`.SC`）。
    pub language_suffix: bool,
}

impl Default for RenameOptions {
    fn default() -> Self {
        Self {
            move_to_video_dir: true,
            fansub_suffix: false,
            language_suffix: false,
        }
    }
}

/// Whether one planned rename can actually run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameStatus {
    /// Target name differs from the current name and is free.
    Ready,
    /// The subtitle already has the target name.
    Unchanged,
    /// The target exists on disk or another row claims the same name.
    Blocked,
    /// No video paired with this subtitle group (or the name cannot be built).
    Unmatched,
}

/// One subtitle row of the rename table.
#[derive(Debug, Clone)]
pub struct RenameEntry {
    pub sub: PathBuf,
    /// Comment parsed from the file name (`chs_jpn` in `Show.01.chs_jpn.ass`).
    pub comment: Option<String>,
    /// 右键菜单的覆盖键（分类名小写，未分类时是注释文本）。
    pub key: String,
    /// 该行实际使用的语言后缀（`SC`）。
    pub language: Option<String>,
    /// 该行实际使用的字幕组缩写（`KTXP`）。
    pub fansub: Option<String>,
    /// 字幕组显示名（中文名；自定义后缀时是用户输入的文本），用于绿色标签。
    pub fansub_name: Option<String>,
    /// 语言显示名（分类中文名；自定义后缀时是用户输入的文本）。
    pub language_name: Option<String>,
    /// Target path; for unmatched rows this is the current path.
    pub target: PathBuf,
    /// 语言后缀来源（分类名 / 覆盖键，`("简体", "SC")`）。
    pub rule_hit: Option<(String, String)>,
    pub status: RenameStatus,
}

/// A video and its subtitles; `video == None` for subtitle groups left over.
#[derive(Debug, Clone)]
pub struct RenameRow {
    pub video: Option<PathBuf>,
    pub entries: Vec<RenameEntry>,
}

/// Counts shown in the rename page footer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenameSummary {
    pub videos: usize,
    pub subtitles: usize,
    pub ready: usize,
    pub blocked: usize,
    pub unmatched: usize,
}

impl RenameSummary {
    pub fn of(rows: &[RenameRow]) -> Self {
        let mut summary = Self {
            videos: rows.iter().filter(|row| row.video.is_some()).count(),
            ..Self::default()
        };
        for row in rows {
            for entry in &row.entries {
                summary.subtitles += 1;
                match entry.status {
                    RenameStatus::Ready => summary.ready += 1,
                    RenameStatus::Blocked => summary.blocked += 1,
                    RenameStatus::Unmatched => summary.unmatched += 1,
                    RenameStatus::Unchanged => {}
                }
            }
        }
        summary
    }

    /// True when at least one subtitle would actually be renamed.
    pub fn has_ready(&self) -> bool {
        self.ready > 0
    }
}

/// Pair naturally sorted videos with subtitle groups ordered by base name
/// (`subtitle_base_name`), i.e. SubRenamer-style positional pairing. Extra
/// subtitle groups get `None`; videos without subtitles keep an empty group so
/// a drop that only contains videos still shows what was recognised.
pub fn pair_rename_groups(
    videos: &[PathBuf],
    subs: &[PathBuf],
) -> Vec<(Option<PathBuf>, Vec<PathBuf>)> {
    let mut sorted_videos = videos.to_vec();
    sort_natural(&mut sorted_videos);

    let mut by_base: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for sub in subs {
        let stem = sub
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_lowercase();
        let base = subtitle_base_name(&stem);
        by_base.entry(base).or_default().push(sub.clone());
    }

    let mut bases: Vec<String> = by_base.keys().cloned().collect();
    bases.sort_by(|left, right| natural_cmp(left, right));

    let mut pairs = Vec::with_capacity(bases.len());
    for (index, base) in bases.iter().enumerate() {
        let mut group = by_base.remove(base).unwrap_or_default();
        sort_natural(&mut group);
        pairs.push((sorted_videos.get(index).cloned(), group));
    }
    // Videos without a subtitle group still get a row (empty group), so dropping
    // only videos is visible instead of silently doing nothing.
    for video in sorted_videos.iter().skip(bases.len()) {
        pairs.push((Some(video.clone()), Vec::new()));
    }
    pairs
}

/// Build the rename table: one row per video/subtitle group, one entry per
/// subtitle. Targets are `<video stem>[.<fansub>][.<language>].<sub extension>`
/// and never include the video extension.
pub fn build_rename_rows(
    videos: &[PathBuf],
    subs: &[PathBuf],
    naming: &RenameNaming<'_>,
) -> Vec<RenameRow> {
    let pairs = pair_rename_groups(videos, subs);
    build_rename_rows_with(&pairs, naming, &|path: &Path| path.exists())
}

/// 兼容旧签名：用内置分类表、无覆盖项重建表格（CLI / 测试）。
pub fn build_rename_rows_from(
    pairs: &[(Option<PathBuf>, Vec<PathBuf>)],
    rule: &SuffixRule,
    options: RenameOptions,
    exists: &dyn Fn(&Path) -> bool,
) -> Vec<RenameRow> {
    let categories = default_suffix_categories();
    let groups = default_fansub_groups();
    let overrides = BTreeMap::new();
    let naming = RenameNaming {
        options,
        rule,
        categories: &categories,
        groups: &groups,
        overrides: &overrides,
    };
    build_rename_rows_with(pairs, &naming, exists)
}

/// 语言后缀：分类映射（`SuffixRule::Map`）> 分类自带后缀。
/// 右键手动覆盖在调用点优先处理，不进入这里。
fn language_suffix_of(category: Option<&SuffixCategory>, rule: &SuffixRule) -> Option<String> {
    let category = category?;
    if let SuffixRule::Map(map) = rule {
        let key = category.name.to_lowercase();
        if let Some((_, to)) = map.iter().find(|(from, _)| *from == key) {
            return Some(to.clone());
        }
    }
    Some(category.suffix.clone())
}

/// Pure core of [`build_rename_rows`]: `exists` is injectable so tests do not
/// touch the filesystem for collision checks.
pub fn build_rename_rows_with(
    pairs: &[(Option<PathBuf>, Vec<PathBuf>)],
    naming: &RenameNaming<'_>,
    exists: &dyn Fn(&Path) -> bool,
) -> Vec<RenameRow> {
    let mut claimed: Vec<PathBuf> = Vec::new();
    let mut rows = Vec::with_capacity(pairs.len());

    for (video, group) in pairs {
        let video_stem = video
            .as_ref()
            .and_then(|video| video.file_stem())
            .and_then(|stem| stem.to_str());
        let mut entries = Vec::with_capacity(group.len());

        for sub in group {
            let comment = subtitle_comment_of(sub, video_stem);
            let key = naming_key(comment.as_deref(), naming.categories);
            let overrides = naming.overrides.get(&key);
            let category = comment
                .as_deref()
                .and_then(|comment| classify_in(naming.categories, comment));

            if video.is_none() || video_stem.is_none() {
                entries.push(RenameEntry {
                    sub: sub.clone(),
                    comment,
                    key,
                    language: None,
                    fansub: None,
                    fansub_name: None,
                    language_name: None,
                    target: sub.clone(),
                    rule_hit: None,
                    status: RenameStatus::Unmatched,
                });
                continue;
            }

            let Some(extension) = sub.extension().and_then(|ext| ext.to_str()) else {
                entries.push(RenameEntry {
                    sub: sub.clone(),
                    comment,
                    key,
                    language: None,
                    fansub: None,
                    fansub_name: None,
                    language_name: None,
                    target: sub.clone(),
                    rule_hit: None,
                    status: RenameStatus::Unmatched,
                });
                continue;
            };

            // 优先级：右键手动设置 > 开关打开时的内置规则自动识别。
            // 手动设置即使对应开关关闭也生效（用户明确指定过这一行/这一组）。
            let (fansub, fansub_name) =
                match overrides.and_then(|overrides| overrides.fansub.clone()) {
                    // 自定义后缀：写什么就用什么（后缀与标签同文本）。
                    Some(custom) => (Some(custom.clone()), Some(custom)),
                    None if naming.options.fansub_suffix => {
                        match comment
                            .as_deref()
                            .and_then(|comment| detect_fansub_group(comment, naming.groups))
                        {
                            Some(group) => (Some(group.abbr.clone()), Some(group.name.clone())),
                            None => (None, None),
                        }
                    }
                    None => (None, None),
                };
            let language = match overrides.and_then(|overrides| overrides.language.clone()) {
                Some(custom) => Some(custom),
                None if naming.options.language_suffix => language_suffix_of(category, naming.rule),
                None => None,
            };
            let language_name = language.as_ref().and_then(|_| {
                overrides
                    .and_then(|overrides| overrides.language.clone())
                    .or_else(|| category.map(|category| category.name.clone()))
            });

            // 两个开关都没产出时退回旧语义：规则映射，或保留注释后缀。
            let legacy_hit = if fansub.is_none() && language.is_none() {
                naming.rule.suffix_for(comment.as_deref())
            } else {
                None
            };
            let suffix = if fansub.is_none() && language.is_none() {
                legacy_hit
                    .as_ref()
                    .map(|(_, to)| to.clone())
                    .or_else(|| comment.clone())
            } else {
                let mut parts: Vec<String> = Vec::new();
                if let Some(fansub) = &fansub {
                    parts.push(fansub.clone());
                }
                if let Some(language) = &language {
                    parts.push(language.clone());
                }
                Some(parts.join("."))
            };

            let rule_hit = language
                .as_ref()
                .map(|language| (key.clone(), language.clone()))
                .or(legacy_hit);

            let file_name = match suffix {
                Some(suffix) if !suffix.is_empty() => {
                    format!(
                        "{}.{}.{}",
                        video_stem.unwrap_or_default(),
                        suffix,
                        extension
                    )
                }
                _ => format!("{}.{}", video_stem.unwrap_or_default(), extension),
            };
            let video = video.as_ref().expect("checked above");
            let video_dir = if naming.options.move_to_video_dir {
                video
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
            } else {
                None
            };
            let directory = video_dir
                .or_else(|| sub.parent().filter(|parent| !parent.as_os_str().is_empty()))
                .map(Path::to_path_buf)
                .unwrap_or_default();
            let target = directory.join(file_name);

            let duplicate = claimed.contains(&target);
            let status = if target == *sub {
                RenameStatus::Unchanged
            } else if duplicate || exists(&target) {
                RenameStatus::Blocked
            } else {
                RenameStatus::Ready
            };
            claimed.push(target.clone());

            entries.push(RenameEntry {
                sub: sub.clone(),
                comment,
                key,
                language,
                fansub,
                fansub_name,
                language_name,
                target,
                rule_hit,
                status,
            });
        }

        if video.is_some() || !entries.is_empty() {
            rows.push(RenameRow {
                video: video.clone(),
                entries,
            });
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        LanguageSpec, language_for_comment, match_media_groups, matches_media_prefix,
        parse_language_spec, resolve_output_path, standalone_comment, subtitle_comment,
    };

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
        assert_eq!(resolve_output_path(target.clone()), dir.join("out#2.mks"));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn language_spec_parses_fixed_and_mappings() {
        let fixed = parse_language_spec("chi");
        assert_eq!(fixed.fixed.as_deref(), Some("chi"));
        assert_eq!(
            language_for_comment(Some("sc"), &fixed).as_deref(),
            Some("chi")
        );
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
        use std::{
            fs,
            time::{SystemTime, UNIX_EPOCH},
        };

        use super::{SuffixRule, rename_proposals};

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
        assert_eq!(
            parse_suffix_rule("sc.ass"),
            SuffixRule::Fixed("sc.ass".into())
        );
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
        // Case-insensitive literal lookup; miss falls back to keep.
        assert_eq!(
            map.suffix_for(Some("CHS")),
            Some(("CHS".to_string(), "sc.Haruhana".to_string()))
        );
        assert_eq!(
            map.suffix_for(Some("cht")),
            Some(("cht".to_string(), "tc.Haruhana".to_string()))
        );
        assert_eq!(map.suffix_for(Some("eng")), None);
        assert_eq!(map.suffix_for(None), None);

        assert_eq!(SuffixRule::Keep.suffix_for(Some("sc")), None);
        assert_eq!(
            SuffixRule::Fixed("x.ass".into()).suffix_for(None),
            Some((String::new(), "x.ass".to_string()))
        );
        // 分类名键：命中「简体」后套用该分类的后缀。
        let category = parse_suffix_rule("简体|SC;繁体|TC");
        assert_eq!(
            category.suffix_for(Some("chs.Haruhana")),
            Some(("简体".to_string(), "SC".to_string()))
        );
        assert_eq!(
            category.suffix_for(Some("cht")),
            Some(("繁体".to_string(), "TC".to_string()))
        );
    }

    #[test]
    fn order_matching_pairs_by_sorted_position() {
        use super::{match_media_groups_by_order, rename_proposals, subtitle_base_name};

        assert_eq!(
            subtitle_base_name("[Haruhana] Show 01"),
            "[Haruhana] Show 01"
        );
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
        let subs = vec![PathBuf::from("s1.ass"), PathBuf::from("s2.ass")];
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

#[cfg(test)]
mod rename_tests {
    use std::path::{Path, PathBuf};

    use super::{
        RenameOptions, RenameRow, RenameStatus, RenameSummary, SuffixRule, build_rename_rows_from,
        pair_rename_groups,
    };

    fn never_exists(_path: &Path) -> bool {
        false
    }

    fn paths(items: &[&str]) -> Vec<PathBuf> {
        items.iter().map(PathBuf::from).collect()
    }

    fn keep() -> SuffixRule {
        SuffixRule::Keep
    }

    fn map(pairs: &[(&str, &str)]) -> SuffixRule {
        // 与 `parse_suffix_rule` 保持一致：映射的键统一小写后比较。
        SuffixRule::Map(
            pairs
                .iter()
                .map(|(from, to)| ((*from).to_ascii_lowercase(), (*to).to_string()))
                .collect(),
        )
    }

    fn rows_for(
        videos: &[&str],
        subs: &[&str],
        rule: &SuffixRule,
        options: RenameOptions,
        exists: &dyn Fn(&Path) -> bool,
    ) -> Vec<RenameRow> {
        let pairs = pair_rename_groups(&paths(videos), &paths(subs));
        build_rename_rows_from(&pairs, rule, options, exists)
    }

    fn entry_targets(rows: &[RenameRow]) -> Vec<PathBuf> {
        rows.iter()
            .flat_map(|row| row.entries.iter())
            .map(|entry| entry.target.clone())
            .collect()
    }

    fn entry_statuses(rows: &[RenameRow]) -> Vec<RenameStatus> {
        rows.iter()
            .flat_map(|row| row.entries.iter())
            .map(|entry| entry.status)
            .collect()
    }

    #[test]
    fn pairing_sorts_videos_naturally_and_matches_by_position() {
        let videos = paths(&[
            "dir/[Group] Show - 10 [1080p].mkv",
            "dir/[Group] Show - 02 [1080p].mkv",
            "dir/[Group] Show - 01 [1080p].mkv",
        ]);
        let subs = paths(&[
            "dir/[Group] Show - 10 [1080p].chs.ass",
            "dir/[Group] Show - 01 [1080p].chs.ass",
            "dir/[Group] Show - 02 [1080p].cht.ass",
        ]);
        let pairs = pair_rename_groups(&videos, &subs);
        let video_names: Vec<String> = pairs
            .iter()
            .filter_map(|(video, _)| video.clone())
            .map(|video| video.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            video_names,
            vec![
                "dir/[Group] Show - 01 [1080p].mkv",
                "dir/[Group] Show - 02 [1080p].mkv",
                "dir/[Group] Show - 10 [1080p].mkv",
            ]
        );
        for (_, group) in &pairs {
            assert_eq!(group.len(), 1);
        }
    }

    #[test]
    fn pairing_groups_multi_language_subs_of_one_episode() {
        let videos = paths(&["dir/Show - 01.mkv"]);
        let subs = paths(&[
            "dir/Show - 01.chs.ass",
            "dir/Show - 01.cht.ass",
            "dir/Show - 02.chs.ass",
        ]);
        let pairs = pair_rename_groups(&videos, &subs);
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].1.len(), 2);
        assert_eq!(pairs[1].0, None, "second subtitle group has no video");
    }

    #[test]
    fn keep_rule_leaves_already_correct_names_unchanged() {
        let rows = rows_for(
            &["dir/Show - 01.mkv"],
            &["dir/Show - 01.chs.ass"],
            &keep(),
            RenameOptions::default(),
            &never_exists,
        );
        assert_eq!(entry_targets(&rows), paths(&["dir/Show - 01.chs.ass"]));
        assert_eq!(entry_statuses(&rows), vec![RenameStatus::Unchanged]);
    }

    #[test]
    fn map_rule_renames_suffix_and_keeps_original_extension_without_video_ext() {
        let rows = rows_for(
            &["dir/Show 01.mkv"],
            &["dir/Show 01.chs.ass", "dir/Show 01.cht.ass"],
            &map(&[("chs", "sc")]),
            RenameOptions::default(),
            &never_exists,
        );
        assert_eq!(
            entry_targets(&rows),
            paths(&["dir/Show 01.sc.ass", "dir/Show 01.cht.ass"])
        );
        assert_eq!(
            entry_statuses(&rows),
            vec![RenameStatus::Ready, RenameStatus::Unchanged]
        );
        assert!(!entry_targets(&rows)[0].to_string_lossy().contains(".mkv"));
    }

    #[test]
    fn existing_target_blocks_rename() {
        let rows = rows_for(
            &["dir/Show 01.mkv"],
            &["dir/Show 01.chs.ass"],
            &map(&[("chs", "sc")]),
            RenameOptions::default(),
            &|_path: &Path| true,
        );
        assert_eq!(entry_statuses(&rows), vec![RenameStatus::Blocked]);
    }

    #[test]
    fn duplicate_target_within_one_plan_blocks_second_entry() {
        let rows = rows_for(
            &["dir/Show 01.mkv"],
            &["dir/Show 01.a.ass", "dir/Show 01.b.ass"],
            &map(&[("a", "sc"), ("b", "sc")]),
            RenameOptions::default(),
            &never_exists,
        );
        assert_eq!(
            entry_statuses(&rows),
            vec![RenameStatus::Ready, RenameStatus::Blocked]
        );
    }

    #[test]
    fn subtitle_group_without_video_is_unmatched() {
        let rows = rows_for(
            &["dir/Show - 01.mkv"],
            &["dir/Show - 01.chs.ass", "dir/Show - 02.chs.ass"],
            &keep(),
            RenameOptions::default(),
            &never_exists,
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].video, None);
        assert_eq!(
            entry_statuses(&rows),
            vec![RenameStatus::Unchanged, RenameStatus::Unmatched]
        );
        assert_eq!(
            entry_targets(&rows)[1],
            PathBuf::from("dir/Show - 02.chs.ass"),
            "unmatched rows keep their current path"
        );
    }

    #[test]
    fn in_place_mode_keeps_the_subtitle_directory() {
        let rows = rows_for(
            &["video/Show 01.mkv"],
            &["subs/Show 01.chs.ass"],
            &map(&[("chs", "sc")]),
            RenameOptions {
                move_to_video_dir: false,
                ..RenameOptions::default()
            },
            &never_exists,
        );
        assert_eq!(entry_targets(&rows), paths(&["subs/Show 01.sc.ass"]));
    }

    #[test]
    fn move_to_video_dir_places_target_beside_the_video() {
        let rows = rows_for(
            &["video/Show 01.mkv"],
            &["subs/Show 01.chs.ass"],
            &map(&[("chs", "sc")]),
            RenameOptions::default(),
            &never_exists,
        );
        assert_eq!(entry_targets(&rows), paths(&["video/Show 01.sc.ass"]));
    }

    #[test]
    fn summary_counts_statuses() {
        let rows = rows_for(
            &["dir/Show 01.mkv", "dir/Show 02.mkv"],
            &[
                "dir/Show 01.chs.ass",
                "dir/Show 01.cht.ass",
                "dir/Show 02.chs.ass",
                "dir/Show 03.chs.ass",
            ],
            &map(&[("chs", "sc")]),
            RenameOptions::default(),
            &never_exists,
        );
        let summary = RenameSummary::of(&rows);
        assert_eq!(summary.videos, 2);
        assert_eq!(summary.subtitles, 4);
        assert!(summary.has_ready());
        assert_eq!(
            summary.ready + summary.blocked + summary.unmatched,
            summary.subtitles - 1,
            "one of Show 01/02/03 keeps its current name"
        );
    }

    #[test]
    fn default_suffix_rule_maps_categories() {
        use super::{DEFAULT_SUFFIX_RULE, parse_suffix_rule};

        let SuffixRule::Map(pairs) = parse_suffix_rule(DEFAULT_SUFFIX_RULE) else {
            panic!("内置规则应当是分类映射");
        };
        assert!(pairs.contains(&("简体".to_string(), "SC".to_string())));
        assert!(pairs.contains(&("繁体".to_string(), "TC".to_string())));
        assert!(pairs.contains(&("简日双语".to_string(), "SCJP".to_string())));
        assert!(pairs.contains(&("繁日双语".to_string(), "TCJP".to_string())));
        assert!(pairs.contains(&("英语".to_string(), "ENG".to_string())));
        assert!(pairs.contains(&("简英双语".to_string(), "SCEN".to_string())));
        assert!(pairs.contains(&("繁英双语".to_string(), "TCEN".to_string())));
    }

    #[test]
    fn suffix_categories_classify_comments() {
        use super::{classify_subtitle_comment, detect_rename_categories, suffix_rule_text};

        let name =
            |text: &str| classify_subtitle_comment(text).map(|category| category.name.as_str());
        assert_eq!(name("sc"), Some("简体"));
        assert_eq!(name("chs.Haruhana"), Some("简体"));
        assert_eq!(name("tc"), Some("繁体"));
        assert_eq!(name("cht.Haruhana"), Some("繁体"));
        assert_eq!(name("sc.jp"), Some("简日双语"));
        assert_eq!(name("简日"), Some("简日双语"));
        assert_eq!(name("TCJP"), Some("繁日双语"));
        assert_eq!(name("eng"), Some("英语"));
        assert_eq!(name("SCEN"), Some("简英双语"));
        assert_eq!(name("繁英"), Some("繁英双语"));
        // BCP 47 写法（大小写混写由 classify_in 统一小写后匹配）。
        assert_eq!(name("zh-Hans"), Some("简体"));
        assert_eq!(name("Show 01.zh-hant.ass"), Some("繁体"));
        assert_eq!(name("zh-Hans.jp"), Some("简日双语"));
        assert_eq!(name("zh-Hant-jpn"), Some("繁日双语"));
        assert_eq!(name("eng.zh-Hans"), Some("简英双语"));
        assert_eq!(name("zh-Hant.eng"), Some("繁英双语"));
        assert_eq!(name("Haruhana"), None);

        let found = detect_rename_categories(
            &paths(&["dir/Show 01.mkv", "dir/Show 02.mkv"]),
            &paths(&["dir/Show 01.sc.ass", "dir/Show 02.cht.ass"]),
        );
        assert_eq!(suffix_rule_text(&found), "简体|SC;繁体|TC");
    }

    #[test]
    fn videos_without_subtitles_still_get_a_row() {
        let rows = rows_for(
            &["dir/Show 01.mkv", "dir/Show 02.mkv"],
            &[],
            &keep(),
            RenameOptions::default(),
            &never_exists,
        );
        assert_eq!(rows.len(), 2, "only-video drops must be visible");
        assert!(rows.iter().all(|row| row.entries.is_empty()));
        let summary = RenameSummary::of(&rows);
        assert_eq!(summary.videos, 2);
        assert_eq!(summary.subtitles, 0);
    }

    #[test]
    fn language_and_fansub_suffixes_compose_in_order() {
        use std::collections::BTreeMap;

        let categories = super::default_suffix_categories();
        let groups = super::default_fansub_groups();
        let overrides = BTreeMap::new();
        let rule = keep();
        let naming = super::RenameNaming {
            options: RenameOptions {
                move_to_video_dir: true,
                fansub_suffix: true,
                language_suffix: true,
            },
            rule: &rule,
            categories: &categories,
            groups: &groups,
            overrides: &overrides,
        };
        let pairs = pair_rename_groups(
            &paths(&["dir/Show 01.mkv"]),
            &paths(&["dir/Show 01.sc.KTXP.ass"]),
        );
        let rows = super::build_rename_rows_with(&pairs, &naming, &never_exists);
        assert_eq!(
            entry_targets(&rows),
            paths(&["dir/Show 01.KTXP.SC.ass"]),
            "字幕组在前、语言在后"
        );
        let entry = &rows[0].entries[0];
        assert_eq!(entry.fansub.as_deref(), Some("KTXP"));
        assert_eq!(entry.language.as_deref(), Some("SC"));
        // 标签显示中文名与分类名，文件名里仍是英文缩写。
        assert_eq!(entry.fansub_name.as_deref(), Some("极影字幕社"));
        assert_eq!(entry.language_name.as_deref(), Some("简体"));
    }

    #[test]
    fn custom_override_text_is_used_verbatim_for_both_suffixes() {
        use std::collections::BTreeMap;

        let categories = super::default_suffix_categories();
        let groups = super::default_fansub_groups();
        let rule = keep();
        let mut overrides = BTreeMap::new();
        overrides.insert(
            "简体".to_string(),
            super::SuffixOverride {
                fansub: Some("MyGroup".to_string()),
                language: Some("MyLang".to_string()),
            },
        );
        let naming = super::RenameNaming {
            options: RenameOptions {
                move_to_video_dir: true,
                fansub_suffix: true,
                language_suffix: true,
            },
            rule: &rule,
            categories: &categories,
            groups: &groups,
            overrides: &overrides,
        };
        let pairs = pair_rename_groups(
            &paths(&["dir/Show 01.mkv"]),
            &paths(&["dir/Show 01.chs.Haruhana.ass"]),
        );
        let rows = super::build_rename_rows_with(&pairs, &naming, &never_exists);
        assert_eq!(
            entry_targets(&rows),
            paths(&["dir/Show 01.MyGroup.MyLang.ass"])
        );
        let entry = &rows[0].entries[0];
        assert_eq!(entry.fansub_name.as_deref(), Some("MyGroup"));
        assert_eq!(entry.language_name.as_deref(), Some("MyLang"));
    }

    #[test]
    fn fansub_detected_from_comment_and_override_wins() {
        use std::collections::BTreeMap;

        let categories = super::default_suffix_categories();
        let groups = super::default_fansub_groups();
        let rule = keep();
        let mut overrides = BTreeMap::new();
        overrides.insert(
            "简体".to_string(),
            super::SuffixOverride {
                fansub: Some("CASO".to_string()),
                language: Some("SC".to_string()),
            },
        );
        let naming = super::RenameNaming {
            options: RenameOptions {
                move_to_video_dir: true,
                fansub_suffix: true,
                language_suffix: true,
            },
            rule: &rule,
            categories: &categories,
            groups: &groups,
            overrides: &overrides,
        };
        let pairs = pair_rename_groups(
            &paths(&["dir/Show 01.mkv"]),
            &paths(&["dir/Show 01.chs.Haruhana.ass"]),
        );
        let rows = super::build_rename_rows_with(&pairs, &naming, &never_exists);
        assert_eq!(entry_targets(&rows), paths(&["dir/Show 01.CASO.SC.ass"]));
        // 覆盖值直接作为标签文本（自定义后缀时“写什么就是什么”）。
        let entry = &rows[0].entries[0];
        assert_eq!(entry.fansub_name.as_deref(), Some("CASO"));
        assert_eq!(entry.language_name.as_deref(), Some("SC"));
    }

    #[test]
    fn manual_override_wins_even_when_auto_switches_are_off() {
        use std::collections::BTreeMap;

        let categories = super::default_suffix_categories();
        let groups = super::default_fansub_groups();
        let rule = keep();
        let mut overrides = BTreeMap::new();
        overrides.insert(
            "简体".to_string(),
            super::SuffixOverride {
                fansub: Some("Custom".to_string()),
                language: Some("XL".to_string()),
            },
        );
        let naming = super::RenameNaming {
            options: RenameOptions {
                move_to_video_dir: true,
                fansub_suffix: false,
                language_suffix: false,
            },
            rule: &rule,
            categories: &categories,
            groups: &groups,
            overrides: &overrides,
        };
        let pairs = pair_rename_groups(
            &paths(&["dir/Show 01.mkv"]),
            &paths(&["dir/Show 01.chs.Haruhana.ass"]),
        );
        let rows = super::build_rename_rows_with(&pairs, &naming, &never_exists);
        assert_eq!(entry_targets(&rows), paths(&["dir/Show 01.Custom.XL.ass"]));
    }

    #[test]
    fn tomllib_categories_are_configurable() {
        use super::{classify_in, default_suffix_categories};

        let mut categories = default_suffix_categories();
        categories.retain(|category| category.name != "英语");
        categories.insert(
            0,
            super::SuffixCategory {
                name: "自定义".to_string(),
                suffix: "CUS".to_string(),
                patterns: vec![r"(^|[^a-z0-9])custom".to_string()],
            },
        );
        let found = classify_in(&categories, "custom.en").expect("自定义分类应命中");
        assert_eq!(found.suffix, "CUS");
        assert!(classify_in(&categories, "eng").is_none(), "英语分类已删除");
    }

    #[test]
    fn rule_hit_records_the_applied_mapping() {
        let rows = rows_for(
            &["dir/Show 01.mkv"],
            &["dir/Show 01.sc.Haruhana.ass", "dir/Show 01.cht.ass"],
            &map(&[("sc.Haruhana", "chs.Haruhana")]),
            RenameOptions::default(),
            &never_exists,
        );
        let hits: Vec<Option<(String, String)>> = rows
            .iter()
            .flat_map(|row| row.entries.iter())
            .map(|entry| entry.rule_hit.clone())
            .collect();
        // 组内字幕按自然排序（cht 在 sc.Haruhana 之前），所以命中记录在后。
        assert_eq!(
            hits,
            vec![
                None,
                Some(("sc.Haruhana".to_string(), "chs.Haruhana".to_string())),
            ]
        );
    }
}

#[cfg(test)]
mod track_meta_tests {
    use super::*;

    fn meta(
        comment: &str,
        style: TrackNameStyle,
        fansub_enabled: bool,
        language_enabled: bool,
    ) -> TrackMeta {
        let categories = default_categories().to_vec();
        let groups = default_fansub_groups();
        let overrides = BTreeMap::new();
        let options = TrackMetaOptions {
            categories: &categories,
            groups: &groups,
            overrides: &overrides,
            style,
            fansub_enabled,
            language_enabled,
        };
        subtitle_track_meta(Some(comment), &options)
    }

    #[test]
    fn english_track_name_uses_abbr_and_suffix() {
        let got = meta("Show 01.KTXP.TC", TrackNameStyle::English, true, true);
        assert_eq!(got.title.as_deref(), Some("KTXP.TC"));
        assert_eq!(got.language.as_deref(), Some("zh-Hant"));
    }

    #[test]
    fn chinese_track_name_uses_group_name_and_category() {
        let got = meta("Show 01.KTXP.TC", TrackNameStyle::Chinese, true, true);
        assert_eq!(got.title.as_deref(), Some("极影字幕社.繁体"));
        assert_eq!(got.language.as_deref(), Some("zh-Hant"));
    }

    #[test]
    fn track_name_respects_suffix_switches() {
        let fansub_only = meta("Show 01.KTXP.TC", TrackNameStyle::English, true, false);
        assert_eq!(fansub_only.title.as_deref(), Some("KTXP"));
        let language_only = meta("Show 01.KTXP.TC", TrackNameStyle::English, false, true);
        assert_eq!(language_only.title.as_deref(), Some("TC"));
        let off = meta("Show 01.KTXP.TC", TrackNameStyle::English, false, false);
        assert_eq!(off.title, None);
    }

    #[test]
    fn japanese_category_sets_jpn_language() {
        let got = meta("Show 01.jp", TrackNameStyle::English, true, true);
        assert_eq!(got.title.as_deref(), Some("JPN"));
        assert_eq!(got.language.as_deref(), Some("jpn"));
    }

    #[test]
    fn manual_override_wins_over_builtin_tables() {
        let categories = default_categories().to_vec();
        let groups = default_fansub_groups();
        let mut overrides = BTreeMap::new();
        overrides.insert(
            "简体".to_string(),
            SuffixOverride {
                fansub: Some("Custom".to_string()),
                language: Some("SCJP".to_string()),
            },
        );
        let options = TrackMetaOptions {
            categories: &categories,
            groups: &groups,
            overrides: &overrides,
            style: TrackNameStyle::English,
            fansub_enabled: true,
            language_enabled: true,
        };
        let got = subtitle_track_meta(Some("Show 01.sc"), &options);
        assert_eq!(got.title.as_deref(), Some("Custom.SCJP"));
        assert_eq!(got.language.as_deref(), Some("zh-Hans"));
    }
}
