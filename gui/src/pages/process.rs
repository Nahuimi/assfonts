//! 处理页：拖放扫描、表格行、列宽拖动与运行/检查入口。

use gpui_kit::prelude::FluentBuilder as _;

use crate::prelude::*;

/// 处理页表格列宽下限（视频列 / 产物列都可以拖宽）。
pub(crate) const PROCESS_COL_MIN: f32 = 80.;

/// 处理页表格列宽上限，避免拖到把字幕列挤没。
pub(crate) const PROCESS_COL_MAX: f32 = 620.;

pub(crate) const MUXERS: [&str; 2] = ["mkvmerge", "ffmpeg"];

pub(crate) const FORMATS: [&str; 3] = ["ass/ssa 内嵌字体", "mks 独立字幕", "mkv 封装视频"];

/// 处理页表格的一行：一条字幕（可能配有视频）+ 生效的字幕组/语言后缀。
#[derive(Debug, Clone)]
pub(crate) struct ProcessRow {
    pub(crate) video: Option<PathBuf>,
    pub(crate) sub: PathBuf,
    /// 只拖入视频、还没配对字幕时的占位行（不参与处理/检查）。
    pub(crate) media_only: bool,
    /// 归类键（分类名小写，未命中则注释小写）。
    pub(crate) key: String,
    /// 生效的字幕组缩写/自定义文本（决定文件名后缀）。
    pub(crate) fansub: Option<String>,
    /// 绿色标签里显示的字幕组中文名/自定义文本。
    pub(crate) fansub_name: Option<String>,
    /// 生效的语言后缀（SC/TCJP/自定义文本）。
    pub(crate) language: Option<String>,
    /// 绿色标签里显示的语言分类中文名/自定义文本。
    pub(crate) language_name: Option<String>,
}

impl ProcessRow {
    /// embed 模式只能把字体写进 ASS，其他字幕格式不支持。
    pub(crate) fn supports_embed(&self) -> bool {
        if self.media_only {
            return false;
        }
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

pub(crate) fn run_process_clicked(entity: &Entity<AssfontsApp>, cx: &mut App) {
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
pub(crate) fn run_check_clicked(entity: &Entity<AssfontsApp>, cx: &mut App) {
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

impl AssfontsApp {
    /// 当前重命名表格的统计。
    /// 处理页：记下拖入的文件/文件夹并重扫表格。
    pub(crate) fn drop_into_process(&mut self, files: &[PathBuf], cx: &mut Context<Self>) {
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

    pub(crate) fn clear_process(&mut self, cx: &mut Context<Self>) {
        self.process_roots.clear();
        self.process_rows.clear();
        self.push_log("[处理] 已清空".to_string());
        cx.notify();
    }

    /// 重扫处理页拖入的路径：视频/字幕分流、按文件名配对，并算好每行的
    /// 字幕组/语言后缀（右键覆盖项优先）。
    pub(crate) fn refresh_process(&mut self, cx: &mut Context<Self>) {
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
            if row.entries.is_empty() {
                if let Some(video) = row.video.clone() {
                    // 只拖了视频时也显示一行，等字幕拖进来再补成正常行。
                    flat.push(ProcessRow {
                        video: Some(video.clone()),
                        sub: video,
                        media_only: true,
                        key: String::new(),
                        fansub: None,
                        fansub_name: None,
                        language: None,
                        language_name: None,
                    });
                }
                continue;
            }
            for entry in row.entries {
                flat.push(ProcessRow {
                    video: row.video.clone(),
                    sub: entry.sub,
                    media_only: false,
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

    /// 预测这一行在当前输出格式下生成的产物文件名（与 `commands.rs` 的命名规则一致）。
    pub(crate) fn product_name(&self, row: &ProcessRow) -> (String, bool) {
        if row.media_only {
            return ("—".to_string(), false);
        }
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

    /// 右键「设为默认字幕」：按归类键记录，其它视频的同类字幕自动生效；
    /// 再点一次同一个键就取消默认。
    pub(crate) fn set_default_track(&mut self, key: String, cx: &mut Context<Self>) {
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

    /// 处理页列宽把手按下：which = 0 视频列、1 产物列。
    pub(crate) fn start_process_col_drag(&mut self, which: u8, x: f32) {
        let start = if which == 0 {
            self.process_video_col
        } else {
            self.process_product_col
        };
        self.process_col_custom = true;
        self.process_col_drag = Some((which, x, start));
    }

    /// 处理页列宽拖动：视频列在分隔线左侧（向右拖变宽），
    /// 产物列在分隔线右侧（向右拖变窄）；返回是否真的变了。
    pub(crate) fn drag_process_col(&mut self, x: f32) -> bool {
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

    pub(crate) fn end_process_col_drag(&mut self) {
        self.process_col_drag = None;
    }

    pub(crate) fn render_process(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
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
        // 没手动拖过列宽时，视频列/产物列按窗口可用宽度自适应；字幕列吃剩余宽度。
        let sidebar = if self.sidebar_pinned { 210. } else { 48. };
        let usable = (f32::from(window.viewport_size().width) - sidebar - 54.)
            .max(PROCESS_COL_MIN * 2. + 160.);
        let (video_col, product_col) = if self.process_col_custom {
            (self.process_video_col, self.process_product_col)
        } else {
            (
                (usable * 0.20).clamp(PROCESS_COL_MIN, PROCESS_COL_MAX),
                (usable * 0.16).clamp(PROCESS_COL_MIN, PROCESS_COL_MAX),
            )
        };
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
                // mks/mkv 每个输出文件会有一条默认字幕轨：没手动指定归类键时取
                // 组内第一条，指定了就按归类键命中；在字幕名前加星标。
                let default_star = (!embed
                    && !row.media_only
                    && match &self.default_track_key {
                        Some(key) => !key.is_empty() && row.key.eq_ignore_ascii_case(key),
                        None => offset == 0,
                    })
                .then(|| {
                    Icon::new(IconName::Star)
                        .xsmall()
                        .flex_none()
                        .text_color(cx.theme().warning)
                });
                let sub_cell = h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .children(default_star)
                    .child(
                        div()
                            .truncate()
                            .text_size(px(12.5))
                            .when(row.media_only, |this| this.text_color(muted))
                            .child(if row.media_only {
                                "—".to_string()
                            } else {
                                display_name(&row.sub)
                            }),
                    )
                    .children(pills);
                // 占位行没有字幕，不给字幕右键菜单。
                let sub_cell = if row.media_only {
                    sub_cell.into_any_element()
                } else {
                    sub_cell
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
                        })
                        .into_any_element()
                };
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
            // 任务进度卡：运行/测试/重建索引时显示标题、阶段与进度条。
            .when_some(self.task_progress_card(cx), |this, card| this.child(card))
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
}
