//! 应用状态 `AssfontsApp`：字段、生命周期、设置收集与窗口级 Render。

use crate::prelude::*;

/// 字体页表格的数据来源状态。
pub(crate) enum FontRowsState {
    /// 还没有请求过（第一次进入字体页时才读库）。
    Idle,
    Loading,
    Loaded,
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Page {
    Process,
    Fonts,
    Rename,
    Logs,
    Settings,
}

pub(crate) struct AssfontsApp {
    pub(crate) page: Page,
    /// 固定侧边栏：true 时始终展开；false 时收起成图标条、鼠标悬停展开。
    pub(crate) sidebar_pinned: bool,
    /// 鼠标是否停在侧边栏上（仅内存状态，不影响布局宽度，只影响浮层展开）。
    pub(crate) sidebar_hovered: bool,
    /// 0 = auto (system appearance), 1 = light, 2 = dark.
    pub(crate) theme_choice: usize,

    pub(crate) fontpaths_input: Entity<InputState>,
    /// 字体页「索引字体目录」：重建 fonts.db 时扫描的目录，只在本次运行有效。
    pub(crate) index_fontpaths_input: Entity<InputState>,
    pub(crate) dbpath_input: Entity<InputState>,
    pub(crate) output_input: Entity<InputState>,
    pub(crate) format_index: usize,

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
    pub(crate) muxer_index: usize,
    pub(crate) muxer_path_input: Entity<InputState>,
    pub(crate) ffprobe_path_input: Entity<InputState>,

    pub(crate) running: bool,
    pub(crate) logs: Vec<String>,
    /// 最近一次「保存日志」写出的文件路径（显示在日志页）。
    pub(crate) last_log_saved: Option<String>,
    /// Set by the 取消 button; checked by the library between units of work.
    pub(crate) cancel_flag: Arc<AtomicBool>,

    /// 视频/字幕扩展名只存在于 `gui.toml`（设置页不再提供输入框）。
    pub(crate) video_exts: String,
    pub(crate) sub_exts: String,
    /// 行内“自定义后缀”输入框：同一时间只编辑一行，复用一个输入。
    pub(crate) custom_suffix_input: Entity<InputState>,
    /// 正在编辑自定义后缀的那一行：`(0=字幕组 / 1=语言, 归类键)`。
    pub(crate) rename_edit_key: Option<(u8, String)>,
    pub(crate) rename_keep_in_place: bool,
    pub(crate) rename_copy_mode: bool,
    /// 最终文件名里是否插入字幕组缩写 / 语言后缀。
    pub(crate) fansub_suffix: bool,
    pub(crate) language_suffix: bool,
    /// 生效中的字幕组表与后缀分类表（来自 `gui.toml`）。
    pub(crate) fansub_groups: Vec<FansubGroup>,
    pub(crate) suffix_categories: Vec<SuffixCategory>,
    /// 右键菜单设置的覆盖项：归类键 → 字幕组/语言后缀。
    pub(crate) rename_overrides: std::collections::BTreeMap<String, SuffixOverride>,
    /// 处理页拖入的文件/文件夹（扫描源）。
    pub(crate) process_roots: Vec<PathBuf>,
    /// 处理页表格行：扫描出的字幕 + 配对视频 + 归类结果。
    pub(crate) process_rows: Vec<ProcessRow>,
    /// mks/mkv 轨道命名方式（设置页切换）。
    pub(crate) track_name_style: TrackNameStyle,
    /// 「设为默认字幕」的归类键（同一归类键在其它视频上同样生效）。
    pub(crate) default_track_key: Option<String>,
    /// Files and folders dropped on the 重命名 page; the scan source.
    pub(crate) rename_roots: Vec<PathBuf>,
    /// Latest scan of `rename_roots`, split into videos and subtitles.
    pub(crate) rename_scan: Option<ScanResult>,
    /// Rename table built from the scan plus the current rule/settings.
    pub(crate) rename_rows: Vec<RenameRow>,
    /// 重命名表格的三列宽度（像素），表头右缘的把手可以拖动调整。
    pub(crate) rename_cols: [f32; 3],
    /// 最后一列默认吃掉剩余宽度；拖过它的把手后改为固定宽度。
    pub(crate) rename_col_last_fixed: bool,
    /// 是否已手动改过列宽：没改过时列宽按窗口宽度自适应。
    pub(crate) rename_col_custom: bool,
    /// 正在拖动的列：(列下标, 按下时的窗口 x, 按下时的列宽)。
    pub(crate) rename_col_drag: Option<(usize, f32, f32)>,
    /// 处理页表格列宽：视频列、产物列（字幕列吃剩余宽度）。
    pub(crate) process_video_col: f32,
    pub(crate) process_product_col: f32,
    /// 是否手动拖过处理页列宽；没拖过时按窗口宽度自适应。
    pub(crate) process_col_custom: bool,
    /// 正在拖的列：0 = 视频列，1 = 产物列；附带按下位置与起始宽度。
    pub(crate) process_col_drag: Option<(u8, f32, f32)>,
    /// 表格的双向滚动位置；垂直/水平滚动条叠加在表格边缘。
    pub(crate) rename_scroll_x: ScrollHandle,
    pub(crate) rename_scroll_y: ScrollHandle,
    /// 日志页的纵向滚动位置；滚动条常驻日志框右缘。
    pub(crate) log_scroll_y: ScrollHandle,
    /// 字体页表格：fonts.db 快照、搜索框内容、显示上限与双向滚动位置。
    pub(crate) font_rows: Vec<assfonts::db::FontIndexRow>,
    pub(crate) font_rows_state: FontRowsState,
    /// 需要重新读取 fonts.db（换页、改 dbpath、重建索引完成后置位）。
    pub(crate) font_rows_dirty: bool,
    pub(crate) font_search_input: Entity<InputState>,
    /// 「显示前 N 条」下拉选择器；选中项变化时同步下面的下标。
    pub(crate) font_limit_select: Entity<SelectState<Vec<String>>>,
    /// `FONT_LIMITS` 的下标（与 `font_limit_select` 的当前值保持一致）。
    pub(crate) font_limit_index: usize,
    pub(crate) font_scroll_x: ScrollHandle,
    pub(crate) font_scroll_y: ScrollHandle,
    /// 字体表六列宽度；没手动拖过时按窗口宽度自适应。
    pub(crate) font_cols: [f32; 6],
    /// 是否手动拖过字体表列宽；拖过后改为固定宽度并显示横向滚动条。
    pub(crate) font_col_custom: bool,
    /// 正在拖动的字体表列：(列下标, 按下时的窗口 x, 按下时的列宽)。
    pub(crate) font_col_drag: Option<(usize, f32, f32)>,
    /// 重建索引正在跑；任务结束后自动刷新表格。
    pub(crate) rebuild_in_flight: bool,
    /// 当前后台任务的进度（处理/重建索引/应用重命名共用；同一时间只跑一个）。
    pub(crate) task_progress: Option<TaskProgress>,
    /// 等下一次 render 时推给通知层的完成提示（只有 render 里拿得到 `Window`）。
    pub(crate) pending_toast: Option<Notification>,
    /// 上一次复制模式操作写出的文件；空表示没有可撤销的复制。
    pub(crate) rename_undo: Vec<CopyRecord>,
    /// 保持输入框观察者的订阅存活（丢弃 Subscription 会取消订阅）。
    #[allow(dead_code)] // 只用于持有订阅，代码里不会读取
    pub(crate) input_subs: Vec<Subscription>,
    /// External tool availability shown in the title bar.
    pub(crate) tools_found: (bool, bool),
}

/// How a drop target adopts dropped file/folder paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DropMode {
    /// Append every dropped path to a `;`-separated multi-value input.
    Append,
    /// Replace the input with the first dropped path (single-value inputs).
    Replace,
}

impl Render for AssfontsApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 后台任务结束记下的提示，在这里补上窗口上下文推给通知层（toast）。
        if let Some(note) = self.pending_toast.take() {
            window.push_notification(note, cx);
        }
        let content = match self.page {
            Page::Process => self.render_process(window, cx).into_any_element(),
            Page::Fonts => self.render_fonts(window, cx).into_any_element(),
            Page::Rename => self.render_rename(window, cx).into_any_element(),
            Page::Logs => self.render_logs(cx).into_any_element(),
            Page::Settings => self.render_settings(cx).into_any_element(),
        };
        // 重命名页、日志页与字体页自带内部滚动，整页不再滚动：外层滚动容器会
        // 让内部滚动区拿不到确定高度（窗口缩放后只占页面一部分）。
        let page = if matches!(self.page, Page::Rename | Page::Logs | Page::Fonts) {
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
        };

        let pinned = self.sidebar_pinned;
        let mut row = h_flex().flex_1().min_h_0().relative();
        if pinned {
            row = row.child(self.render_sidebar(cx));
        } else {
            // 未固定时侧边栏是浮层：只占 48px 图标条的位置，悬停展开不挤动页面。
            row = row.child(div().w(px(48.)).flex_shrink_0().h_full());
        }
        row = row.child(page);
        if !pinned {
            // 浮层最后绘制，展开后才盖在页面内容之上。
            row = row.child(self.render_sidebar(cx));
        }

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
            .child(row)
    }
}

impl AssfontsApp {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
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
        let index_fontpaths_input = make_input("重建索引时扫描的字体目录，多个用 ; 分隔");
        let dbpath_input = make_input("fonts.db 所在的目录，留空用当前目录");
        let output_input = make_input("输出目录，默认 out");
        let custom_suffix_input = make_input("自定义后缀：输入什么就用什么，回车确认");
        let font_search_input = make_input("搜索 path / 名称 / 别名 / 字重");
        let muxer_path_input = make_input("封装器可执行文件路径，留空用 PATH");
        let ffprobe_path_input = make_input("ffprobe 路径（仅 ffmpeg 需要），留空用 PATH");

        // 「显示前 N 条」：用组件的下拉选择器（带边框的触发框，和右侧搜索框同一家族）。
        // 菜单宽度由组件按触发框宽度自适应，不需要手动对齐。
        let font_limit_select = cx.new(|cx| {
            let items: Vec<String> = FONT_LIMITS
                .iter()
                .map(|count| format!("{count} 条"))
                .collect();
            SelectState::new(items, Some(IndexPath::new(0)), window, cx)
        });

        // 处理页的字体目录只在本次运行有效，不从 gui.toml 恢复。
        apply_input(&dbpath_input, &settings.dbpath, window, cx);
        apply_input(&output_input, &settings.output, window, cx);
        apply_input(&muxer_path_input, &settings.muxer_path, window, cx);
        apply_input(&ffprobe_path_input, &settings.ffprobe_path, window, cx);

        // 行内自定义后缀：回车提交（空 = 清除覆盖，回到自动识别）。
        let mut input_subs = Vec::new();
        input_subs.push(cx.subscribe(
            &custom_suffix_input,
            |this: &mut Self, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.commit_custom_edit(cx);
                }
            },
        ));
        // 字体页搜索：输入变化后重绘，表格在 render 里按关键字过滤。
        input_subs.push(cx.subscribe(
            &font_search_input,
            |_this: &mut Self, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            },
        ));
        // 改 fonts.db 目录后重新读库（下一次进入字体页或当页渲染时生效）。
        input_subs.push(cx.subscribe(
            &dbpath_input,
            |this: &mut Self, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.font_rows_dirty = true;
                    cx.notify();
                }
            },
        ));
        // 「显示前 N 条」：确认选择后把「100 条」这类标题解析回 FONT_LIMITS 下标。
        input_subs.push(cx.subscribe(
            &font_limit_select,
            |this: &mut Self, _select, event: &SelectEvent<Vec<String>>, cx| {
                let SelectEvent::Confirm(value) = event;
                let index = value
                    .as_deref()
                    .and_then(|title| title.split_whitespace().next())
                    .and_then(|digits| digits.parse::<usize>().ok())
                    .and_then(|count| FONT_LIMITS.iter().position(|&limit| limit == count));
                if let Some(index) = index {
                    this.font_limit_index = index;
                    cx.notify();
                }
            },
        ));

        let app = Self {
            page: Page::Process,
            sidebar_pinned: settings.sidebar_pinned,
            sidebar_hovered: false,
            theme_choice: settings.theme_choice,
            fontpaths_input,
            index_fontpaths_input,
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
            process_col_custom: false,
            process_col_drag: None,
            rename_scroll_x: ScrollHandle::default(),
            rename_scroll_y: ScrollHandle::default(),
            log_scroll_y: ScrollHandle::default(),
            font_rows: Vec::new(),
            font_rows_state: FontRowsState::Idle,
            font_rows_dirty: false,
            font_search_input,
            font_limit_select,
            font_limit_index: 0,
            font_scroll_x: ScrollHandle::default(),
            font_scroll_y: ScrollHandle::default(),
            font_cols: FONT_COL_DEFAULTS,
            font_col_custom: false,
            font_col_drag: None,
            rebuild_in_flight: false,
            task_progress: None,
            pending_toast: None,
            rename_undo: Vec::new(),
            input_subs,
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
    pub(crate) fn persist(&self, cx: &App) {
        let settings = GuiSettings {
            theme_choice: self.theme_choice,
            sidebar_pinned: self.sidebar_pinned,
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

    pub(crate) fn push_log(&mut self, message: String) {
        self.logs.push(message);
        let excess = self.logs.len().saturating_sub(200);
        self.logs.drain(..excess);
        // 新日志总是追加在底部：请求下一次布局把视图滚到末尾。
        self.log_scroll_y.scroll_to_bottom();
    }

    pub(crate) fn input_value(&self, state: &Entity<InputState>, cx: &App) -> String {
        state.read(cx).value().to_string()
    }

    /// 字体页读取 fonts.db 的目录：输入框留空时用当前目录。
    pub(crate) fn font_dbpath(&self, cx: &App) -> PathBuf {
        let value = self.input_value(&self.dbpath_input, cx);
        let trimmed = value.trim();
        if trimmed.is_empty() {
            PathBuf::from(".")
        } else {
            PathBuf::from(trimmed)
        }
    }

    pub(crate) fn collect_options(&self, cx: &App) -> Result<RunOptions, String> {
        let format = format_output_format(self.format_index);
        // 只拖了视频的占位行不算输入。
        let subtitle_rows = self.process_rows.iter().filter(|row| !row.media_only);
        if subtitle_rows.clone().next().is_none() {
            return Err("请把字幕文件拖进表格".to_string());
        }
        // embed 只能写 ASS/SSA；其它格式在表格里已标为不可用，这里再兜底过滤。
        let inputs: Vec<PathBuf> = subtitle_rows
            .clone()
            .filter(|row| format != OutputFormat::Embed || row.supports_embed())
            .map(|row| row.sub.clone())
            .collect();
        if inputs.is_empty() {
            return Err(
                "内嵌模式只支持 .ass/.ssa 字幕；其它格式请改用“生成 mks 字幕”或“封装到 mkv”"
                    .to_string(),
            );
        }
        // mks 和 mkv 都要视频列表：按视频分组、用视频名给输出命名（mks 只是
        // 不把视频轨封进去）；只有 embed 模式不需要 --video。
        let videos: Vec<PathBuf> = if format != OutputFormat::Embed {
            self.process_rows
                .iter()
                .filter_map(|row| row.video.clone())
                .collect()
        } else {
            Vec::new()
        };
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

    pub(crate) fn muxer_choice(&self) -> MuxerChoice {
        if self.muxer_index % MUXERS.len() == 1 {
            MuxerChoice::Ffmpeg
        } else {
            MuxerChoice::Mkvmerge
        }
    }

    /// 记下一条完成提示，下一次 render 时交给通知层（那里才有 `Window`）。
    pub(crate) fn push_toast(&mut self, note: Notification) {
        self.pending_toast = Some(note);
    }

    /// 把文件管理器拖入的文件/文件夹（可多选）写入目标输入框。
    pub(crate) fn adopt_dropped_paths(
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
}
