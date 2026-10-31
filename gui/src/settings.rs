//! GUI 持久化设置：`GuiSettings`、默认值与 `gui.toml` 读取。

use crate::prelude::*;

/// Persisted GUI state, stored as TOML under `<config_dir>/assfonts/gui.toml`.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct GuiSettings {
    pub(crate) theme_choice: usize,
    /// 固定侧边栏（true 时始终展开；false 时鼠标悬停才展开）。
    /// 旧版本写的是 `sidebar_collapsed`，读取时按未知字段忽略。
    #[serde(default = "default_true")]
    pub(crate) sidebar_pinned: bool,
    pub(crate) format_index: usize,
    pub(crate) muxer_index: usize,
    pub(crate) strict_mode: bool,
    pub(crate) allow_missing_sample: bool,
    pub(crate) allow_missing_fonts: bool,
    pub(crate) rename_fonts: bool,
    pub(crate) keep_temp: bool,
    pub(crate) keep_subs: bool,
    pub(crate) keep_attachments: bool,
    pub(crate) utf8_sig: bool,
    pub(crate) char_compatible: bool,
    pub(crate) use_embedded_fonts: bool,
    #[serde(default)]
    pub(crate) dbpath: String,
    #[serde(default)]
    pub(crate) output: String,
    #[serde(default = "default_video_exts")]
    pub(crate) video_exts: String,
    #[serde(default = "default_sub_exts")]
    pub(crate) sub_exts: String,
    /// Keep renamed subtitles in their own folder instead of moving them next
    /// to the paired video (the default is to move them).
    #[serde(default)]
    pub(crate) rename_keep_in_place: bool,
    /// Copy the subtitle to its new name instead of renaming it in place.
    #[serde(default)]
    pub(crate) rename_copy_mode: bool,
    /// 在最终文件名里插入字幕组缩写（`.KTXP`）。
    #[serde(default)]
    pub(crate) fansub_suffix: bool,
    /// 在最终文件名里插入语言后缀（`.SC`）。
    #[serde(default)]
    pub(crate) language_suffix: bool,
    /// mks/mkv 字幕轨命名方式：英文（KTXP-TC）或中文（极影字幕社-繁体）。
    #[serde(default = "default_track_name_style")]
    pub(crate) track_name_style: TrackNameStyle,
    /// 右键菜单「设为默认字幕」的归类键：该归类键的轨道在 mks/mkv 里标 default。
    #[serde(default)]
    pub(crate) default_track_key: Option<String>,
    /// 内置字幕组表；`gui.toml` 的 `[[fansub_groups]]` 会整体替换它。
    #[serde(default = "default_fansub_groups_cfg")]
    pub(crate) fansub_groups: Vec<FansubGroup>,
    /// 内置后缀分类表（正则 + 语言后缀）；`[[suffix_categories]]` 可整体替换。
    #[serde(default = "default_suffix_categories_cfg")]
    pub(crate) suffix_categories: Vec<SuffixCategory>,
    #[serde(default)]
    pub(crate) muxer_path: String,
    #[serde(default)]
    pub(crate) ffprobe_path: String,
}

/// 便携式：配置与程序同目录（exe 旁的 gui.toml），不依赖启动时工作目录。
pub(crate) fn settings_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("gui.toml")
}

/// 默认视频扩展名的配置写法（`gui.toml` 里直接写成具体列表）。
pub(crate) fn default_video_exts() -> String {
    DEFAULT_VIDEO_EXTENSIONS.join(", ")
}

/// 默认字幕扩展名的配置写法。
pub(crate) fn default_sub_exts() -> String {
    DEFAULT_SUBTITLE_EXTENSIONS.join(", ")
}

pub(crate) fn default_fansub_groups_cfg() -> Vec<FansubGroup> {
    default_fansub_groups()
}

pub(crate) fn default_suffix_categories_cfg() -> Vec<SuffixCategory> {
    default_suffix_categories()
}

pub(crate) fn default_track_name_style() -> TrackNameStyle {
    TrackNameStyle::English
}

/// 侧边栏默认固定展开：自动收起是新行为，默认保持原来的常驻外观。
pub(crate) fn default_true() -> bool {
    true
}

pub(crate) fn load_settings() -> GuiSettings {
    let mut settings = std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_else(|| GuiSettings {
            video_exts: default_video_exts(),
            sub_exts: default_sub_exts(),
            sidebar_pinned: true,
            ..GuiSettings::default()
        });
    // 空扩展名列表回退到内置列表：配置文件里看到的就是实际生效的默认值。
    if settings.video_exts.trim().is_empty() {
        settings.video_exts = default_video_exts();
    }
    if settings.sub_exts.trim().is_empty() {
        settings.sub_exts = default_sub_exts();
    }
    if settings.fansub_groups.is_empty() {
        settings.fansub_groups = default_fansub_groups_cfg();
    }
    if settings.suffix_categories.is_empty() {
        settings.suffix_categories = default_suffix_categories_cfg();
    }
    settings
}
