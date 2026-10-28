//! assfonts-gui: gpui-kit based graphical front end for the assfonts library.
//!
//! Layout follows the FluxDown reference: collapsible sidebar navigation plus
//! grouped setting cards. The 处理 (process) page runs the library pipeline on
//! a background thread with log streaming; 设置 (settings) toggles feed the
//! `RunOptions` construction; 字体 (fonts) rebuilds `fonts.db`; 重命名 (rename)
//! pairs dropped videos with subtitles and aligns subtitle file names to the
//! video names (SubRenamer-style, see `assfonts::mux::job`/`scan`).

// The GUI ships as a windowed application; no console host is needed.
#![windows_subsystem = "windows"]

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use assfonts::{
    cli::{BuildOptions, MuxerChoice, OutputFormat, RunOptions},
    commands::ProgressReporter,
    mux::{
        self,
        job::{
            FansubGroup, RenameEntry, RenameNaming, RenameOptions, RenameRow, RenameStatus,
            RenameSummary, SuffixCategory, SuffixOverride, SuffixRule, TrackNameStyle,
            build_rename_rows, default_fansub_groups, default_suffix_categories,
        },
        scan::{
            DEFAULT_SUBTITLE_EXTENSIONS, DEFAULT_VIDEO_EXTENSIONS, ExtConfig, ScanResult,
            scan_paths,
        },
    },
};
use gpui_kit::{
    ExternalPaths,
    assets::{AllAssets, IconName},
    component::{
        ActiveTheme, TitleBar,
        button::{Button, ButtonVariants},
        input::{Input, InputEvent, InputState},
        menu::{ContextMenuExt, PopupMenu, PopupMenuItem},
        scroll::{Scrollbar, ScrollbarMode},
        switch::Switch,
        theme::{Theme, ThemeMode},
        *,
    },
    prelude::FluentBuilder as _,
    *,
};

/// Persisted GUI state, stored as TOML under `<config_dir>/assfonts/gui.toml`.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct GuiSettings {
    theme_choice: usize,
    sidebar_collapsed: bool,
    format_index: usize,
    muxer_index: usize,
    strict_mode: bool,
    allow_missing_sample: bool,
    allow_missing_fonts: bool,
    rename_fonts: bool,
    keep_temp: bool,
    keep_subs: bool,
    keep_attachments: bool,
    utf8_sig: bool,
    char_compatible: bool,
    use_embedded_fonts: bool,
    #[serde(default)]
    fontpaths: String,
    #[serde(default)]
    dbpath: String,
    #[serde(default)]
    output: String,
    #[serde(default = "default_video_exts")]
    video_exts: String,
    #[serde(default = "default_sub_exts")]
    sub_exts: String,
    /// Keep renamed subtitles in their own folder instead of moving them next
    /// to the paired video (the default is to move them).
    #[serde(default)]
    rename_keep_in_place: bool,
    /// Copy the subtitle to its new name instead of renaming it in place.
    #[serde(default)]
    rename_copy_mode: bool,
    /// 在最终文件名里插入字幕组缩写（`.KTXP`）。
    #[serde(default)]
    fansub_suffix: bool,
    /// 在最终文件名里插入语言后缀（`.SC`）。
    #[serde(default)]
    language_suffix: bool,
    /// mks/mkv 字幕轨命名方式：英文（KTXP.TC）或中文（极影字幕社.繁体）。
    #[serde(default = "default_track_name_style")]
    track_name_style: TrackNameStyle,
    /// 右键菜单「设为默认字幕」的归类键：该归类键的轨道在 mks/mkv 里标 default。
    #[serde(default)]
    default_track_key: Option<String>,
    /// 内置字幕组表；`gui.toml` 的 `[[fansub_groups]]` 会整体替换它。
    #[serde(default = "default_fansub_groups_cfg")]
    fansub_groups: Vec<FansubGroup>,
    /// 内置后缀分类表（正则 + 语言后缀）；`[[suffix_categories]]` 可整体替换。
    #[serde(default = "default_suffix_categories_cfg")]
    suffix_categories: Vec<SuffixCategory>,
    #[serde(default)]
    muxer_path: String,
    #[serde(default)]
    ffprobe_path: String,
}

/// 便携式：配置与程序同目录（exe 旁的 gui.toml），不依赖启动时工作目录。
fn settings_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("gui.toml")
}

/// 默认视频扩展名的配置写法（`gui.toml` 里直接写成具体列表）。
fn default_video_exts() -> String {
    DEFAULT_VIDEO_EXTENSIONS.join(", ")
}

/// 默认字幕扩展名的配置写法。
fn default_sub_exts() -> String {
    DEFAULT_SUBTITLE_EXTENSIONS.join(", ")
}

fn default_fansub_groups_cfg() -> Vec<FansubGroup> {
    default_fansub_groups()
}

fn default_suffix_categories_cfg() -> Vec<SuffixCategory> {
    default_suffix_categories()
}

fn default_track_name_style() -> TrackNameStyle {
    TrackNameStyle::English
}

fn load_settings() -> GuiSettings {
    let mut settings = std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_else(|| GuiSettings {
            video_exts: default_video_exts(),
            sub_exts: default_sub_exts(),
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

/// Rename table rows rendered at once; large folders stay responsive because
/// the page scroll is capped instead of building thousands of elements.
const RENAME_ROWS_SHOWN: usize = 600;
/// 重命名表格三列的初始像素宽度；表头右缘可拖拽调整，不持久化。
const RENAME_COL_DEFAULTS: [f32; 3] = [200., 230., 270.];
const RENAME_COL_MIN: f32 = 90.;
/// 处理页表格列宽下限（视频列 / 产物列都可以拖宽）。
const PROCESS_COL_MIN: f32 = 80.;
/// 处理页表格列宽上限，避免拖到把字幕列挤没。
const PROCESS_COL_MAX: f32 = 620.;
const MUXERS: [&str; 2] = ["mkvmerge", "ffmpeg"];
const FORMATS: [&str; 3] = ["ass/ssa 内嵌字体", "mks 独立字幕", "mkv 封装视频"];
const THEME_CHOICES: [&str; 3] = ["自动", "亮色", "暗色"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Process,
    Fonts,
    Rename,
    Logs,
    Settings,
}

struct AssfontsApp {
    page: Page,
    sidebar_collapsed: bool,
    /// 0 = auto (system appearance), 1 = light, 2 = dark.
    theme_choice: usize,

    fontpaths_input: Entity<InputState>,
    dbpath_input: Entity<InputState>,
    output_input: Entity<InputState>,
    format_index: usize,

    strict_mode: bool,
    allow_missing_sample: bool,
    allow_missing_fonts: bool,
    rename_fonts: bool,
    keep_temp: bool,
    keep_subs: bool,
    keep_attachments: bool,
    utf8_sig: bool,
    char_compatible: bool,
    use_embedded_fonts: bool,
    muxer_index: usize,
    muxer_path_input: Entity<InputState>,
    ffprobe_path_input: Entity<InputState>,

    running: bool,
    logs: Vec<String>,
    /// 最近一次「保存日志」写出的文件路径（显示在日志页）。
    last_log_saved: Option<String>,
    /// Set by the 取消 button; checked by the library between units of work.
    cancel_flag: Arc<AtomicBool>,

    /// 视频/字幕扩展名只存在于 `gui.toml`（设置页不再提供输入框）。
    video_exts: String,
    sub_exts: String,
    /// 行内“自定义后缀”输入框：同一时间只编辑一行，复用一个输入。
    custom_suffix_input: Entity<InputState>,
    /// 正在编辑自定义后缀的那一行：`(0=字幕组 / 1=语言, 归类键)`。
    rename_edit_key: Option<(u8, String)>,
    rename_keep_in_place: bool,
    rename_copy_mode: bool,
    /// 最终文件名里是否插入字幕组缩写 / 语言后缀。
    fansub_suffix: bool,
    language_suffix: bool,
    /// 生效中的字幕组表与后缀分类表（来自 `gui.toml`）。
    fansub_groups: Vec<FansubGroup>,
    suffix_categories: Vec<SuffixCategory>,
    /// 右键菜单设置的覆盖项：归类键 → 字幕组/语言后缀。
    rename_overrides: std::collections::BTreeMap<String, SuffixOverride>,
    /// 处理页拖入的文件/文件夹（扫描源）。
    process_roots: Vec<PathBuf>,
    /// 处理页表格行：扫描出的字幕 + 配对视频 + 归类结果。
    process_rows: Vec<ProcessRow>,
    /// mks/mkv 轨道命名方式（设置页切换）。
    track_name_style: TrackNameStyle,
    /// 「设为默认字幕」的归类键（同一归类键在其它视频上同样生效）。
    default_track_key: Option<String>,
    /// Files and folders dropped on the 重命名 page; the scan source.
    rename_roots: Vec<PathBuf>,
    /// Latest scan of `rename_roots`, split into videos and subtitles.
    rename_scan: Option<ScanResult>,
    /// Rename table built from the scan plus the current rule/settings.
    rename_rows: Vec<RenameRow>,
    /// 重命名表格的三列宽度（像素），表头右缘的把手可以拖动调整。
    rename_cols: [f32; 3],
    /// 最后一列默认吃掉剩余宽度；拖过它的把手后改为固定宽度。
    rename_col_last_fixed: bool,
    /// 是否已手动改过列宽：没改过时列宽按窗口宽度自适应。
    rename_col_custom: bool,
    /// 正在拖动的列：(列下标, 按下时的窗口 x, 按下时的列宽)。
    rename_col_drag: Option<(usize, f32, f32)>,
    /// 处理页表格列宽：视频列、产物列（字幕列吃剩余宽度）。
    process_video_col: f32,
    process_product_col: f32,
    /// 正在拖的列：0 = 视频列，1 = 产物列；附带按下位置与起始宽度。
    process_col_drag: Option<(u8, f32, f32)>,
    /// 表格的双向滚动位置；垂直/水平滚动条叠加在表格边缘。
    rename_scroll_x: ScrollHandle,
    rename_scroll_y: ScrollHandle,
    /// 日志页的纵向滚动位置；滚动条常驻日志框右缘。
    log_scroll_y: ScrollHandle,
    /// 保持重命名页观察者的订阅存活（丢弃 Subscription 会取消订阅）。
    #[allow(dead_code)] // 只用于持有订阅，代码里不会读取
    rename_input_subs: Vec<Subscription>,
    /// External tool availability shown in the title bar.
    tools_found: (bool, bool),
}

/// Forwards library progress lines into the GUI log channel and exposes
/// the cancel flag to the library's checkpoints.
struct ChannelReporter {
    tx: smol::channel::Sender<String>,
    cancelled: Arc<AtomicBool>,
}

impl ProgressReporter for ChannelReporter {
    fn report(&self, message: String) {
        let _ = self.tx.send_blocking(message);
    }

    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

/// How a drop target adopts dropped file/folder paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DropMode {
    /// Append every dropped path to a `;`-separated multi-value input.
    Append,
    /// Replace the input with the first dropped path (single-value inputs).
    Replace,
}

/// Split a free-form extension list (`mkv, mp4` / `mkv;mp4` / `.mkv`) into
/// individual entries; blanks are dropped and `.` prefixes are tolerated by
/// `ExtConfig::new`.
fn split_exts(value: &str) -> Vec<String> {
    value
        .split([';', '；', ',', '，', ' ', '\t', '\n'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// Lossy file name for display; never empty.
fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// 处理页表格的一行：一条字幕（可能配有视频）+ 生效的字幕组/语言后缀。
#[derive(Debug, Clone)]
struct ProcessRow {
    video: Option<PathBuf>,
    sub: PathBuf,
    /// 归类键（分类名小写，未命中则注释小写）。
    key: String,
    /// 生效的字幕组缩写/自定义文本（决定文件名后缀）。
    fansub: Option<String>,
    /// 绿色标签里显示的字幕组中文名/自定义文本。
    fansub_name: Option<String>,
    /// 生效的语言后缀（SC/TCJP/自定义文本）。
    language: Option<String>,
    /// 绿色标签里显示的语言分类中文名/自定义文本。
    language_name: Option<String>,
}

impl ProcessRow {
    /// embed 模式只能把字体写进 ASS，其他字幕格式不支持。
    fn supports_embed(&self) -> bool {
        matches!(
            self.sub
                .extension()
                .and_then(|ext| ext.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("ass" | "ssa")
        )
    }
}

fn format_output_format(index: usize) -> OutputFormat {
    match index % FORMATS.len() {
        1 => OutputFormat::Mks,
        2 => OutputFormat::Mkv,
        _ => OutputFormat::Embed,
    }
}

fn split_paths(value: &str) -> Vec<PathBuf> {
    value
        .split([';', '；'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Resolve the auto theme choice from the OS appearance.
fn resolve_theme_mode(choice: usize, cx: &App) -> ThemeMode {
    match choice % THEME_CHOICES.len() {
        1 => ThemeMode::Light,
        2 => ThemeMode::Dark,
        _ => match cx.window_appearance() {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeMode::Dark,
            _ => ThemeMode::Light,
        },
    }
}

/// 字幕单元格的右键菜单：字幕组后缀 / 语言后缀 / 设为默认字幕。
///
/// 覆盖项按归类键生效，所以给某条字幕设置后，其它视频上的同类字幕也会
/// 自动跟着变。
#[allow(clippy::too_many_arguments)]
fn subtitle_context_menu(
    mut menu: PopupMenu,
    window: &mut Window,
    context: &mut Context<PopupMenu>,
    entity: Entity<AssfontsApp>,
    key: String,
    current_fansub: Option<String>,
    current_language: Option<String>,
    groups: Vec<FansubGroup>,
    categories: Vec<SuffixCategory>,
    default_key: Option<String>,
) -> PopupMenu {
    let fansub_entity = entity.clone();
    let fansub_key = key.clone();
    let current = current_fansub;
    menu = menu.submenu(
        "字幕组后缀",
        window,
        context,
        move |mut sub, _window, _cx| {
            for group in &groups {
                let entity = fansub_entity.clone();
                let item_key = fansub_key.clone();
                let checked = current.as_deref() == Some(group.abbr.as_str());
                let value = group.abbr.clone();
                sub = sub.item(
                    PopupMenuItem::new(format!("{}（{}）", group.name, group.abbr))
                        .checked(checked)
                        .on_click(move |_, _, cx| {
                            entity.update(cx, |this, cx| {
                                this.apply_override(0, item_key.clone(), value.clone(), cx);
                            });
                        }),
                );
            }
            let entity = fansub_entity.clone();
            let clear_key = fansub_key.clone();
            let clear_checked = current.is_none();
            sub = sub.item(
                PopupMenuItem::new("不使用字幕组后缀")
                    .checked(clear_checked)
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |this, cx| {
                            this.apply_override(0, clear_key.clone(), String::new(), cx);
                        });
                    }),
            );
            let entity = fansub_entity.clone();
            let custom_key = fansub_key.clone();
            sub.item(
                PopupMenuItem::new("自定义…").on_click(move |_, window, cx| {
                    entity.update(cx, |this, cx| {
                        this.begin_custom_edit(0, custom_key.clone(), window, cx);
                    });
                }),
            )
        },
    );

    let language_entity = entity.clone();
    let language_key = key.clone();
    let current = current_language;
    let categories_for_menu = categories.clone();
    menu = menu.submenu(
        "语言后缀",
        window,
        context,
        move |mut sub, _window, _cx| {
            for category in &categories_for_menu {
                let entity = language_entity.clone();
                let item_key = language_key.clone();
                let checked = current.as_deref() == Some(category.suffix.as_str());
                let value = category.suffix.clone();
                sub = sub.item(
                    PopupMenuItem::new(format!("{}（{}）", category.name, category.suffix))
                        .checked(checked)
                        .on_click(move |_, _, cx| {
                            entity.update(cx, |this, cx| {
                                this.apply_override(1, item_key.clone(), value.clone(), cx);
                            });
                        }),
                );
            }
            let entity = language_entity.clone();
            let clear_key = language_key.clone();
            let clear_checked = current.is_none();
            sub = sub.item(
                PopupMenuItem::new("不使用语言后缀")
                    .checked(clear_checked)
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |this, cx| {
                            this.apply_override(1, clear_key.clone(), String::new(), cx);
                        });
                    }),
            );
            let entity = language_entity.clone();
            let custom_key = language_key.clone();
            sub.item(
                PopupMenuItem::new("自定义…").on_click(move |_, window, cx| {
                    entity.update(cx, |this, cx| {
                        this.begin_custom_edit(1, custom_key.clone(), window, cx);
                    });
                }),
            )
        },
    );

    let default_checked = default_key.as_deref() == Some(key.as_str());
    let default_entity = entity;
    menu.item(
        PopupMenuItem::new("设为默认字幕")
            .checked(default_checked)
            .on_click(move |_, _, cx| {
                default_entity.update(cx, |this, cx| {
                    this.set_default_track(key.clone(), cx);
                });
            }),
    )
}

impl AssfontsApp {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = load_settings();
        let mut make_input = |placeholder: &'static str| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let apply_input = |state: &Entity<InputState>,
                           value: &str,
                           window: &mut Window,
                           cx: &mut Context<AssfontsApp>| {
            if !value.is_empty() {
                state.update(cx, |input, cx| {
                    input.replace_all(value.to_string(), window, cx)
                });
            }
        };

        let fontpaths_input = make_input("字体目录，留空则只用 fonts.db");
        let dbpath_input = make_input("fonts.db 所在目录，默认 .");
        let output_input = make_input("输出目录，默认 out");
        let custom_suffix_input = make_input("自定义后缀：输入什么就用什么，回车确认");
        let muxer_path_input = make_input("封装器可执行文件路径，留空用 PATH");
        let ffprobe_path_input = make_input("ffprobe 路径（仅 ffmpeg 需要），留空用 PATH");

        apply_input(&fontpaths_input, &settings.fontpaths, window, cx);
        apply_input(&dbpath_input, &settings.dbpath, window, cx);
        apply_input(&output_input, &settings.output, window, cx);
        apply_input(&muxer_path_input, &settings.muxer_path, window, cx);
        apply_input(&ffprobe_path_input, &settings.ffprobe_path, window, cx);

        // 行内自定义后缀：回车提交（空 = 清除覆盖，回到自动识别）。
        let mut rename_input_subs = Vec::new();
        rename_input_subs.push(cx.subscribe(
            &custom_suffix_input,
            |this: &mut Self, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.commit_custom_edit(cx);
                }
            },
        ));

        let app = Self {
            page: Page::Process,
            sidebar_collapsed: settings.sidebar_collapsed,
            theme_choice: settings.theme_choice,
            fontpaths_input,
            dbpath_input,
            output_input,
            format_index: settings.format_index,
            strict_mode: settings.strict_mode,
            allow_missing_sample: settings.allow_missing_sample,
            allow_missing_fonts: settings.allow_missing_fonts,
            rename_fonts: settings.rename_fonts,
            keep_temp: settings.keep_temp,
            keep_subs: settings.keep_subs,
            keep_attachments: settings.keep_attachments,
            utf8_sig: settings.utf8_sig,
            char_compatible: settings.char_compatible,
            use_embedded_fonts: settings.use_embedded_fonts,
            muxer_index: settings.muxer_index,
            muxer_path_input,
            ffprobe_path_input,
            running: false,
            logs: Vec::new(),
            last_log_saved: None,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            video_exts: settings.video_exts.clone(),
            sub_exts: settings.sub_exts.clone(),
            custom_suffix_input,
            rename_edit_key: None,
            rename_keep_in_place: settings.rename_keep_in_place,
            rename_copy_mode: settings.rename_copy_mode,
            fansub_suffix: settings.fansub_suffix,
            language_suffix: settings.language_suffix,
            fansub_groups: settings.fansub_groups.clone(),
            suffix_categories: settings.suffix_categories.clone(),
            rename_overrides: std::collections::BTreeMap::new(),
            process_roots: Vec::new(),
            process_rows: Vec::new(),
            track_name_style: settings.track_name_style,
            default_track_key: settings.default_track_key.clone(),
            rename_roots: Vec::new(),
            rename_scan: None,
            rename_rows: Vec::new(),
            rename_cols: RENAME_COL_DEFAULTS,
            rename_col_last_fixed: false,
            rename_col_custom: false,
            rename_col_drag: None,
            process_video_col: 230.,
            process_product_col: 200.,
            process_col_drag: None,
            rename_scroll_x: ScrollHandle::default(),
            rename_scroll_y: ScrollHandle::default(),
            log_scroll_y: ScrollHandle::default(),
            rename_input_subs,
            // PATH lookup only — no subprocess (a GUI parent would flash consoles).
            tools_found: (
                mux::find_on_path("mkvmerge").is_some(),
                mux::find_on_path("ffmpeg").is_some(),
            ),
        };

        // Restore the persisted theme choice.
        Theme::change(resolve_theme_mode(app.theme_choice, cx), None, cx);

        app
    }

    /// Serialize the current UI state to `gui.toml`.
    fn persist(&self, cx: &App) {
        let settings = GuiSettings {
            theme_choice: self.theme_choice,
            sidebar_collapsed: self.sidebar_collapsed,
            format_index: self.format_index,
            muxer_index: self.muxer_index,
            strict_mode: self.strict_mode,
            allow_missing_sample: self.allow_missing_sample,
            allow_missing_fonts: self.allow_missing_fonts,
            rename_fonts: self.rename_fonts,
            keep_temp: self.keep_temp,
            keep_subs: self.keep_subs,
            keep_attachments: self.keep_attachments,
            utf8_sig: self.utf8_sig,
            char_compatible: self.char_compatible,
            use_embedded_fonts: self.use_embedded_fonts,
            fontpaths: self.input_value(&self.fontpaths_input, cx),
            dbpath: self.input_value(&self.dbpath_input, cx),
            output: self.input_value(&self.output_input, cx),
            // 配置里写实际生效的默认值（而不是空字符串），打开 `gui.toml` 就能
            // 看到默认的视频/字幕扩展名与后缀规则。
            video_exts: {
                if self.video_exts.trim().is_empty() {
                    default_video_exts()
                } else {
                    self.video_exts.clone()
                }
            },
            sub_exts: {
                if self.sub_exts.trim().is_empty() {
                    default_sub_exts()
                } else {
                    self.sub_exts.clone()
                }
            },
            rename_keep_in_place: self.rename_keep_in_place,
            rename_copy_mode: self.rename_copy_mode,
            fansub_suffix: self.fansub_suffix,
            language_suffix: self.language_suffix,
            fansub_groups: self.fansub_groups.clone(),
            suffix_categories: self.suffix_categories.clone(),
            track_name_style: self.track_name_style,
            default_track_key: self.default_track_key.clone(),
            muxer_path: self.input_value(&self.muxer_path_input, cx),
            ffprobe_path: self.input_value(&self.ffprobe_path_input, cx),
        };
        let Ok(text) = toml::to_string_pretty(&settings) else {
            return;
        };
        let path = settings_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, text);
    }

    fn push_log(&mut self, message: String) {
        self.logs.push(message);
        let excess = self.logs.len().saturating_sub(200);
        self.logs.drain(..excess);
        // 新日志总是追加在底部：请求下一次布局把视图滚到末尾。
        self.log_scroll_y.scroll_to_bottom();
    }

    fn input_value(&self, state: &Entity<InputState>, cx: &App) -> String {
        state.read(cx).value().to_string()
    }

    fn collect_options(&self, cx: &App) -> Result<RunOptions, String> {
        let format = format_output_format(self.format_index);
        if self.process_rows.is_empty() {
            return Err("请把字幕文件（和 mkv 模式下的媒体文件）拖进表格".to_string());
        }
        // embed 只能写 ASS/SSA；其它格式在表格里已标为不可用，这里再兜底过滤。
        let inputs: Vec<PathBuf> = self
            .process_rows
            .iter()
            .filter(|row| format != OutputFormat::Embed || row.supports_embed())
            .map(|row| row.sub.clone())
            .collect();
        if inputs.is_empty() {
            return Err(
                "内嵌模式只支持 .ass/.ssa 字幕；其它格式请改用“生成 mks 字幕”或“封装到 mkv”"
                    .to_string(),
            );
        }
        let videos: Vec<PathBuf> = self
            .process_rows
            .iter()
            .filter_map(|row| row.video.clone())
            .collect();
        let videos = if videos.is_empty() {
            None
        } else {
            Some(videos)
        };

        let fontpaths = split_paths(&self.input_value(&self.fontpaths_input, cx));
        let fontpaths = if fontpaths.is_empty() {
            None
        } else {
            Some(fontpaths)
        };

        let dbpath_value = self.input_value(&self.dbpath_input, cx);
        let dbpath = if dbpath_value.trim().is_empty() {
            PathBuf::from(".")
        } else {
            PathBuf::from(dbpath_value.trim())
        };

        let output_value = self.input_value(&self.output_input, cx);
        let output = if output_value.trim().is_empty() {
            PathBuf::from("out")
        } else {
            PathBuf::from(output_value.trim())
        };

        let format = format_output_format(self.format_index);

        Ok(RunOptions {
            inputs,
            output,
            fontpaths,
            dbpath,
            strict: self.strict_mode,
            allow_missing_sample: self.allow_missing_sample,
            allow_error_fonts: self.allow_missing_fonts,
            report: false,
            force: false,
            format,
            videos,
            muxer: self.muxer_choice(),
            language: None,
            default_track: None,
            default_track_keys: self.default_track_key.iter().cloned().collect(),
            track_name_style: self.track_name_style,
            track_fansub_suffix: self.fansub_suffix,
            track_language_suffix: self.language_suffix,
            track_categories: self.suffix_categories.clone(),
            track_groups: self.fansub_groups.clone(),
            track_overrides: self.rename_overrides.clone(),
            title: None,
            keep_subs: self.keep_subs,
            keep_attachments: self.keep_attachments,
            rename_fonts: self.rename_fonts,
            keep_temp: self.keep_temp,
            utf8_sig: self.utf8_sig,
            char_compatible: self.char_compatible,
            use_embedded_fonts: self.use_embedded_fonts,
            muxer_path: {
                let value = self.input_value(&self.muxer_path_input, cx);
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(trimmed))
                }
            },
            ffprobe_path: {
                let value = self.input_value(&self.ffprobe_path_input, cx);
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(trimmed))
                }
            },
        })
    }

    fn muxer_choice(&self) -> MuxerChoice {
        if self.muxer_index % MUXERS.len() == 1 {
            MuxerChoice::Ffmpeg
        } else {
            MuxerChoice::Mkvmerge
        }
    }

    /// Run one task on a background thread, streaming status lines into the
    /// shared log panel.
    /// 取消当前任务：库在各工作单元之间检查该标志。
    fn request_cancel(&mut self, cx: &mut Context<Self>) {
        self.cancel_flag.store(true, Ordering::Relaxed);
        self.push_log("[取消] 已请求取消，等待当前单元结束…".to_string());
        cx.notify();
    }

    fn spawn_task(
        &mut self,
        weak: WeakEntity<Self>,
        cx: &mut App,
        what: String,
        task: impl FnOnce(&dyn ProgressReporter) -> assfonts::error::Result<()> + Send + 'static,
    ) -> Option<String> {
        self.logs.clear();
        self.running = true;
        self.cancel_flag = Arc::new(AtomicBool::new(false));

        let (tx, rx) = smol::channel::unbounded::<String>();
        let reporter = ChannelReporter {
            tx: tx.clone(),
            cancelled: Arc::clone(&self.cancel_flag),
        };
        std::thread::spawn(move || {
            let _ = tx.send_blocking(format!("开始{what}…"));
            match task(&reporter) {
                Ok(()) => {
                    let _ = tx.send_blocking(format!("{what}完成。"));
                }
                Err(err) if err.to_string().contains("cancelled") => {
                    let _ = tx.send_blocking(format!("[取消] {err}"));
                    let _ = tx.send_blocking(format!("{what}已取消。"));
                }
                Err(err) => {
                    let _ = tx.send_blocking(format!("[错误] {err}"));
                    let _ = tx.send_blocking(format!("{what}失败。"));
                }
            }
        });

        cx.spawn(async move |cx| {
            while let Ok(message) = rx.recv().await {
                let suffix_done = message.ends_with("完成。");
                let suffix_failed = message.ends_with("失败。");
                let suffix_cancelled = message.ends_with("已取消。");
                let _ = weak.update(cx, |this, cx| {
                    this.push_log(message);
                    if suffix_done || suffix_failed || suffix_cancelled {
                        this.running = false;
                    }
                    cx.notify();
                });
                if suffix_done || suffix_failed || suffix_cancelled {
                    break;
                }
            }
            let _ = weak.update(cx, |this, cx| {
                this.running = false;
                cx.notify();
            });
        })
        .detach();

        None
    }

    /// 日志页的一行：缺少字体/读取失败整行标红；字体覆盖行只把缺字列表标红。
    fn log_line(cx: &App, line: &str, missing_chars: Option<std::ops::Range<usize>>) -> Div {
        let trimmed = line.trim_start();
        let danger = cx.theme().danger;
        let text = div().w_full().text_size(px(12.));
        if trimmed.starts_with('✗')
            || trimmed.starts_with("[错误]")
            || (trimmed.starts_with("有 ") && trimmed.contains("读取失败"))
        {
            return text.text_color(danger).child(line.to_string());
        }
        match missing_chars {
            Some(range) => text.child(StyledText::new(line.to_string()).with_highlights([(
                range,
                HighlightStyle {
                    color: Some(danger),
                    ..Default::default()
                },
            )])),
            None => text.child(line.to_string()),
        }
    }

    /// 把文件管理器拖入的文件/文件夹（可多选）写入目标输入框。
    fn adopt_dropped_paths(
        &mut self,
        state: &Entity<InputState>,
        mode: DropMode,
        files: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if files.is_empty() {
            return;
        }
        let paths: Vec<String> = files
            .iter()
            .map(|path| path.display().to_string())
            .collect();

        match mode {
            DropMode::Append => {
                let current = self.input_value(state, cx);
                let addition = paths.join("; ");
                let joined = if current.trim().is_empty() {
                    addition
                } else {
                    format!("{current}; {addition}")
                };
                state.update(cx, |input, cx| input.replace_all(joined, window, cx));
            }
            DropMode::Replace => {
                let first = paths.first().cloned().unwrap_or_default();
                state.update(cx, |input, cx| input.replace_all(first, window, cx));
            }
        }
        cx.notify();
    }

    /// Extension lists from `gui.toml` (empty lists fall back to the library
    /// defaults, so a broken config cannot disable recognition).
    fn ext_config(&self) -> ExtConfig {
        ExtConfig::new(split_exts(&self.video_exts), split_exts(&self.sub_exts))
    }

    fn rename_options(&self) -> RenameOptions {
        RenameOptions {
            move_to_video_dir: !self.rename_keep_in_place,
            fansub_suffix: self.fansub_suffix,
            language_suffix: self.language_suffix,
        }
    }

    /// 右键“自定义…”：把该行切换成行内输入框，预填当前生效的后缀。
    fn begin_custom_edit(
        &mut self,
        kind: u8,
        key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self
            .rename_rows
            .iter()
            .flat_map(|row| row.entries.iter())
            .find(|entry| entry.key == key)
            .and_then(|entry| {
                if kind == 0 {
                    entry.fansub.clone()
                } else {
                    entry.language.clone()
                }
            })
            .unwrap_or_default();
        let input = self.custom_suffix_input.clone();
        input.update(cx, |input, cx| input.replace_all(&current, window, cx));
        input.update(cx, |input, cx| input.focus(window, cx));
        self.rename_edit_key = Some((kind, key));
        cx.notify();
    }

    /// 行内输入框回车：把输入文本写成该类后缀（空 = 清除覆盖）。
    fn commit_custom_edit(&mut self, cx: &mut Context<Self>) {
        let Some((kind, key)) = self.rename_edit_key.take() else {
            return;
        };
        let value = self.input_value(&self.custom_suffix_input, cx);
        self.apply_override(kind, key, value, cx);
    }

    /// 右键菜单设置/清除某个归类键的字幕组或语言后缀，然后重建表格。
    /// 覆盖项按归类键生效，所以同类的其他字幕会自动跟着变。
    fn apply_override(&mut self, kind: u8, key: String, value: String, cx: &mut Context<Self>) {
        if key.is_empty() {
            return;
        }
        let value = (!value.trim().is_empty()).then(|| value.trim().to_string());
        let entry = self.rename_overrides.entry(key).or_default();
        match kind {
            0 => entry.fansub = value,
            _ => entry.language = value,
        }
        if entry.fansub.is_none() && entry.language.is_none() {
            self.rename_overrides
                .retain(|_, item| item.fansub.is_some() || item.language.is_some());
        }
        self.rename_edit_key = None;
        self.refresh_rename(cx);
        self.refresh_process(cx);
        cx.notify();
    }

    /// 重扫拖入重命名页的文件夹/文件并重建表格：扫描结果、目录选项和右键
    /// 覆盖项一起决定每一行的最终文件名。
    fn refresh_rename(&mut self, cx: &mut Context<Self>) {
        if self.rename_roots.is_empty() {
            self.rename_scan = None;
            self.rename_rows.clear();
            cx.notify();
            return;
        }
        let config = self.ext_config();
        let scan = scan_paths(&self.rename_roots, &config, true);
        let rule = SuffixRule::Keep;
        let overrides = self.rename_overrides.clone();
        let naming = RenameNaming {
            options: self.rename_options(),
            rule: &rule,
            categories: &self.suffix_categories,
            groups: &self.fansub_groups,
            overrides: &overrides,
        };
        let rows = build_rename_rows(&scan.videos, &scan.subtitles, &naming);
        self.rename_rows = rows;
        let counts = (scan.videos.len(), scan.subtitles.len(), scan.ignored);
        let changed = self
            .rename_scan
            .as_ref()
            .map(|old| (old.videos.len(), old.subtitles.len(), old.ignored) != counts)
            .unwrap_or(true);
        if changed {
            let ignored = if scan.ignored > 0 {
                format!("，忽略 {} 个其他文件", scan.ignored)
            } else {
                String::new()
            };
            self.push_log(format!(
                "[重命名] 识别 {} 个视频、{} 个字幕{ignored}",
                counts.0, counts.1
            ));
        }
        self.rename_scan = Some(scan);
        self.persist(cx);
        cx.notify();
    }

    /// 重命名页的拖放入口：路径直接成为扫描根，扩展名决定视频/字幕归属，
    /// 目录递归展开（文件管理器拖入文件或文件夹都可以）。
    fn drop_into_rename(&mut self, files: &[PathBuf], cx: &mut Context<Self>) {
        if files.is_empty() {
            return;
        }
        let mut added = 0usize;
        for path in files {
            if !self.rename_roots.contains(path) {
                self.rename_roots.push(path.clone());
                added += 1;
            }
        }
        if added == 0 {
            self.push_log("[重命名] 拖入的路径已在列表中".to_string());
        }
        self.refresh_rename(cx);
        self.persist(cx);
    }

    fn clear_rename(&mut self, cx: &mut Context<Self>) {
        self.rename_roots.clear();
        self.rename_scan = None;
        self.rename_rows.clear();
        self.rename_edit_key = None;
        self.push_log("[重命名] 已清空".to_string());
        self.persist(cx);
        cx.notify();
    }

    /// 当前重命名表格的统计。
    /// 处理页：记下拖入的文件/文件夹并重扫表格。
    fn drop_into_process(&mut self, files: &[PathBuf], cx: &mut Context<Self>) {
        let mut added = 0usize;
        for path in files {
            if !self.process_roots.contains(path) {
                self.process_roots.push(path.clone());
                added += 1;
            }
        }
        if added == 0 {
            self.push_log("[处理] 拖入的路径已在列表中".to_string());
        }
        self.refresh_process(cx);
        cx.notify();
    }

    fn clear_process(&mut self, cx: &mut Context<Self>) {
        self.process_roots.clear();
        self.process_rows.clear();
        self.push_log("[处理] 已清空".to_string());
        cx.notify();
    }

    /// 预测这一行在当前输出格式下生成的产物文件名（与 `commands.rs` 的命名规则一致）。
    fn product_name(&self, row: &ProcessRow) -> (String, bool) {
        let sub_stem = row
            .sub
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("output");
        match format_output_format(self.format_index) {
            OutputFormat::Embed => {
                if !row.supports_embed() {
                    return ("不支持内嵌".to_string(), true);
                }
                (display_name(&row.sub), false)
            }
            OutputFormat::Mks => {
                let stem = row
                    .video
                    .as_ref()
                    .and_then(|video| video.file_stem())
                    .and_then(|s| s.to_str())
                    .unwrap_or(sub_stem);
                (format!("{stem}.mks"), false)
            }
            OutputFormat::Mkv => match row.video.as_ref() {
                Some(video) => {
                    let stem = video
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("output");
                    (format!("{stem}.muxed.mkv"), false)
                }
                None => ("需要视频文件".to_string(), true),
            },
        }
    }

    /// 把日志写到程序目录下的 `assfonts-log-<UTC 时间>.log`。
    fn save_log(&mut self, cx: &mut Context<Self>) {
        if self.logs.is_empty() {
            self.push_log("[日志] 没有可保存的内容".to_string());
            cx.notify();
            return;
        }
        let dir = settings_path()
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let path = dir.join(format!("assfonts-log-{}.log", utc_timestamp()));
        match std::fs::write(
            &path,
            format!(
                "{}
",
                self.logs.join(
                    "
"
                )
            ),
        ) {
            Ok(()) => {
                let shown = path.display().to_string();
                self.last_log_saved = Some(shown.clone());
                self.push_log(format!("[日志] 已保存到 {shown}"));
            }
            Err(err) => self.push_log(format!("[日志] 保存失败：{err}")),
        }
        cx.notify();
    }

    /// 重扫处理页拖入的路径：视频/字幕分流、按文件名配对，并算好每行的
    /// 字幕组/语言后缀（右键覆盖项优先）。
    fn refresh_process(&mut self, cx: &mut Context<Self>) {
        if self.process_roots.is_empty() {
            self.process_rows.clear();
            cx.notify();
            return;
        }
        let config = self.ext_config();
        let scan = scan_paths(&self.process_roots, &config, true);
        let rule = SuffixRule::Keep;
        let overrides = self.rename_overrides.clone();
        let naming = RenameNaming {
            options: self.rename_options(),
            rule: &rule,
            categories: &self.suffix_categories,
            groups: &self.fansub_groups,
            overrides: &overrides,
        };
        let rows = build_rename_rows(&scan.videos, &scan.subtitles, &naming);
        let mut flat = Vec::new();
        for row in rows {
            for entry in row.entries {
                flat.push(ProcessRow {
                    video: row.video.clone(),
                    sub: entry.sub,
                    key: entry.key,
                    fansub: entry.fansub,
                    fansub_name: entry.fansub_name,
                    language: entry.language,
                    language_name: entry.language_name,
                });
            }
        }
        self.process_rows = flat;
        cx.notify();
    }

    /// 右键「设为默认字幕」：按归类键记录，其它视频的同类字幕自动生效；
    /// 再点一次同一个键就取消默认。
    fn set_default_track(&mut self, key: String, cx: &mut Context<Self>) {
        if key.is_empty() {
            return;
        }
        self.default_track_key = if self.default_track_key.as_deref() == Some(key.as_str()) {
            None
        } else {
            Some(key)
        };
        self.persist(cx);
        cx.notify();
    }

    fn rename_summary(&self) -> RenameSummary {
        RenameSummary::of(&self.rename_rows)
    }

    /// 表头把手按下：记录列号、按下位置和该列当前宽度。拖动最后一列后它从
    /// “吃掉剩余宽度”改为固定宽度，否则拖不动。
    fn start_column_drag(&mut self, column: usize, x: f32, columns: [f32; 3]) {
        if column < self.rename_cols.len() {
            // 自适应模式下 self.rename_cols 还是默认值，先对齐到屏幕上的实际宽度。
            self.rename_cols = columns;
            self.rename_col_custom = true;
            if column + 1 == self.rename_cols.len() {
                self.rename_col_last_fixed = true;
            }
            self.rename_col_drag = Some((column, x, self.rename_cols[column]));
        }
    }

    /// 拖动中：按光标位移更新列宽，返回是否真的改变了（用于决定是否重绘）。
    fn drag_column(&mut self, x: f32) -> bool {
        let Some((column, start_x, start_width)) = self.rename_col_drag else {
            return false;
        };
        // 只保留下限：列宽可以拖到任意宽（超出窗口就交给横向滚动条）。
        let width = (start_width + (x - start_x)).max(RENAME_COL_MIN);
        if (width - self.rename_cols[column]).abs() < 0.5 {
            return false;
        }
        self.rename_cols[column] = width;
        true
    }

    fn end_column_drag(&mut self) {
        self.rename_col_drag = None;
    }

    /// 处理页列宽把手按下：which = 0 视频列、1 产物列。
    fn start_process_col_drag(&mut self, which: u8, x: f32) {
        let start = if which == 0 {
            self.process_video_col
        } else {
            self.process_product_col
        };
        self.process_col_drag = Some((which, x, start));
    }

    /// 处理页列宽拖动：视频列在分隔线左侧（向右拖变宽），
    /// 产物列在分隔线右侧（向右拖变窄）；返回是否真的变了。
    fn drag_process_col(&mut self, x: f32) -> bool {
        let Some((which, start_x, start_width)) = self.process_col_drag else {
            return false;
        };
        let delta = x - start_x;
        let width = if which == 0 {
            start_width + delta
        } else {
            start_width - delta
        }
        .clamp(PROCESS_COL_MIN, PROCESS_COL_MAX);
        let target = if which == 0 {
            &mut self.process_video_col
        } else {
            &mut self.process_product_col
        };
        if (width - *target).abs() < 0.5 {
            return false;
        }
        *target = width;
        true
    }

    fn end_process_col_drag(&mut self) {
        self.process_col_drag = None;
    }

    /// 执行表格中所有“就绪”的行（按当前设置重命名或复制），随后重扫。
    fn apply_renames(&mut self, cx: &mut Context<Self>) {
        let rows = self.rename_rows.clone();
        let summary = RenameSummary::of(&rows);
        if !summary.has_ready() {
            self.push_log("[重命名] 没有可执行的行".to_string());
            cx.notify();
            return;
        }
        let copy_mode = self.rename_copy_mode;
        let verb = if copy_mode { "复制" } else { "重命名" };
        let mut done = 0usize;
        let mut failed = 0usize;
        for row in &rows {
            for entry in &row.entries {
                if entry.status != RenameStatus::Ready {
                    continue;
                }
                let result = if copy_mode {
                    std::fs::copy(&entry.sub, &entry.target).map(|_| ())
                } else {
                    std::fs::rename(&entry.sub, &entry.target)
                };
                match result {
                    Ok(()) => {
                        done += 1;
                        self.push_log(format!(
                            "[重命名] {verb}: {} → {}",
                            display_name(&entry.sub),
                            display_name(&entry.target)
                        ));
                    }
                    Err(err) => {
                        failed += 1;
                        self.push_log(format!(
                            "[重命名] {verb}失败 {}: {err}",
                            display_name(&entry.sub)
                        ));
                    }
                }
            }
        }
        self.push_log(format!("[重命名] 完成：成功 {done} 个，失败 {failed} 个"));
        self.refresh_rename(cx);
        self.persist(cx);
    }

    // -- rendering helpers -------------------------------------------------

    fn page_header(cx: &App, title: &'static str, desc: &'static str) -> Div {
        v_flex()
            .gap_0p5()
            .child(
                div()
                    .text_size(px(18.))
                    .font_weight(FontWeight::BOLD)
                    .child(title),
            )
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(muted_foreground(cx))
                    .child(desc),
            )
    }

    fn card(cx: &App, children: Div) -> Div {
        div()
            .w_full()
            .rounded(px(10.))
            .border_1()
            .border_color(card_border(cx))
            .bg(card_background(cx))
            .p_3()
            .child(children)
    }

    #[allow(clippy::too_many_arguments)]
    fn setting_row(
        &self,
        cx: &Context<Self>,
        id: &'static str,
        title: &'static str,
        desc: &'static str,
        checked: bool,
        setter: fn(&mut Self, bool),
        entity: &Entity<Self>,
    ) -> Div {
        let entity = entity.clone();
        h_flex()
            .items_center()
            .justify_between()
            .py_1p5()
            .child(
                v_flex()
                    .max_w(px(520.))
                    .child(div().text_size(px(13.5)).child(title))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(muted_foreground(cx))
                            .child(desc),
                    ),
            )
            .child(
                Switch::new(id)
                    .checked(checked)
                    .on_click(move |checked: &bool, _, cx| {
                        entity.update(cx, |this, cx| {
                            setter(this, *checked);
                            this.persist(cx);
                            cx.notify();
                        });
                    }),
            )
    }

    /// 自绘侧边栏：布局完全自控，折叠/展开两种状态共用同一套内边距与
    /// 恒定高度的标题行，菜单起始位置不随折叠变化（此前组件 Sidebar 在
    /// 折叠态对头部与内容区有不对称的内边距调整，补偿无法对齐）。
    fn render_sidebar(&self, cx: &mut Context<Self>) -> Div {
        let entity = cx.entity();
        let collapsed = self.sidebar_collapsed;
        let width = if collapsed { 48. } else { 210. };
        let sidebar_bg = cx.theme().sidebar;
        let sidebar_fg = cx.theme().sidebar_foreground;
        let sidebar_border = cx.theme().sidebar_border;
        let sidebar_accent = cx.theme().sidebar_accent;
        let sidebar_accent_fg = cx.theme().sidebar_accent_foreground;

        let make_toggle = || {
            let toggle_entity = entity.clone();
            Button::new("sidebar-toggle")
                .ghost()
                .icon(Icon::new(if collapsed {
                    IconName::PanelLeftOpen
                } else {
                    IconName::PanelLeftClose
                }))
                .on_click(move |_, _, cx| {
                    toggle_entity.update(cx, |this, cx| {
                        this.sidebar_collapsed = !this.sidebar_collapsed;
                        this.persist(cx);
                        cx.notify();
                    });
                })
        };

        // 标题行两种状态恒定 32px 高：折叠时仅折叠按钮居中。
        let header = if collapsed {
            h_flex()
                .h(px(32.))
                .items_center()
                .justify_center()
                .child(make_toggle())
                .into_any_element()
        } else {
            h_flex()
                .h(px(32.))
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(FontWeight::BOLD)
                        .child("assfonts"),
                )
                .child(make_toggle())
                .into_any_element()
        };

        let nav_item =
            |id: &'static str, icon: IconName, label: &'static str, page: Page| -> AnyElement {
                let nav_entity = entity.clone();
                let active = self.page == page;
                let row = h_flex()
                    .id(id)
                    .h(px(32.))
                    .w_full()
                    .flex_shrink_0()
                    .items_center()
                    .rounded(px(6.))
                    .text_color(if active {
                        sidebar_accent_fg
                    } else {
                        sidebar_fg
                    })
                    .when(active, |this| this.bg(sidebar_accent))
                    .when(!active, |this| {
                        this.hover(|style| style.bg(sidebar_accent.opacity(0.8)))
                    })
                    .on_click(move |_, _, cx| {
                        nav_entity.update(cx, |this, cx| {
                            this.page = page;
                            cx.notify();
                        });
                    });

                if collapsed {
                    row.justify_center()
                        .child(Icon::new(icon))
                        .into_any_element()
                } else {
                    row.gap_2()
                        .px_2()
                        .child(Icon::new(icon))
                        .child(div().text_size(px(13.)).child(label))
                        .into_any_element()
                }
            };

        v_flex()
            .flex_shrink_0()
            .h_full()
            .w(px(width))
            .bg(sidebar_bg)
            .text_color(sidebar_fg)
            .border_r_1()
            .border_color(sidebar_border)
            .pt_2()
            .px_2()
            .gap_1()
            .child(header)
            .child(nav_item(
                "nav-process",
                IconName::Film,
                "处理",
                Page::Process,
            ))
            .child(nav_item("nav-fonts", IconName::Type, "字体", Page::Fonts))
            .child(nav_item(
                "nav-rename",
                IconName::PencilLine,
                "重命名",
                Page::Rename,
            ))
            .child(nav_item("nav-logs", IconName::FileText, "日志", Page::Logs))
            .child(nav_item(
                "nav-settings",
                IconName::SlidersHorizontal,
                "设置",
                Page::Settings,
            ))
            .child(div().flex_1())
            .child(self.render_theme_footer(cx))
    }

    /// 侧边栏底部的主题切换：图标随当前生效模式变化，点击在 自动/亮色/暗色 间循环。
    fn render_theme_footer(&self, cx: &mut Context<Self>) -> Div {
        let entity = cx.entity();
        let collapsed = self.sidebar_collapsed;
        let theme_label = THEME_CHOICES[self.theme_choice % THEME_CHOICES.len()];
        let theme_icon = match resolve_theme_mode(self.theme_choice, cx) {
            ThemeMode::Dark => IconName::Moon,
            ThemeMode::Light => IconName::Sun,
        };

        let button = Button::new("theme-toggle")
            .ghost()
            .icon(Icon::new(theme_icon))
            .on_click(move |_, _, cx| {
                entity.update(cx, |this, cx| {
                    this.cycle_theme(cx);
                });
            });

        if collapsed {
            h_flex()
                .h(px(32.))
                .items_center()
                .justify_center()
                .child(button)
        } else {
            h_flex()
                .h(px(32.))
                .items_center()
                .gap_2()
                .px_2()
                .child(button)
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(muted_foreground(cx))
                        .child(theme_label),
                )
        }
    }

    fn cycle_theme(&mut self, cx: &mut App) {
        self.theme_choice = (self.theme_choice + 1) % THEME_CHOICES.len();
        let mode = resolve_theme_mode(self.theme_choice, cx);
        Theme::change(mode, None, cx);
        self.persist(cx);
    }

    fn render_process(&self, cx: &mut Context<Self>) -> Div {
        let running = self.running;
        let entity = cx.entity();
        let muxer_entity = entity.clone();
        let run_entity = entity.clone();
        let check_entity = entity.clone();
        let format_entity = entity.clone();
        let cancel_entity = entity.clone();
        let muxer_label = MUXERS[self.muxer_index % MUXERS.len()];
        let format_label = FORMATS[self.format_index % FORMATS.len()];

        let drop_input = |entity: &Entity<Self>,
                          state: &Entity<InputState>,
                          mode: DropMode,
                          element: AnyElement|
         -> AnyElement {
            let state = state.clone();
            let drop_entity = entity.clone();
            div()
                .w_full()
                .rounded(px(4.))
                .on_drop::<ExternalPaths>(move |files, window, cx| {
                    drop_entity.update(cx, |this, cx| {
                        this.adopt_dropped_paths(&state, mode, &files.0, window, cx);
                        this.persist(cx);
                    });
                })
                .drag_over::<ExternalPaths>(|style, _, _, cx| {
                    style
                        .border_1()
                        .border_color(cx.theme().primary.opacity(0.7))
                })
                .child(element)
                .into_any_element()
        };

        let (mkvmerge_found, ffmpeg_found) = self.tools_found;
        let tool_chip = |name: &'static str, found: bool| -> Div {
            h_flex()
                .gap_1()
                .items_center()
                .child(div().size(px(7.)).rounded_full().bg(if found {
                    gpui_kit::hsla(0.33, 0.7, 0.45, 1.0)
                } else {
                    muted_foreground(cx)
                }))
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(muted_foreground(cx))
                        .child(name),
                )
        };

        let muted = muted_foreground(cx);
        let border = card_border(cx);
        let foreground = cx.theme().foreground;
        let danger = cx.theme().danger;
        let success = cx.theme().success;
        let colors = RenameRowColors {
            border,
            muted,
            foreground,
            danger,
            success,
        };
        let clear_entity = entity.clone();
        let accent = cx.theme().primary;
        let video_col = self.process_video_col;
        let product_col = self.process_product_col;
        let down_video = entity.clone();
        let down_product = entity.clone();
        let col_move = entity.clone();
        let col_up = entity.clone();
        let col_up_out = entity.clone();

        // 表格行：按视频分组，一个视频的多个字幕共用左侧视频名单元格；
        // mks/mkv 下同一个视频只产出一个文件，产物列也只在组内显示一次。
        let embed = matches!(format_output_format(self.format_index), OutputFormat::Embed);
        let column_line = border.opacity(0.55);
        let mut table_groups: Vec<AnyElement> = Vec::new();
        let mut cursor = 0usize;
        while cursor < self.process_rows.len() {
            if table_groups.len() >= 600 {
                break;
            }
            let video = self.process_rows[cursor].video.clone();
            let start = cursor;
            while cursor < self.process_rows.len() && self.process_rows[cursor].video == video {
                cursor += 1;
            }
            let slice = &self.process_rows[start..cursor];
            let merge_product = !embed && video.is_some();
            let mut merged_product: Option<(String, bool)> = None;
            let mut lines: Vec<AnyElement> = Vec::with_capacity(slice.len());
            for (offset, row) in slice.iter().enumerate() {
                let (product, blocked) = self.product_name(row);
                if merge_product && merged_product.is_none() {
                    merged_product = Some((product.clone(), blocked));
                }
                let mut pills: Vec<AnyElement> = Vec::new();
                if let Some(name) = row.fansub_name.clone() {
                    pills.push(suffix_pill(name, colors));
                }
                if let Some(name) = row.language_name.clone() {
                    pills.push(suffix_pill(name, colors));
                }
                let menu_entity = entity.clone();
                let menu_key = row.key.clone();
                let menu_fansub = row.fansub.clone();
                let menu_language = row.language.clone();
                let menu_groups = self.fansub_groups.clone();
                let menu_categories = self.suffix_categories.clone();
                let default_key = self.default_track_key.clone();
                let sub_cell = h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(12.5))
                            .child(display_name(&row.sub)),
                    )
                    .children(pills)
                    .context_menu(move |menu, window, cx| {
                        subtitle_context_menu(
                            menu,
                            window,
                            cx,
                            menu_entity.clone(),
                            menu_key.clone(),
                            menu_fansub.clone(),
                            menu_language.clone(),
                            menu_groups.clone(),
                            menu_categories.clone(),
                            default_key.clone(),
                        )
                    });
                let mut line = h_flex().w_full().items_center().child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .px_3()
                        .py_1()
                        .when(offset > 0, |this| {
                            this.border_t_1().border_color(border.opacity(0.25))
                        })
                        .child(sub_cell),
                );
                if !merge_product {
                    line = line.child(
                        div()
                            .w(px(product_col))
                            .flex_none()
                            .border_l_1()
                            .border_color(column_line)
                            .px_3()
                            .py_1()
                            .when(offset > 0, |this| {
                                this.border_t_1().border_color(border.opacity(0.25))
                            })
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(11.5))
                                    .when(blocked, |this| this.text_color(danger))
                                    .when(!blocked, |this| this.text_color(foreground))
                                    .child(product),
                            ),
                    );
                }
                lines.push(line.into_any_element());
            }
            let video_label = video
                .as_ref()
                .map(|path| display_name(path))
                .unwrap_or_else(|| "— 未配对 —".to_string());
            let mut group = h_flex()
                .w_full()
                .items_stretch()
                .border_t_1()
                .border_color(border.opacity(0.35))
                .child(
                    div()
                        .w(px(video_col))
                        .flex_none()
                        .border_r_1()
                        .border_color(column_line)
                        .child(
                            h_flex()
                                .w_full()
                                .h_full()
                                .items_center()
                                .justify_center()
                                .px_3()
                                .py_1()
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(12.5))
                                        .text_color(foreground)
                                        .child(video_label),
                                ),
                        ),
                )
                .child(v_flex().flex_1().min_w_0().children(lines));
            if let Some((product, blocked)) = merged_product {
                group = group.child(
                    div()
                        .w(px(product_col))
                        .flex_none()
                        .border_l_1()
                        .border_color(column_line)
                        .child(
                            h_flex()
                                .w_full()
                                .h_full()
                                .items_center()
                                .justify_center()
                                .px_3()
                                .py_1()
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(11.5))
                                        .when(blocked, |this| this.text_color(danger))
                                        .when(!blocked, |this| this.text_color(foreground))
                                        .child(product),
                                ),
                        ),
                );
            }
            table_groups.push(group.into_any_element());
        }

        // 表头始终显示，空表格时也能看出三列含义；竖线与行线让每列边界看得清。
        let process_header = h_flex()
            .w_full()
            .flex_none()
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .w(px(video_col))
                    .flex_none()
                    .relative()
                    .border_r_1()
                    .border_color(column_line)
                    .child(
                        h_flex()
                            .w_full()
                            .justify_center()
                            .px_3()
                            .py_2()
                            .child(div().text_size(px(11.5)).text_color(muted).child("视频")),
                    )
                    .child(
                        div()
                            .id("process-col-handle-video")
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .right_0()
                            .w(px(8.))
                            .cursor(CursorStyle::ResizeLeftRight)
                            .hover(move |style| style.bg(accent.opacity(0.45)))
                            .on_mouse_down(MouseButton::Left, {
                                let handle = down_video.clone();
                                move |event, _, cx| {
                                    let x = f32::from(event.position.x);
                                    handle.update(cx, |this, cx| {
                                        this.start_process_col_drag(0, x);
                                        cx.notify();
                                    });
                                }
                            }),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .relative()
                    .child(
                        h_flex()
                            .w_full()
                            .justify_center()
                            .px_3()
                            .py_2()
                            .child(div().text_size(px(11.5)).text_color(muted).child("字幕")),
                    )
                    .child(
                        div()
                            .id("process-col-handle-product")
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .right_0()
                            .w(px(8.))
                            .cursor(CursorStyle::ResizeLeftRight)
                            .hover(move |style| style.bg(accent.opacity(0.45)))
                            .on_mouse_down(MouseButton::Left, {
                                let handle = down_product.clone();
                                move |event, _, cx| {
                                    let x = f32::from(event.position.x);
                                    handle.update(cx, |this, cx| {
                                        this.start_process_col_drag(1, x);
                                        cx.notify();
                                    });
                                }
                            }),
                    ),
            )
            .child(
                div()
                    .w(px(product_col))
                    .flex_none()
                    .border_l_1()
                    .border_color(column_line)
                    .child(
                        h_flex()
                            .w_full()
                            .justify_center()
                            .px_3()
                            .py_2()
                            .child(div().text_size(px(11.5)).text_color(muted).child("产物")),
                    ),
            );

        let process_table: AnyElement = if self.process_rows.is_empty() {
            v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .items_center()
                .justify_center()
                .gap_1()
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(muted)
                        .child("把字幕文件/文件夹（以及 mkv 模式的媒体文件）拖到这里"),
                )
                .child(div().text_size(px(11.5)).text_color(muted).child(
                    "自动区分视频与字幕并按文件名配对；.ass/.ssa 可用于内嵌，其它格式请选 mks/mkv",
                ))
                .into_any_element()
        } else {
            div()
                .id("process-scroll")
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_y_scroll()
                .children(table_groups)
                .into_any_element()
        };

        v_flex()
            .p_4()
            .gap_3()
            .size_full()
            // 整页都能接收文件拖入：卡片内外、空状态区域都算目标。
            .on_drop::<ExternalPaths>({
                let drop_entity = entity.clone();
                move |files, _window, cx| {
                    drop_entity.update(cx, |this, cx| {
                        this.drop_into_process(&files.0, cx);
                    });
                }
            })
            .drag_over::<ExternalPaths>(|style, _, _, cx| {
                style
                    .border_1()
                    .border_color(cx.theme().primary.opacity(0.45))
            })
            .child(
                v_flex()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .items_center()
                            .child(
                                div()
                                    .text_size(px(18.))
                                    .font_weight(FontWeight::BOLD)
                                    .child("处理"),
                            )
                            .child(div().flex_1())
                            .child(tool_chip("mkvmerge", mkvmerge_found))
                            .child(div().w_3())
                            .child(tool_chip("ffmpeg", ffmpeg_found)),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(muted_foreground(cx))
                            .child(
                                "字幕字体子集化、内嵌或封装为 mks/mkv（输入框支持拖入，可多选）",
                            ),
                    ),
            )
            .child(
                // 卡片本体用纵向 flex：表头固定，表格区 flex_1 吃满剩余高度并自己滚动。
                v_flex()
                    .flex_1()
                    .min_h(px(180.))
                    .w_full()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(border)
                    .bg(card_background(cx))
                    .p_3()
                    .on_mouse_move(move |event, _, cx| {
                        let x = f32::from(event.position.x);
                        col_move.update(cx, |this, cx| {
                            if this.drag_process_col(x) {
                                cx.notify();
                            }
                        });
                    })
                    .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                        col_up.update(cx, |this, _| this.end_process_col_drag());
                    })
                    .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                        col_up_out.update(cx, |this, _| this.end_process_col_drag());
                    })
                    .child(process_header)
                    .child(process_table),
            )
            // 操作栏固定在最下方：字体目录 / 格式 / 封装器 / 运行。
            .child(Self::card(
                cx,
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().text_size(px(13.)).child("字体目录"))
                            .child(div().flex_1().child(drop_input(
                                &entity,
                                &self.fontpaths_input,
                                DropMode::Append,
                                Input::new(&self.fontpaths_input).into_any_element(),
                            ))),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().text_size(px(13.)).child("格式"))
                            .child(Button::new("format").label(format_label).on_click(
                                move |_, _, cx| {
                                    format_entity.update(cx, |this, cx| {
                                        this.format_index = (this.format_index + 1) % FORMATS.len();
                                        this.persist(cx);
                                        cx.notify();
                                    });
                                },
                            ))
                            .child(div().text_size(px(13.)).child("封装器"))
                            .child(Button::new("muxer").label(muxer_label).on_click(
                                move |_, _, cx| {
                                    muxer_entity.update(cx, |this, cx| {
                                        this.muxer_index = (this.muxer_index + 1) % MUXERS.len();
                                        this.persist(cx);
                                        cx.notify();
                                    });
                                },
                            ))
                            .child(div().flex_1())
                            .when(!self.process_rows.is_empty(), |this| {
                                this.child(
                                    Button::new("clear-process").label("清空").ghost().on_click(
                                        move |_, _, cx| {
                                            clear_entity.update(cx, |this, cx| {
                                                this.clear_process(cx);
                                            });
                                        },
                                    ),
                                )
                            })
                            .child(
                                // 测试：只检查字体与字形，不写任何输出文件。
                                Button::new("check-fonts").label("测试").ghost().on_click(
                                    move |_, _, cx| {
                                        run_check_clicked(&check_entity, cx);
                                    },
                                ),
                            )
                            .child(
                                Button::new("run")
                                    .primary()
                                    .label(if running { "处理中…" } else { "运行" })
                                    .on_click(move |_, _, cx| {
                                        run_process_clicked(&run_entity, cx);
                                    }),
                            )
                            .when(running, |this| {
                                this.child(Button::new("cancel").label("取消").on_click(
                                    move |_, _, cx| {
                                        cancel_entity.update(cx, |this, cx| {
                                            this.request_cancel(cx);
                                        });
                                    },
                                ))
                            }),
                    ),
            ))
    }

    /// 日志页：独立展示处理/封装/重命名日志，可清空或保存到程序目录。
    fn render_logs(&self, cx: &mut Context<Self>) -> Div {
        let entity = cx.entity();
        let save_entity = entity.clone();
        let clear_entity = entity.clone();
        let muted = muted_foreground(cx);
        let logs_empty = self.logs.is_empty();
        let log_lines: Vec<Div> = self
            .logs
            .iter()
            .map(|line| Self::log_line(cx, line, missing_chars_range(line)))
            .collect();
        let summary = match self.last_log_saved.as_deref() {
            Some(path) => format!("共 {} 行 · 上次保存：{path}", self.logs.len()),
            None => format!("共 {} 行 · 保存在程序目录", self.logs.len()),
        };

        v_flex()
            .p_4()
            .gap_3()
            .size_full()
            .child(Self::page_header(
                cx,
                "日志",
                "处理、封装与重命名的运行日志；可保存到程序目录（与 gui.toml 同目录，文件名用 UTC 时间）",
            ))
            .child(
                Self::card(
                    cx,
                    // card 是块级容器（不是 flex），子元素用 flex_1 拿不到高度；
                    // 用 h_full 撑满卡片内容盒，内部滚动区才有确定视口。
                    div()
                        .relative()
                        .h_full()
                        .min_h_0()
                        .w_full()
                        .child(
                            div()
                                .id("log-view")
                                .size_full()
                                // 右侧留出滚动条宽度，文字不压在滚动条下面。
                                .pr(px(12.))
                                .overflow_y_scroll()
                                // 日志只有纵向滚动：触控板的横向位移不驱动这里。
                                .restrict_scroll_to_axis()
                                .track_scroll(&self.log_scroll_y)
                                .when(logs_empty, |this| {
                                    this.child(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(muted)
                                            .child("暂无日志，运行一次处理或重命名后这里会有输出。"),
                                    )
                                })
                                .when(!logs_empty, |this| {
                                    this.child(v_flex().w_full().children(log_lines))
                                }),
                        )
                        // 滚动条叠加在日志框右缘，始终可见（与重命名表格一致）。
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .right_0()
                                .w(px(10.))
                                .child(
                                    Scrollbar::vertical(&self.log_scroll_y)
                                        .mode(ScrollbarMode::Always)
                                        .viewport_from_layout(),
                                ),
                        ),
                )
                .flex_1()
                .min_h(px(160.)),
            )
            // 清空 / 保存按钮放在日志框下方。
            .child(Self::card(
                cx,
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().text_size(px(12.)).text_color(muted).child(summary))
                    .child(div().flex_1())
                    .child(
                        Button::new("clear-log")
                            .label("清空")
                            .ghost()
                            .on_click(move |_, _, cx| {
                                clear_entity.update(cx, |this, cx| {
                                    this.logs.clear();
                                    this.last_log_saved = None;
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        Button::new("save-log")
                            .primary()
                            .label("保存日志")
                            .on_click(move |_, _, cx| {
                                save_entity.update(cx, |this, cx| this.save_log(cx));
                            }),
                    ),
            ))
    }

    fn render_fonts(&self, cx: &mut Context<Self>) -> Div {
        let entity = cx.entity();
        v_flex()
            .p_4()
            .gap_3()
            .child(Self::page_header(cx, "字体", "字体索引与字体库维护"))
            .child(Self::card(
                cx,
                v_flex().gap_2()
                    .child(div().text_size(px(13.5)).child("重建字体索引 (fonts.db)"))
                    .child(
                        div().text_size(px(11.5)).text_color(muted_foreground(cx)).child(
                            "扫描处理页填写的字体目录并重建 fonts.db；输出目录取 fonts.db 所在目录输入",
                        ),
                    )
                    .child(
                        Button::new("rebuild")
                            .primary()
                            .label("重建索引")
                            .on_click(move |_, _, cx| {
                                run_build_clicked(&entity, cx);
                            }),
                    ),
            ))
    }

    fn render_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let entity = cx.entity();
        let muted = muted_foreground(cx);
        let border = card_border(cx);
        let foreground = cx.theme().foreground;
        let accent = cx.theme().primary;
        let danger = cx.theme().danger;
        let success = cx.theme().success;
        let summary = self.rename_summary();
        let has_scan = self.rename_scan.is_some();
        // 没手动拖过列宽时，三列按窗口可用宽度自适应，打开就不再出现横向滚动条。
        // 可用宽度 = 窗口 - 侧边栏 - 页面左右内边距(32) - 卡片边框(2) - 纵向滚动条(12) - 余量(8)。
        let sidebar = if self.sidebar_collapsed { 48. } else { 210. };
        let usable =
            (f32::from(window.viewport_size().width) - sidebar - 54.).max(RENAME_COL_MIN * 3.);
        let columns = if self.rename_col_custom {
            self.rename_cols
        } else {
            [
                (usable * 0.24).max(RENAME_COL_MIN),
                (usable * 0.36).max(RENAME_COL_MIN),
                (usable * 0.40).max(RENAME_COL_MIN),
            ]
        };
        let last_fixed = self.rename_col_last_fixed;
        let table_width: f32 = columns.iter().sum();
        // 只有内容确实宽于视口时才显示横向滚动条（自适应列宽时恒为 false）。
        let needs_h = table_width > usable + 0.5;

        let table_entity = entity.clone();
        let table_entity_up = entity.clone();
        let table_entity_out = entity.clone();

        // 表头三列，每列右缘有一个 8px 宽的拖拽把手（像 Excel 那样调列宽）。
        let header_cell = |text: &'static str, column: usize| -> Div {
            let resize_entity = entity.clone();
            // 前两列固定像素宽；最后一列吃掉剩余宽度（不足时用各自的最小宽度
            // 撑开表格，交给横向滚动），这样窗口变宽不会在右侧留下空白。
            div()
                .flex_none()
                .relative()
                .px_3()
                .py_2()
                .when(column + 1 == columns.len() && !last_fixed, |this| {
                    this.flex_1().min_w(px(columns[column]))
                })
                .when(column + 1 == columns.len() && last_fixed, |this| {
                    this.w(px(columns[column]))
                })
                .when(column + 1 < columns.len(), |this| {
                    this.w(px(columns[column]))
                })
                .flex()
                .items_center()
                .justify_center()
                .when(column + 1 < columns.len(), |this| {
                    this.border_r_1().border_color(border)
                })
                .child(div().text_size(px(11.5)).text_color(muted).child(text))
                .child(
                    div()
                        .id(("rename-col-handle", column))
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        .w(px(8.))
                        .cursor(CursorStyle::ResizeLeftRight)
                        .hover(move |style| style.bg(accent.opacity(0.45)))
                        .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                            let x = f32::from(event.position.x);
                            resize_entity.update(cx, |this, cx| {
                                this.start_column_drag(column, x, columns);
                                cx.notify();
                            });
                        }),
                )
        };

        let header = h_flex()
            .w_full()
            .flex_none()
            .border_b_1()
            .border_color(border)
            .child(header_cell("视频文件名", 0))
            .child(header_cell("字幕文件名", 1))
            .child(header_cell("最终文件名", 2));

        // 一行一个字幕；同一个视频的字幕归在一个组里，视频名只显示一次并垂直
        // 居中，占据该组所有行的高度（合并单元格的效果）。
        let colors = RenameRowColors {
            border,
            muted,
            foreground,
            danger,
            success,
        };
        let edit = self
            .rename_edit_key
            .as_ref()
            .map(|key| (key, &self.custom_suffix_input));
        let row_context = RenameRowContext {
            entity: &entity,
            columns,
            style: colors,
            groups: &self.fansub_groups,
            categories: &self.suffix_categories,
            default_key: self.default_track_key.clone(),
            edit,
        };
        let mut groups: Vec<AnyElement> = Vec::new();
        let mut shown = 0usize;
        for row in &self.rename_rows {
            if shown >= RENAME_ROWS_SHOWN {
                break;
            }
            let video_name = row
                .video
                .as_deref()
                .map(display_name)
                .unwrap_or_else(|| "（未配对）".to_string());
            let entries: Vec<AnyElement> = if row.entries.is_empty() {
                // 只有视频、还没拖字幕：留一行占位，让“拖入视频”也有可见反馈。
                vec![rename_entry_row(
                    &row_context,
                    None,
                    true,
                    self.rename_col_last_fixed,
                )]
            } else {
                row.entries
                    .iter()
                    .enumerate()
                    .map(|(index, entry)| {
                        rename_entry_row(
                            &row_context,
                            Some(entry),
                            index == 0,
                            self.rename_col_last_fixed,
                        )
                    })
                    .collect()
            };
            shown += row.entries.len().max(1);
            groups.push(
                h_flex()
                    .w_full()
                    .border_b_1()
                    .border_color(border)
                    .hover(|style| style.bg(accent.opacity(0.08)))
                    .child(
                        div()
                            .w(px(columns[0]))
                            .flex_none()
                            .px_3()
                            .truncate()
                            .text_size(px(12.))
                            .text_color(foreground)
                            .border_r_1()
                            .border_color(border)
                            .child(video_name),
                    )
                    .child(v_flex().flex_1().min_w_0().children(entries))
                    .into_any_element(),
            );
        }
        let total = summary.subtitles;
        let truncated = shown < total;

        let rows_content: AnyElement = if self.rename_rows.is_empty() {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_1()
                .child(div().text_size(px(14.)).child(if has_scan {
                    "没有识别到视频或字幕文件"
                } else {
                    "把视频与字幕文件夹拖到这里"
                }))
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(muted)
                        .child(if has_scan {
                            "检查设置页的扩展名列表，或直接拖入单个文件"
                        } else {
                            "目录会递归扫描；扩展名决定文件是视频还是字幕"
                        }),
                )
                .into_any_element()
        } else {
            v_flex()
                .w_full()
                .children(groups)
                .when(truncated, |this| {
                    this.child(
                        div()
                            .px_3()
                            .py_2()
                            .text_size(px(11.))
                            .text_color(muted)
                            .child(format!(
                                "仅显示前 {shown} 行，共 {total} 行；应用重命名会处理全部就绪行"
                            )),
                    )
                })
                .into_any_element()
        };

        // 横向滚动包住表头和行区，纵向滚动只作用于行区；两条滚动条叠加在
        // 表格边缘，鼠标滚动与拖拽互不干扰。
        let table_area = div()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .on_mouse_move(move |event, _, cx| {
                let x = f32::from(event.position.x);
                table_entity.update(cx, |this, cx| {
                    if this.drag_column(x) {
                        cx.notify();
                    }
                });
            })
            .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                table_entity_up.update(cx, |this, _| this.end_column_drag());
            })
            .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                table_entity_out.update(cx, |this, _| this.end_column_drag());
            })
            .child(
                div()
                    .id("rename-scroll-x")
                    .size_full()
                    .overflow_x_scroll()
                    // 竖向滚轮只滚行区（纵向），横向只在 Shift+滚轮 或触控板
                    // 真有横向位移时才动，避免两个滚动条被同一次滚动同时驱动。
                    .restrict_scroll_to_axis()
                    .track_scroll(&self.rename_scroll_x)
                    .child(
                        v_flex()
                            .min_w(px(table_width))
                            .w_full()
                            .h_full()
                            .child(header)
                            .child(
                                div().flex_1().min_h_0().child(
                                    div()
                                        .id("rename-scroll-y")
                                        .size_full()
                                        .overflow_y_scroll()
                                        // 只回应该轴的滚动：Shift+滚轮带来的水平
                                        // 位移不再被这里当成纵向滚动。
                                        .restrict_scroll_to_axis()
                                        .track_scroll(&self.rename_scroll_y)
                                        .child(rows_content),
                                ),
                            ),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right_0()
                    .w(px(12.))
                    .child(
                        Scrollbar::vertical(&self.rename_scroll_y)
                            .mode(ScrollbarMode::Always)
                            .viewport_from_layout(),
                    ),
            )
            .when(needs_h, |this| {
                this.child(
                    div()
                        .absolute()
                        .left_0()
                        .right(px(12.))
                        .bottom_0()
                        .h(px(12.))
                        .child(
                            Scrollbar::horizontal(&self.rename_scroll_x)
                                .mode(ScrollbarMode::Always)
                                .viewport_from_layout(),
                        ),
                )
            });

        let hint_text = if self.rename_roots.is_empty() {
            "还没有拖入任何文件夹".to_string()
        } else {
            format!(
                "视频 {} · 字幕 {} · 待重命名 {} · 目标已存在 {} · 未配对 {}{}",
                summary.videos,
                summary.subtitles,
                summary.ready,
                summary.blocked,
                summary.unmatched,
                if self.rename_keep_in_place {
                    " · 字幕原地"
                } else {
                    " · 字幕随视频目录"
                }
            )
        };
        let apply_label = if summary.ready > 0 {
            format!("应用重命名（{}）", summary.ready)
        } else {
            "应用重命名".to_string()
        };
        let can_apply = summary.has_ready();

        let clear_entity = entity.clone();
        let apply_entity = entity.clone();
        let drop_entity = entity.clone();

        // 操作按钮跟表格在同一张卡片里，表格和卡片之间不再留出多余空白。
        let bottom_bar = v_flex()
            .flex_none()
            .gap_2()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().text_size(px(12.)).text_color(muted).child(hint_text))
                    .child(div().flex_1())
                    .child(Button::new("rename-clear").ghost().label("清空").on_click(
                        move |_, _, cx| {
                            clear_entity.update(cx, |this, cx| {
                                this.clear_rename(cx);
                            });
                        },
                    ))
                    .child(
                        Button::new("rename-apply")
                            .primary()
                            .label(apply_label)
                            .disabled(!can_apply)
                            .on_click(move |_, _, cx| {
                                apply_entity.update(cx, |this, cx| {
                                    this.apply_renames(cx);
                                });
                            }),
                    ),
            );

        v_flex()
            .p_4()
            .gap_3()
            .size_full()
            .child(Self::page_header(
                cx,
                "重命名",
                "拖入视频与字幕文件夹，按文件名自动配对并批量对齐字幕名",
            ))
            .child(
                div()
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(border)
                    .bg(card_background(cx))
                    .overflow_hidden()
                    .on_drop::<ExternalPaths>(move |files, _, cx| {
                        drop_entity.update(cx, |this, cx| {
                            this.drop_into_rename(&files.0, cx);
                        });
                    })
                    .drag_over::<ExternalPaths>(|style, _, _, cx| {
                        style.border_color(cx.theme().primary.opacity(0.7))
                    })
                    .child(v_flex().size_full().child(table_area).child(bottom_bar)),
            )
    }

    fn render_settings(&self, cx: &mut Context<Self>) -> Div {
        let entity = cx.entity();
        let muxer_label = MUXERS[self.muxer_index % MUXERS.len()];
        let row = |id: &'static str,
                   title: &'static str,
                   desc: &'static str,
                   checked: bool,
                   setter: fn(&mut Self, bool)| {
            self.setting_row(cx, id, title, desc, checked, setter, &entity)
        };

        let muxer_entity = entity.clone();
        let track_entity = entity.clone();
        let track_label = match self.track_name_style {
            TrackNameStyle::English => "英文（KTXP.TC）",
            TrackNameStyle::Chinese => "中文（极影字幕社.繁体）",
        };
        v_flex()
            .p_4()
            .gap_3()
            .child(Self::page_header(cx, "设置", "处理与封装选项"))
            .child(Self::card(
                cx,
                v_flex()
                    .gap_0p5()
                    .child(div().text_size(px(12.)).text_color(muted_foreground(cx)).child("处理"))
                    .child(row(
                        "strict",
                        "严格模式",
                        "字体缺失或字符缺失时中止处理，而不是尽力继续",
                        self.strict_mode,
                        |this, value| this.strict_mode = value,
                    ))
                    .child(row(
                        "allow-sample",
                        "允许缺失字符样本",
                        "字体缺少字幕所需字符时继续处理",
                        self.allow_missing_sample,
                        |this, value| this.allow_missing_sample = value,
                    ))
                    .child(row(
                        "allow-fonts",
                        "允许缺失字体",
                        "字幕引用的字体找不到时继续处理",
                        self.allow_missing_fonts,
                        |this, value| this.allow_missing_fonts = value,
                    ))
                    .child(row(
                        "rename-fonts",
                        "CRC 字体改名",
                        "子集字体以 CRC 后缀重命名并同步改写 ASS 引用，避免同族多字面歧义",
                        self.rename_fonts,
                        |this, value| this.rename_fonts = value,
                    ))
                    .child(row(
                        "utf8-sig",
                        "输出带 BOM（utf-8-sig）",
                        "写出的 ASS 文件带 UTF-8 BOM，兼容部分老播放器与工具链",
                        self.utf8_sig,
                        |this, value| this.utf8_sig = value,
                    ))
                    .child(row(
                        "char-compatible",
                        "子集化兼容字符集",
                        "子集字体恒保留 a-z/A-Z，出现任意数字时保留全部 0-9",
                        self.char_compatible,
                        |this, value| this.char_compatible = value,
                    ))
                    .child(row(
                        "use-embedded",
                        "使用字幕内嵌字体",
                        "把输入字幕 [Fonts] 段内嵌的字体作为最高优先级字体源（二次处理无需系统安装该字体）",
                        self.use_embedded_fonts,
                        |this, value| this.use_embedded_fonts = value,
                    )),
            ))
            .child(Self::card(
                cx,
                v_flex()
                    .gap_0p5()
                    .child(div().text_size(px(12.)).text_color(muted_foreground(cx)).child("封装"))
                    .child(row(
                        "keep-temp",
                        "保留中间文件",
                        "封装成功后保留 <输出>.fonts/ 目录（子集字体与改名字幕）",
                        self.keep_temp,
                        |this, value| this.keep_temp = value,
                    ))
                    .child(row(
                        "keep-subs",
                        "保留原字幕轨",
                        "mkv 模式下保留输入媒体中已有的字幕轨道",
                        self.keep_subs,
                        |this, value| this.keep_subs = value,
                    ))
                    .child(row(
                        "keep-attachments",
                        "保留原附件",
                        "mkv 模式下保留输入媒体中已有的附件",
                        self.keep_attachments,
                        |this, value| this.keep_attachments = value,
                    ))
                    .child(
                        v_flex()
                            .py_1p5()
                            .gap_1()
                            .child(div().text_size(px(13.5)).child("输出目录"))
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .text_color(muted_foreground(cx))
                                    .child("处理结果写到这个目录，留空用 out"),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .on_drop::<ExternalPaths>({
                                        let drop_entity = entity.clone();
                                        let state = self.output_input.clone();
                                        move |files, window, cx| {
                                            drop_entity.update(cx, |this, cx| {
                                                this.adopt_dropped_paths(
                                                    &state,
                                                    DropMode::Replace,
                                                    &files.0,
                                                    window,
                                                    cx,
                                                );
                                                this.persist(cx);
                                            });
                                        }
                                    })
                                    .drag_over::<ExternalPaths>(|style, _, _, cx| {
                                        style
                                            .border_1()
                                            .border_color(cx.theme().primary.opacity(0.7))
                                    })
                                    .child(Input::new(&self.output_input)),
                            ),
                    )
                    .child(
                        v_flex()
                            .py_1p5()
                            .gap_1()
                            .child(div().text_size(px(13.5)).child("fonts.db 目录"))
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .text_color(muted_foreground(cx))
                                    .child("字体索引 fonts.db 所在的目录，留空用当前目录"),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .on_drop::<ExternalPaths>({
                                        let drop_entity = entity.clone();
                                        let state = self.dbpath_input.clone();
                                        move |files, window, cx| {
                                            drop_entity.update(cx, |this, cx| {
                                                this.adopt_dropped_paths(
                                                    &state,
                                                    DropMode::Replace,
                                                    &files.0,
                                                    window,
                                                    cx,
                                                );
                                                this.persist(cx);
                                            });
                                        }
                                    })
                                    .drag_over::<ExternalPaths>(|style, _, _, cx| {
                                        style
                                            .border_1()
                                            .border_color(cx.theme().primary.opacity(0.7))
                                    })
                                    .child(Input::new(&self.dbpath_input)),
                            ),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .py_1p5()
                            .child(
                                v_flex()
                                    .max_w(px(520.))
                                    .child(div().text_size(px(13.5)).child("封装器"))
                                    .child(
                                        div()
                                            .text_size(px(11.5))
                                            .text_color(muted_foreground(cx))
                                            .child("mks/mkv 模式使用的外部封装工具"),
                                    ),
                            )
                            .child(
                                Button::new("muxer-select").label(muxer_label).on_click(
                                    move |_, _, cx| {
                                        muxer_entity.update(cx, |this, cx| {
                                            this.muxer_index =
                                                (this.muxer_index + 1) % MUXERS.len();
                                            this.persist(cx);
                                            cx.notify();
                                        });
                                    },
                                ),
                            ),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .py_1p5()
                            .child(
                                v_flex()
                                    .max_w(px(560.))
                                    .child(div().text_size(px(13.5)).child("字幕轨命名"))
                                    .child(
                                        div()
                                            .text_size(px(11.5))
                                            .text_color(muted_foreground(cx))
                                            .child(
                                                "mks/mkv 字幕轨标题用哪个后缀名；轨道语言标记按语言后缀自动设置（简体→zh-Hans、繁体→zh-Hant、日语→jpn、英语→eng）",
                                            ),
                                    ),
                            )
                            .child(
                                Button::new("track-name-style")
                                    .label(track_label)
                                    .on_click(move |_, _, cx| {
                                        track_entity.update(cx, |this, cx| {
                                            this.track_name_style =
                                                match this.track_name_style {
                                                    TrackNameStyle::English => {
                                                        TrackNameStyle::Chinese
                                                    }
                                                    TrackNameStyle::Chinese => {
                                                        TrackNameStyle::English
                                                    }
                                                };
                                            this.persist(cx);
                                            cx.notify();
                                        });
                                    }),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_2()
                            .pt_1()
                            .child(
                                div()
                                    .w_full()
                            .on_drop::<ExternalPaths>({
                                let drop_entity = entity.clone();
                                let state = self.muxer_path_input.clone();
                                move |files, window, cx| {
                                    drop_entity.update(cx, |this, cx| {
                                        this.adopt_dropped_paths(
                                            &state,
                                            DropMode::Replace,
                                            &files.0,
                                            window,
                                            cx,
                                        );
                                        this.persist(cx);
                                    });
                                }
                            })
                            .drag_over::<ExternalPaths>(|style, _, _, cx| {
                                style
                                    .border_1()
                                    .border_color(cx.theme().primary.opacity(0.7))
                            })
                            .child(Input::new(&self.muxer_path_input)),
                            )
                            .child(
                                div()
                                    .w_full()
                            .on_drop::<ExternalPaths>({
                                let drop_entity = entity.clone();
                                let state = self.ffprobe_path_input.clone();
                                move |files, window, cx| {
                                    drop_entity.update(cx, |this, cx| {
                                        this.adopt_dropped_paths(
                                            &state,
                                            DropMode::Replace,
                                            &files.0,
                                            window,
                                            cx,
                                        );
                                        this.persist(cx);
                                    });
                                }
                            })
                            .drag_over::<ExternalPaths>(|style, _, _, cx| {
                                style
                                    .border_1()
                                    .border_color(cx.theme().primary.opacity(0.7))
                            })
                            .child(Input::new(&self.ffprobe_path_input)),
                            ),
                    ),
            ))
            .child(Self::card(
                cx,
                v_flex()
                    .gap_0p5()
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(muted_foreground(cx))
                            .child("重命名"),
                    )

                    .child(row(
                        "rename-move",
                        "移动字幕到视频目录",
                        "输出字幕写入对应视频所在目录；关闭则原地重命名（多目录时推荐开启）",
                        !self.rename_keep_in_place,
                        |this, value| this.rename_keep_in_place = !value,
                    ))
                    .child(row(
                        "rename-copy",
                        "复制而非移动",
                        "保留原字幕文件，写入一份对齐后的副本",
                        self.rename_copy_mode,
                        |this, value| this.rename_copy_mode = value,
                    ))
                    .child(row(
                        "rename-fansub",
                        "字幕组后缀",
                        "从字幕注释识别字幕组（KTXP/CASO…），在最终文件名里插入 .KTXP；规则见 gui.toml 的 [[fansub_groups]]",
                        self.fansub_suffix,
                        |this, value| this.fansub_suffix = value,
                    ))
                    .child(row(
                        "rename-lang",
                        "语言后缀",
                        "按内置分类识别语言（简体→SC、繁日双语→TCJP…），在最终文件名里插入 .SC；规则见 gui.toml 的 [[suffix_categories]]",
                        self.language_suffix,
                        |this, value| this.language_suffix = value,
                    )),
            ))
    }
}

/// 表格行用到的主题色（集中传入，避免函数参数过多）。
#[derive(Clone, Copy)]
struct RenameRowColors {
    border: Hsla,
    muted: Hsla,
    foreground: Hsla,
    danger: Hsla,
    success: Hsla,
}

/// 渲染一行时共享的上下文（避免把一长串参数传给每一行）。
struct RenameRowContext<'a> {
    entity: &'a Entity<AssfontsApp>,
    columns: [f32; 3],
    style: RenameRowColors,
    groups: &'a [FansubGroup],
    categories: &'a [SuffixCategory],
    /// 当前「默认字幕」归类键，用于右键菜单勾选状态。
    default_key: Option<String>,
    /// 正在自定义后缀的行：`((0=字幕组 / 1=语言, 归类键), 输入框)`。
    edit: Option<(&'a (u8, String), &'a Entity<InputState>)>,
}

/// 字幕名后面的绿色标签：`澄空学园字幕组 简日双语`。
fn suffix_pill(text: String, colors: RenameRowColors) -> AnyElement {
    div()
        .flex_none()
        .px_1p5()
        .py_0p5()
        .rounded(px(999.))
        .bg(colors.success.opacity(0.16))
        .border_1()
        .border_color(colors.success.opacity(0.42))
        .text_size(px(10.))
        .text_color(colors.success)
        .child(text)
        .into_any_element()
}

fn rename_entry_row(
    context: &RenameRowContext<'_>,
    entry: Option<&RenameEntry>,
    first: bool,
    last_fixed: bool,
) -> AnyElement {
    let RenameRowContext {
        entity,
        columns,
        style,
        groups,
        categories,
        default_key,
        edit,
    } = context;
    let colors = *style;
    let RenameRowColors {
        border,
        muted,
        foreground,
        danger,
        ..
    } = colors;
    let (sub_text, target_text, target_color, note, note_color) = match entry {
        None => (
            "—".to_string(),
            "—".to_string(),
            muted,
            Some("未配对"),
            muted,
        ),
        Some(entry) => {
            let (target_text, target_color, note, note_color) = match entry.status {
                RenameStatus::Ready => (display_name(&entry.target), foreground, None, muted),
                RenameStatus::Unchanged => {
                    (display_name(&entry.target), muted, Some("已一致"), muted)
                }
                RenameStatus::Blocked => (
                    display_name(&entry.target),
                    muted,
                    Some("目标已存在"),
                    danger,
                ),
                RenameStatus::Unmatched => ("—".to_string(), muted, Some("未配对"), muted),
            };
            (
                display_name(&entry.sub),
                target_text,
                target_color,
                note,
                note_color,
            )
        }
    };

    // 字幕文件名：命中规则时后面跟一个绿色标签（字幕组中文名 + 语言分类中文名）。
    // 右键菜单可改这一“归类键”的字幕组/语言后缀，同类的其他字幕一起生效。
    let mut sub_cell = div()
        .w(px(columns[1]))
        .flex_none()
        .px_3()
        .py_1p5()
        .text_size(px(12.))
        .text_color(muted)
        .border_r_1()
        .border_color(border)
        .flex()
        .items_center()
        .justify_center()
        .child(div().min_w_0().truncate().child(sub_text));

    let sub_cell = match entry {
        Some(entry) => {
            let key = entry.key.clone();
            let current_fansub = entry.fansub.clone();
            let current_language = entry.language.clone();
            // 字幕组后缀与语言后缀是两个独立标签，各占一个小胶囊。
            let mut pills: Vec<AnyElement> = Vec::new();
            if let Some(name) = &entry.fansub_name {
                pills.push(suffix_pill(name.clone(), colors));
            }
            if let Some(name) = &entry.language_name {
                pills.push(suffix_pill(name.clone(), colors));
            }
            let editing = match edit {
                Some((edit_key, input)) if entry.key.as_str() == edit_key.1.as_str() => {
                    Some(*input)
                }
                _ => None,
            };
            sub_cell = match editing {
                // 正在自定义这一行：标签位置换成输入框，回车提交。
                Some(input) => sub_cell.child(div().w(px(130.)).ml_1().child(Input::new(input))),
                None if pills.is_empty() => sub_cell,
                None => sub_cell.child(h_flex().gap_1().ml_1().children(pills)),
            };

            let menu_entity = (*entity).clone();
            sub_cell
                .context_menu({
                    let menu_key = key.clone();
                    let menu_groups = groups.to_vec();
                    let menu_categories = categories.to_vec();
                    let current_fansub = current_fansub.clone();
                    let current_language = current_language.clone();
                    let default_key = default_key.clone();
                    move |menu, window, cx| {
                        subtitle_context_menu(
                            menu,
                            window,
                            cx,
                            menu_entity.clone(),
                            menu_key.clone(),
                            current_fansub.clone(),
                            current_language.clone(),
                            menu_groups.clone(),
                            menu_categories.clone(),
                            default_key.clone(),
                        )
                    }
                })
                .into_any_element()
        }
        None => sub_cell.into_any_element(),
    };

    h_flex()
        .w_full()
        .items_center()
        .when(!first, |this| {
            this.border_t_1().border_color(border.opacity(0.35))
        })
        .child(sub_cell)
        .child(
            h_flex()
                .when(last_fixed, |this| this.w(px(columns[2])).flex_none())
                .when(!last_fixed, |this| this.flex_1().min_w(px(columns[2])))
                .items_center()
                .justify_center()
                .gap_1()
                .px_3()
                .py_1p5()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.))
                        .text_color(target_color)
                        .child(target_text),
                )
                .when_some(note, |this, note| {
                    this.child(
                        div()
                            .flex_none()
                            .text_size(px(10.5))
                            .text_color(note_color)
                            .child(note),
                    )
                }),
        )
        .into_any_element()
}

fn muted_foreground(cx: &App) -> Hsla {
    cx.theme().muted_foreground
}

/// `SystemTime` → `YYYYMMDD-HHMMSS`（UTC），用于日志文件名。
fn utc_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// Howard Hinnant 的 civil-from-days：把 1970-01-01 起的天数换成年月日。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    (y + i64::from(m <= 2), m as u32, d as u32)
}

/// 字体覆盖行里缺字列表的字节范围：`…字）：'a' 'b'` 中的 `'a' 'b'` 部分。
fn missing_chars_range(line: &str) -> Option<std::ops::Range<usize>> {
    if !line.contains("缺 ") || !line.contains(" 字（共 ") {
        return None;
    }
    let start = line.rfind("字）：")? + "字）：".len();
    (start < line.len()).then_some(start..line.len())
}

fn card_background(cx: &App) -> Hsla {
    cx.theme().background
}

fn card_border(cx: &App) -> Hsla {
    cx.theme().border
}

fn run_process_clicked(entity: &Entity<AssfontsApp>, cx: &mut App) {
    entity.update(cx, |this, cx| {
        if this.running {
            return;
        }
        let weak = cx.entity().downgrade();
        let options = match this.collect_options(cx) {
            Ok(options) => options,
            Err(message) => {
                this.push_log(format!("[错误] {message}"));
                return;
            }
        };
        this.persist(cx);
        let mode = match options.format {
            OutputFormat::Embed => "embed",
            OutputFormat::Mks => "mks",
            OutputFormat::Mkv => "mkv",
        }
        .to_string();
        if let Some(message) = this.spawn_task(
            weak,
            cx,
            format!("处理 {} 个输入（{mode} 模式）", options.inputs.len()),
            move |reporter| assfonts::commands::run_process(options, reporter),
        ) {
            this.push_log(message);
        }
        cx.notify();
    });
}

/// 「测试」按钮：只做字体检查（需要哪些字体 / 缺少哪些字体 / 命中字体缺
/// 哪些字形），不写任何输出文件；结果全部流进日志页。
fn run_check_clicked(entity: &Entity<AssfontsApp>, cx: &mut App) {
    entity.update(cx, |this, cx| {
        if this.running {
            return;
        }
        let weak = cx.entity().downgrade();
        let options = match this.collect_options(cx) {
            Ok(options) => options,
            Err(message) => {
                this.push_log(format!("[错误] {message}"));
                return;
            }
        };
        // 字体检查只认 ASS/SSA；同一字幕在表格里出现多次时只查一遍。
        let mut seen = std::collections::BTreeSet::new();
        let subtitles: Vec<PathBuf> = this
            .process_rows
            .iter()
            .filter(|row| row.supports_embed())
            .map(|row| row.sub.clone())
            .filter(|sub| seen.insert(sub.clone()))
            .collect();
        if subtitles.is_empty() {
            this.push_log(
                "[提示] 字体检查只支持 .ass/.ssa 字幕，当前表格里没有可检查的字幕。".to_string(),
            );
            return;
        }
        this.persist(cx);
        if let Some(message) = this.spawn_task(
            weak,
            cx,
            format!("字体检查 {} 个字幕", subtitles.len()),
            move |reporter| {
                assfonts::commands::check_subtitles(&subtitles, &options, reporter).map(|_| ())
            },
        ) {
            this.push_log(message);
        }
        cx.notify();
    });
}

fn run_build_clicked(entity: &Entity<AssfontsApp>, cx: &mut App) {
    entity.update(cx, |this, cx| {
        if this.running {
            return;
        }
        let weak = cx.entity().downgrade();
        this.persist(cx);
        let fontpaths = split_paths(&this.input_value(&this.fontpaths_input, cx));
        if fontpaths.is_empty() {
            this.push_log("[错误] 请先在处理页填写字体目录".to_string());
            return;
        }
        let dbpath_value = this.input_value(&this.dbpath_input, cx);
        let output = if dbpath_value.trim().is_empty() {
            PathBuf::from(".")
        } else {
            PathBuf::from(dbpath_value.trim())
        };
        let options = BuildOptions { fontpaths, output };
        if let Some(message) =
            this.spawn_task(weak, cx, "重建字体索引".to_string(), move |reporter| {
                assfonts::commands::run_build(options, reporter)
            })
        {
            this.push_log(message);
        }
        cx.notify();
    });
}

impl Render for AssfontsApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.page {
            Page::Process => self.render_process(cx).into_any_element(),
            Page::Fonts => self.render_fonts(cx).into_any_element(),
            Page::Rename => self.render_rename(window, cx).into_any_element(),
            Page::Logs => self.render_logs(cx).into_any_element(),
            Page::Settings => self.render_settings(cx).into_any_element(),
        };

        v_flex()
            .size_full()
            .child(
                TitleBar::new().child(
                    div()
                        .px_2()
                        .text_size(px(12.5))
                        .text_color(muted_foreground(cx))
                        .child("assfonts"),
                ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_sidebar(cx))
                    // 重命名页与日志页自带内部滚动，整页不再滚动：外层滚动容器会
                    // 让内部滚动区拿不到确定高度（窗口缩放后只占页面一部分）。
                    .child(if matches!(self.page, Page::Rename | Page::Logs) {
                        v_flex()
                            .h_full()
                            .flex_1()
                            .min_w_0()
                            .child(content)
                            .into_any_element()
                    } else {
                        v_flex()
                            .id("page-scroll")
                            // h_flex 的交叉轴默认是 items_center：列不占行高，
                            // 滚动区域必须显式 h_full 才能以行高为视口（见 gpui-base StyledExt 文档）。
                            .h_full()
                            .flex_1()
                            .min_w_0()
                            .overflow_y_scroll()
                            .child(content)
                            .into_any_element()
                    }),
            )
    }
}

fn main() {
    let app = gpui_kit::application().with_assets(AllAssets);

    app.run(move |cx| {
        gpui_kit::init(cx);
        Theme::change(resolve_theme_mode(0, cx), None, cx);

        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1000.), px(680.)), cx)),
            ..TitleBar::window_options()
        };

        gpui_kit::open_window(window_options, cx, |window, cx| {
            cx.new(|cx| AssfontsApp::new(window, cx))
        })
        .expect("Failed to open window");
    });
}

#[cfg(test)]
mod tests {
    use super::missing_chars_range;

    #[test]
    fn missing_chars_range_marks_only_the_character_list() {
        let line =
            "  ✓ FZFW ZhuZi A Old Mincho（D:\\fonts\\FZFW.ttf#0）：缺 2 字（共 5 字）：'瀞' '𠮷'";
        let range = missing_chars_range(line).expect("missing chars should be highlighted");
        assert_eq!(&line[range], "'瀞' '𠮷'");
    }

    #[test]
    fn missing_chars_range_ignores_clean_and_font_only_lines() {
        let clean = "  ✓ Reed（D:\\fonts\\Reed.ttf#0）：31 字全部可用";
        let missing_font = "  ✗ 缺少字体（1）：Arial";
        let summary = "检查汇总：4 个字幕；字体请求 16 个；命中字体 14 个；缺少字体 1 个；缺字字体 1 个（共 1 字）。";
        assert_eq!(missing_chars_range(clean), None);
        assert_eq!(missing_chars_range(missing_font), None);
        assert_eq!(missing_chars_range(summary), None);
    }
}
