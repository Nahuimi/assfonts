//! assfonts-gui: gpui-kit based graphical front end for the assfonts library.
//!
//! Layout follows the FluxDown reference: collapsible sidebar navigation plus
//! grouped setting cards. The 处理 (process) page runs the library pipeline on
//! a background thread with log streaming; 设置 (settings) toggles feed the
//! `RunOptions` construction; 字体 (fonts) rebuilds `fonts.db`; 匹配 (match)
//! is still a placeholder.

// The GUI ships as a windowed application; no console host is needed.
#![windows_subsystem = "windows"]

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use assfonts::{
    cli::{BuildOptions, MuxerChoice, OutputFormat, RunOptions},
    commands::ProgressReporter,
    mux,
};
use gpui_kit::{
    ExternalPaths,
    assets::{AllAssets, IconName},
    component::{
        ActiveTheme, TitleBar,
        button::{Button, ButtonVariants},
        input::{Input, InputState},
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
    inputs: String,
    #[serde(default)]
    fontpaths: String,
    #[serde(default)]
    dbpath: String,
    #[serde(default)]
    output: String,
    #[serde(default)]
    videos: String,
    #[serde(default)]
    match_videos: String,
    #[serde(default)]
    match_subs: String,
    #[serde(default)]
    suffix_rule: String,
    #[serde(default)]
    match_copy_mode: bool,
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

fn load_settings() -> GuiSettings {
    std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

const LOG_LINES_SHOWN: usize = 12;
const MUXERS: [&str; 2] = ["mkvmerge", "ffmpeg"];
const FORMATS: [&str; 3] = ["ass/ssa 内嵌字体", "mks 独立字幕", "mkv 封装视频"];
const THEME_CHOICES: [&str; 3] = ["自动", "亮色", "暗色"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Process,
    Fonts,
    Match,
    Settings,
}

struct AssfontsApp {
    page: Page,
    sidebar_collapsed: bool,
    /// 0 = auto (system appearance), 1 = light, 2 = dark.
    theme_choice: usize,

    inputs_input: Entity<InputState>,
    fontpaths_input: Entity<InputState>,
    dbpath_input: Entity<InputState>,
    output_input: Entity<InputState>,
    videos_input: Entity<InputState>,
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
    /// Set by the 取消 button; checked by the library between units of work.
    cancel_flag: Arc<AtomicBool>,

    match_videos_input: Entity<InputState>,
    match_subs_input: Entity<InputState>,
    suffix_rule_input: Entity<InputState>,
    match_copy_mode: bool,
    match_result: Option<MatchOutcome>,
    /// External tool availability shown in the title bar.
    tools_found: (bool, bool),
}

/// One scan of the 匹配 page: media-subtitle pairing plus rename proposals.
#[derive(Debug, Clone)]
struct MatchOutcome {
    groups: Vec<(PathBuf, Vec<PathBuf>)>,
    unmatched: Vec<PathBuf>,
    proposals: Vec<(PathBuf, PathBuf)>,
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

/// Expand directories into files matching `extensions` (recursive); files pass through.
fn expand_paths(paths: Vec<PathBuf>, extensions: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in paths {
        if path.is_dir() {
            for entry in walkdir::WalkDir::new(&path)
                .into_iter()
                .filter_map(Result::ok)
            {
                if entry.file_type().is_file()
                    && entry
                        .path()
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .map(|ext| {
                            extensions
                                .iter()
                                .any(|allowed| ext.eq_ignore_ascii_case(allowed))
                        })
                        .unwrap_or(false)
                {
                    out.push(entry.path().to_path_buf());
                }
            }
        } else if path.is_file() {
            out.push(path);
        }
    }
    out.sort();
    out
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

        let inputs_input = make_input("字幕文件/目录，多个用 ; 分隔（必填）");
        let fontpaths_input = make_input("字体目录，留空则只用 fonts.db");
        let dbpath_input = make_input("fonts.db 所在目录，默认 .");
        let output_input = make_input("输出目录，默认 out");
        let videos_input = make_input("媒体文件/目录（mkv 模式，按文件名与字幕配对）");
        let match_videos_input = make_input("视频文件/目录，多个用 ; 分隔");
        let match_subs_input = make_input("字幕文件/目录（ass/ssa），多个用 ; 分隔");
        let suffix_rule_input = make_input(
            "后缀规则：留空保留原后缀；sc.ass 统一后缀；chs|sc.Haruhana;cht|tc.Haruhana 按注释映射",
        );
        let muxer_path_input = make_input("封装器可执行文件路径，留空用 PATH");
        let ffprobe_path_input = make_input("ffprobe 路径（仅 ffmpeg 需要），留空用 PATH");

        apply_input(&inputs_input, &settings.inputs, window, cx);
        apply_input(&fontpaths_input, &settings.fontpaths, window, cx);
        apply_input(&dbpath_input, &settings.dbpath, window, cx);
        apply_input(&output_input, &settings.output, window, cx);
        apply_input(&videos_input, &settings.videos, window, cx);
        apply_input(&match_videos_input, &settings.match_videos, window, cx);
        apply_input(&match_subs_input, &settings.match_subs, window, cx);
        apply_input(&suffix_rule_input, &settings.suffix_rule, window, cx);
        apply_input(&muxer_path_input, &settings.muxer_path, window, cx);
        apply_input(&ffprobe_path_input, &settings.ffprobe_path, window, cx);

        let app = Self {
            page: Page::Process,
            sidebar_collapsed: settings.sidebar_collapsed,
            theme_choice: settings.theme_choice,
            inputs_input,
            fontpaths_input,
            dbpath_input,
            output_input,
            videos_input,
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
            cancel_flag: Arc::new(AtomicBool::new(false)),
            match_videos_input,
            match_subs_input,
            suffix_rule_input,
            match_copy_mode: settings.match_copy_mode,
            match_result: None,
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
            inputs: self.input_value(&self.inputs_input, cx),
            fontpaths: self.input_value(&self.fontpaths_input, cx),
            dbpath: self.input_value(&self.dbpath_input, cx),
            output: self.input_value(&self.output_input, cx),
            videos: self.input_value(&self.videos_input, cx),
            match_videos: self.input_value(&self.match_videos_input, cx),
            match_subs: self.input_value(&self.match_subs_input, cx),
            suffix_rule: self.input_value(&self.suffix_rule_input, cx),
            match_copy_mode: self.match_copy_mode,
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
    }

    fn input_value(&self, state: &Entity<InputState>, cx: &App) -> String {
        state.read(cx).value().to_string()
    }

    fn collect_options(&self, cx: &App) -> Result<RunOptions, String> {
        let inputs = split_paths(&self.input_value(&self.inputs_input, cx));
        if inputs.is_empty() {
            return Err("请填写字幕输入路径".to_string());
        }

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
        let videos = split_paths(&self.input_value(&self.videos_input, cx));
        let videos = if videos.is_empty() {
            None
        } else {
            Some(videos)
        };

        if videos.is_some() && format != OutputFormat::Mkv {
            return Err("媒体路径仅在 mkv 模式有效，请切换格式或清空该输入".to_string());
        }

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

    fn log_text(&self) -> String {
        let start = self.logs.len().saturating_sub(LOG_LINES_SHOWN);
        self.logs[start..].join("\n")
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

    /// 匹配页：按当前输入扫描媒体-字幕配对并生成重命名预览。
    fn scan_match(&mut self, cx: &mut Context<Self>) {
        let videos = expand_paths(
            split_paths(&self.input_value(&self.match_videos_input, cx)),
            mux::MEDIA_EXTENSIONS,
        );
        let subs = expand_paths(
            split_paths(&self.input_value(&self.match_subs_input, cx)),
            &["ass", "ssa"],
        );

        if videos.is_empty() {
            self.push_log("[匹配] 未找到视频文件".to_string());
        }
        if subs.is_empty() {
            self.push_log("[匹配] 未找到字幕文件".to_string());
        }

        let (groups, unmatched) = mux::job::match_media_groups_by_order(&videos, &subs);
        self.persist(cx);
        let rule = mux::job::parse_suffix_rule(&self.input_value(&self.suffix_rule_input, cx));
        let proposals = mux::job::rename_proposals(&groups, &rule);
        self.match_result = Some(MatchOutcome {
            groups,
            unmatched,
            proposals,
        });
        cx.notify();
    }

    /// 匹配页混合拖放分流：文件/文件夹按扩展名自动分到视频或字幕输入
    /// （目录只取顶层文件，对齐 SubRenamer 的 TopDirectoryOnly 行为）。
    fn route_dropped_to_match(
        &mut self,
        files: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_video = |path: &PathBuf| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| {
                    mux::MEDIA_EXTENSIONS
                        .iter()
                        .any(|allowed| ext.eq_ignore_ascii_case(allowed))
                })
                .unwrap_or(false)
        };
        let is_sub = |path: &PathBuf| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| matches!(ext.to_ascii_lowercase().as_str(), "ass" | "ssa"))
                .unwrap_or(false)
        };

        let mut videos: Vec<PathBuf> = Vec::new();
        let mut subs: Vec<PathBuf> = Vec::new();
        for path in files {
            if path.is_file() {
                if is_video(path) {
                    videos.push(path.clone());
                } else if is_sub(path) {
                    subs.push(path.clone());
                }
            } else if path.is_dir() {
                if let Ok(entries) = std::fs::read_dir(path) {
                    for entry in entries.filter_map(Result::ok) {
                        let entry_path = entry.path();
                        if entry_path.is_file()
                            && let Some(file_name) = entry.file_name().to_str().map(str::to_string)
                        {
                            let as_path = PathBuf::from(file_name);
                            if is_video(&as_path) {
                                videos.push(entry_path);
                            } else if is_sub(&as_path) {
                                subs.push(entry_path);
                            }
                        }
                    }
                }
            }
        }

        let total = videos.len() + subs.len();
        if total == 0 {
            self.push_log("[匹配] 拖入内容中没有可识别的视频/字幕文件".to_string());
            cx.notify();
            return;
        }
        let videos_input = self.match_videos_input.clone();
        let subs_input = self.match_subs_input.clone();
        if !videos.is_empty() {
            self.adopt_dropped_paths(&videos_input, DropMode::Append, &videos, window, cx);
        }
        if !subs.is_empty() {
            self.adopt_dropped_paths(&subs_input, DropMode::Append, &subs, window, cx);
        }
        self.push_log(format!("[匹配] 已分流 {total} 个文件到视频/字幕输入"));
        self.persist(cx);
        cx.notify();
    }

    /// 应用当前预览的重命名（逐条 fs::rename，失败记录并继续），随后重扫。
    fn apply_renames(&mut self, cx: &mut Context<Self>) {
        let Some(outcome) = self.match_result.clone() else {
            return;
        };

        let mut renamed = 0usize;
        let copy_mode = self.match_copy_mode;
        for (from, to) in &outcome.proposals {
            if to.exists() {
                self.push_log(format!("[匹配] 目标已存在，跳过: {}", to.display()));
                continue;
            }
            let result = if copy_mode {
                std::fs::copy(from, to).map(|_| ())
            } else {
                std::fs::rename(from, to)
            };
            match result {
                Ok(()) => {
                    renamed += 1;
                    self.push_log(format!(
                        "[匹配] {}: {} -> {}",
                        if copy_mode { "复制" } else { "重命名" },
                        from.display(),
                        to.display()
                    ));
                }
                Err(err) => {
                    self.push_log(format!(
                        "[匹配] {}失败 {}: {err}",
                        if copy_mode { "复制" } else { "重命名" },
                        from.display()
                    ));
                }
            }
        }
        self.push_log(format!(
            "[匹配] 完成，共{} {renamed} 个文件",
            if copy_mode { "复制" } else { "重命名" }
        ));
        self.scan_match(cx);
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
            .child(nav_item("nav-match", IconName::Link, "匹配", Page::Match))
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
        let format_entity = entity.clone();
        let cancel_entity = entity.clone();
        let muxer_label = MUXERS[self.muxer_index % MUXERS.len()];
        let format_label = FORMATS[self.format_index % FORMATS.len()];
        let log_text = self.log_text();

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

        v_flex()
            .p_4()
            .gap_3()
            .size_full()
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
            .child(Self::card(
                cx,
                v_flex()
                    .gap_2()
                    .child(drop_input(
                        &entity,
                        &self.inputs_input,
                        DropMode::Append,
                        Input::new(&self.inputs_input).into_any_element(),
                    ))
                    .child(drop_input(
                        &entity,
                        &self.fontpaths_input,
                        DropMode::Append,
                        Input::new(&self.fontpaths_input).into_any_element(),
                    ))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().child(drop_input(
                                &entity,
                                &self.dbpath_input,
                                DropMode::Replace,
                                Input::new(&self.dbpath_input).into_any_element(),
                            )))
                            .child(div().flex_1().child(drop_input(
                                &entity,
                                &self.output_input,
                                DropMode::Replace,
                                Input::new(&self.output_input).into_any_element(),
                            ))),
                    )
                    .child(drop_input(
                        &entity,
                        &self.videos_input,
                        DropMode::Append,
                        Input::new(&self.videos_input).into_any_element(),
                    )),
            ))
            .child(
                Self::card(
                    cx,
                    v_flex().flex_1().min_h_0().child(
                        div()
                            .id("process-log")
                            .h_full()
                            .overflow_y_scroll()
                            .child(div().text_size(px(12.)).child(log_text)),
                    ),
                )
                .flex_1()
                .min_h(px(120.)),
            )
            // 操作栏固定在最下方：格式/封装器/运行。
            .child(Self::card(
                cx,
                v_flex().gap_1().child(
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
                        .child(
                            Button::new("muxer")
                                .label(muxer_label)
                                .on_click(move |_, _, cx| {
                                    muxer_entity.update(cx, |this, cx| {
                                        this.muxer_index = (this.muxer_index + 1) % MUXERS.len();
                                        this.persist(cx);
                                        cx.notify();
                                    });
                                }),
                        )
                        .child(div().flex_1())
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

    fn render_match(&self, cx: &mut Context<Self>) -> Div {
        let entity = cx.entity();

        let mut lines: Vec<String> = Vec::new();
        let mut proposal_count = 0usize;
        if let Some(outcome) = &self.match_result {
            for (video, subs) in &outcome.groups {
                let video_name = video
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let subs_text: Vec<String> = subs
                    .iter()
                    .map(|sub| {
                        sub.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default()
                    })
                    .collect();
                lines.push(format!("{video_name}  ⇐  {}", subs_text.join("  |  ")));
            }
            if !outcome.unmatched.is_empty() {
                lines.push(String::new());
                lines.push("未匹配字幕:".to_string());
                for sub in &outcome.unmatched {
                    let name = sub
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    lines.push(format!("  {name}"));
                }
            }
            if !outcome.proposals.is_empty() {
                lines.push(String::new());
                lines.push("重命名预览:".to_string());
                for (from, to) in &outcome.proposals {
                    proposal_count += 1;
                    let from_name = from
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let to_name = to
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    lines.push(format!("  {from_name}  →  {to_name}"));
                }
            }
            if lines.is_empty() {
                lines.push("没有找到媒体文件".to_string());
            }
        } else {
            lines.push("填写视频与字幕路径后点击“扫描匹配”".to_string());
        }

        let has_proposals = proposal_count > 0;
        let apply_entity = entity.clone();
        let copy_entity = entity.clone();
        let route_entity = entity.clone();

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

        v_flex()
            .p_4()
            .gap_3()
            .size_full()
            .child(Self::page_header(
                cx,
                "匹配",
                "媒体-字幕名称配对，重命名对齐预览与应用",
            ))
            .child(
                div()
                    .w_full()
                    .on_drop::<ExternalPaths>(move |files, window, cx| {
                        route_entity.update(cx, |this, cx| {
                            this.route_dropped_to_match(&files.0, window, cx);
                        });
                    })
                    .drag_over::<ExternalPaths>(|style, _, _, cx| {
                        style.border_1()
                            .border_color(cx.theme().primary.opacity(0.7))
                    })
                    .child(Self::card(
                        cx,
                        v_flex()
                            .gap_2()
                            .child(drop_input(
                                &entity,
                                &self.match_videos_input,
                                DropMode::Append,
                                Input::new(&self.match_videos_input).into_any_element(),
                            ))
                            .child(drop_input(
                                &entity,
                                &self.match_subs_input,
                                DropMode::Append,
                                Input::new(&self.match_subs_input).into_any_element(),
                            ))
                            .child(Input::new(&self.suffix_rule_input))
                            .child(
                                div().text_size(px(11.5)).text_color(muted_foreground(cx)).child(
                                    "后缀规则：留空保留原后缀；sc.ass 全部统一；chs|sc.Haruhana;cht|tc.Haruhana 按原注释映射（未命中保留）",
                                ),
                            ),
                    )),
            )
            .child(
                Self::card(
                    cx,
                    v_flex().flex_1().min_h_0().child(
                        div()
                            .id("match-log")
                            .h_full()
                            .overflow_y_scroll()
                            .child(div().text_size(px(12.)).child(lines.join("\n"))),
                    ),
                )
                .flex_1()
                .min_h(px(160.)),
            )
            .child(Self::card(
                cx,
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("scan-match")
                            .primary()
                            .label("扫描匹配")
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |this, cx| {
                                    this.scan_match(cx);
                                });
                            }),
                    )
                    .child(
                        Switch::new("match-copy")
                            .checked(self.match_copy_mode)
                            .label("复制而非移动")
                            .on_click(move |checked: &bool, _, cx| {
                                copy_entity.update(cx, |this, cx| {
                                    this.match_copy_mode = *checked;
                                    this.persist(cx);
                                    cx.notify();
                                });
                            }),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("apply-renames")
                            .label(if has_proposals {
                                format!("应用重命名（{proposal_count}）")
                            } else {
                                "应用重命名".to_string()
                            })
                            .on_click(move |_, _, cx| {
                                apply_entity.update(cx, |this, cx| {
                                    this.apply_renames(cx);
                                });
                            }),
                    ),
            ))
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
    }
}

fn muted_foreground(cx: &App) -> Hsla {
    cx.theme().muted_foreground
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.page {
            Page::Process => self.render_process(cx).into_any_element(),
            Page::Fonts => self.render_fonts(cx).into_any_element(),
            Page::Match => self.render_match(cx).into_any_element(),
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
                    .child(
                        v_flex()
                            .id("page-scroll")
                            // h_flex 的交叉轴默认是 items_center：列不占行高，
                            // 滚动区域必须显式 h_full 才能以行高为视口（见 gpui-base StyledExt 文档）。
                            .h_full()
                            .flex_1()
                            .min_w_0()
                            .overflow_y_scroll()
                            .child(content),
                    ),
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
