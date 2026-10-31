//! 字体页：fonts.db 快照、搜索/分组、六列宽度与重建索引。

use gpui_kit::prelude::FluentBuilder as _;

use crate::prelude::*;

/// 字体页表格每页最多显示多少个字体；选择框在 100/300/500/1000 之间切换。
pub(crate) const FONT_LIMITS: [usize; 4] = [100, 300, 500, 1000];

/// 「显示前 N 条」下拉选择器的宽度。
/// 100/300/500/1000 切换时宽度不变，后面的搜索框才不会左右跳动；菜单宽度
/// 默认跟随触发框（`Select` 的 `menu_width` 是 `Length::Auto`），左右边缘自然对齐。
pub(crate) const FONT_LIMIT_SELECT_W: f32 = 100.;

/// 字体表六列的标题。
pub(crate) const FONT_COLUMNS: [&str; 6] = [
    "path",
    "display_name",
    "normalized_name",
    "inferred_weight",
    "alias",
    "normalized_alias",
];

/// 手动拖过列宽后的默认像素宽度；没拖过时按窗口可用宽度自适应。
pub(crate) const FONT_COL_DEFAULTS: [f32; 6] = [220., 105., 115., 60., 110., 110.];

/// 手没拖过列宽时按内容自适应，列宽上限防止单列过宽。
pub(crate) const FONT_COL_MAX: f32 = 560.;

/// 字体表列宽下限。
pub(crate) const FONT_COL_MIN: f32 = 56.;

pub(crate) fn run_build_clicked(entity: &Entity<AssfontsApp>, cx: &mut App) {
    entity.update(cx, |this, cx| {
        if this.running {
            return;
        }
        let weak = cx.entity().downgrade();
        this.persist(cx);
        let fontpaths = split_paths(&this.input_value(&this.index_fontpaths_input, cx));
        if fontpaths.is_empty() {
            this.push_log("[错误] 请先在字体页填写索引字体目录".to_string());
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
            // 重建结束后字体页会重新读取 fonts.db。
            this.rebuild_in_flight = true;
        }
        cx.notify();
    });
}

/// 字体页的搜索匹配：不区分大小写，命中路径、名称、别名或字重任意一项即可。
pub(crate) fn font_row_matches(row: &assfonts::db::FontIndexRow, query_lower: &str) -> bool {
    if query_lower.is_empty() {
        return true;
    }
    let contains = |text: &str| text.to_lowercase().contains(query_lower);
    contains(&row.path)
        || contains(&row.display_name)
        || contains(&row.normalized_name)
        || row.inferred_weight.to_string().contains(query_lower)
        || row
            .aliases
            .iter()
            .any(|alias| contains(&alias.alias) || contains(&alias.normalized_alias))
}

/// 字体表按 path 合并后的一组：字体级字段只显示一次，alias 一行一个。
pub(crate) struct FontGroup<'a> {
    pub(crate) row: &'a assfonts::db::FontIndexRow,
    pub(crate) aliases: Vec<(&'a str, &'a str)>,
}

/// 按 path 合并匹配的字体行；不同 face 的 alias 归到同一组并去重。
/// `limit` 限制显示的字体组数（不是 alias 行数）。
pub(crate) fn group_font_rows<'a>(
    rows: &'a [assfonts::db::FontIndexRow],
    query_lower: &str,
    limit: usize,
) -> Vec<FontGroup<'a>> {
    let mut groups: Vec<FontGroup<'a>> = Vec::new();
    let mut paths: Vec<&'a str> = Vec::new();
    for row in rows {
        if !font_row_matches(row, query_lower) {
            continue;
        }
        let position = match paths.iter().position(|path| *path == row.path.as_str()) {
            Some(position) => position,
            None => {
                paths.push(row.path.as_str());
                groups.push(FontGroup {
                    row,
                    aliases: Vec::new(),
                });
                groups.len() - 1
            }
        };
        let group = &mut groups[position];
        for alias in &row.aliases {
            let duplicate = group.aliases.iter().any(|(text, normalized)| {
                *text == alias.alias.as_str() && *normalized == alias.normalized_alias.as_str()
            });
            if !duplicate {
                group
                    .aliases
                    .push((alias.alias.as_str(), alias.normalized_alias.as_str()));
            }
        }
    }
    groups.truncate(limit);
    groups
}

/// 按当前显示的数据计算字体表各列的自适应宽度。
pub(crate) fn font_column_widths(groups: &[FontGroup<'_>]) -> [f32; 6] {
    let mut widths = [FONT_COL_MIN; FONT_COLUMNS.len()];
    for (index, title) in FONT_COLUMNS.iter().enumerate() {
        widths[index] = widths[index].max(estimate_text_width(title));
    }
    for group in groups {
        let row = group.row;
        let path = if row.face_index == 0 {
            row.path.clone()
        } else {
            format!("{}#{}", row.path, row.face_index)
        };
        widths[0] = widths[0].max(estimate_text_width(&path));
        widths[1] = widths[1].max(estimate_text_width(&row.display_name));
        widths[2] = widths[2].max(estimate_text_width(&row.normalized_name));
        widths[3] = widths[3].max(estimate_text_width(&row.inferred_weight.to_string()));
        if group.aliases.is_empty() {
            widths[4] = widths[4].max(estimate_text_width("—"));
            widths[5] = widths[5].max(estimate_text_width("—"));
        }
        for (alias, normalized_alias) in &group.aliases {
            widths[4] = widths[4].max(estimate_text_width(alias));
            widths[5] = widths[5].max(estimate_text_width(normalized_alias));
        }
    }
    widths.map(|width| width.clamp(FONT_COL_MIN, FONT_COL_MAX))
}

impl AssfontsApp {
    /// 后台读取 fonts.db 的全部字体行；读完后更新表格状态并重绘。
    pub(crate) fn reload_font_index(&mut self, cx: &mut Context<Self>) {
        if matches!(self.font_rows_state, FontRowsState::Loading) {
            return;
        }
        self.font_rows_dirty = false;
        self.font_rows_state = FontRowsState::Loading;
        let dbpath = self.font_dbpath(cx);
        let (tx, rx) = smol::channel::unbounded::<
            std::result::Result<Vec<assfonts::db::FontIndexRow>, String>,
        >();
        std::thread::spawn(move || {
            let result = assfonts::db::load_font_index(&dbpath).map_err(|err| err.to_string());
            let _ = tx.send_blocking(result);
        });
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            if let Ok(result) = rx.recv().await {
                let _ = weak.update(cx, |this, cx| {
                    match result {
                        Ok(rows) => {
                            this.font_rows = rows;
                            this.font_rows_state = FontRowsState::Loaded;
                        }
                        Err(err) => {
                            this.font_rows = Vec::new();
                            this.font_rows_state = FontRowsState::Error(err);
                        }
                    }
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }

    /// 字体表列宽把手按下：自适应模式下先把当前屏幕宽度写回固定宽度再拖。
    pub(crate) fn start_font_col_drag(&mut self, column: usize, x: f32, columns: [f32; 6]) {
        if column < self.font_cols.len() {
            self.font_cols = columns;
            self.font_col_custom = true;
            self.font_col_drag = Some((column, x, self.font_cols[column]));
        }
    }

    /// 字体表列宽拖动；返回是否真的变了。
    pub(crate) fn drag_font_col(&mut self, x: f32) -> bool {
        let Some((column, start_x, start_width)) = self.font_col_drag else {
            return false;
        };
        let width = (start_width + (x - start_x)).max(FONT_COL_MIN);
        if (width - self.font_cols[column]).abs() < 0.5 {
            return false;
        }
        self.font_cols[column] = width;
        true
    }

    pub(crate) fn end_font_col_drag(&mut self) {
        self.font_col_drag = None;
    }

    pub(crate) fn render_fonts(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // 重建索引结束后自动刷新表格。
        if self.rebuild_in_flight && !self.running {
            self.rebuild_in_flight = false;
            self.font_rows_dirty = true;
        }
        if self.font_rows_dirty {
            self.reload_font_index(cx);
        }

        let entity = cx.entity();
        let muted = muted_foreground(cx);
        let border = card_border(cx);
        let accent = cx.theme().primary;
        let db_entity = entity.clone();
        let index_entity = entity.clone();
        let rebuild_entity = entity.clone();
        let db_state = self.dbpath_input.clone();
        let index_state = self.index_fontpaths_input.clone();

        // 搜索与显示上限：表格按 path 合并成字体组，只渲染过滤后的前 N 组。
        let query = self.input_value(&self.font_search_input, cx);
        let query_lower = query.trim().to_lowercase();
        let limit = FONT_LIMITS[self.font_limit_index % FONT_LIMITS.len()];
        let groups = group_font_rows(&self.font_rows, &query_lower, limit);
        let group_count = group_font_rows(&self.font_rows, &query_lower, usize::MAX).len();
        let shown = groups.len();

        // 没手动拖过列宽时按内容自适应（列宽随文字变宽、超长时省略号），
        // 拖过之后改为固定宽度并显示横向滚动条。
        // 可用宽度 = 窗口 - 侧边栏 - 页面左右内边距(32) - 卡片边框(2) - 纵向滚动条(12) - 余量(8)。
        let sidebar = if self.sidebar_pinned { 210. } else { 48. };
        let usable = (f32::from(window.viewport_size().width) - sidebar - 54.)
            .max(FONT_COL_MIN * FONT_COLUMNS.len() as f32);
        let columns = if self.font_col_custom {
            self.font_cols
        } else {
            let mut columns = font_column_widths(&groups);
            let total: f32 = columns.iter().sum();
            if total < usable {
                // 内容没占满时各列按比例放大，表格铺满可用宽度且列宽仍然均衡。
                let scale = usable / total;
                for width in &mut columns {
                    *width *= scale;
                }
            }
            columns
        };
        let table_width: f32 = columns.iter().sum();
        // 表头每列右缘一个 8px 拖拽把手；标题与内容都居中。
        let header_cell = |title: &'static str, column: usize| -> Div {
            let resize_entity = entity.clone();
            let mut cell = div()
                .w(px(columns[column]))
                .flex_none()
                .flex_shrink_0()
                .relative()
                .px_2()
                .py_2()
                .flex()
                .items_center()
                .justify_center()
                .when(column + 1 < FONT_COLUMNS.len(), |this| {
                    this.border_r_1().border_color(border)
                })
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_center()
                        .text_size(px(11.5))
                        .text_color(muted)
                        .child(title),
                );
            if column + 1 < FONT_COLUMNS.len() {
                cell = cell.child(
                    div()
                        .id(("font-col-handle", column))
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
                                this.start_font_col_drag(column, x, columns);
                                cx.notify();
                            });
                        }),
                );
            }
            cell
        };
        let header = h_flex()
            .w_full()
            .flex_none()
            .border_b_1()
            .border_color(border)
            .children(
                (0..FONT_COLUMNS.len()).map(|column| header_cell(FONT_COLUMNS[column], column)),
            );

        let cell_text = |text: Option<String>, column: usize| -> Div {
            div()
                .w(px(columns[column]))
                .flex_none()
                .flex_shrink_0()
                .px_2()
                .py_1()
                .text_size(px(12.))
                .when(column + 1 < FONT_COLUMNS.len(), |this| {
                    this.border_r_1().border_color(border)
                })
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_center()
                        .child(text.unwrap_or_default()),
                )
        };

        // 合并单元格：字体级字段只在组内显示一次，纵向撑满整组的高度（Excel 合并效果）。
        let font_cell = |text: Option<String>, column: usize| -> Div {
            div()
                .w(px(columns[column]))
                .flex_none()
                .flex_shrink_0()
                .px_2()
                .text_size(px(12.))
                .when(column + 1 < FONT_COLUMNS.len(), |this| {
                    this.border_r_1().border_color(border)
                })
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_center()
                        .child(text.unwrap_or_default()),
                )
        };

        let empty_hint = |text: String| -> AnyElement {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .p_4()
                .child(div().text_size(px(12.5)).text_color(muted).child(text))
                .into_any_element()
        };

        let rows_content: AnyElement = match &self.font_rows_state {
            FontRowsState::Idle | FontRowsState::Loading => {
                empty_hint("正在读取字体索引…".to_string())
            }
            FontRowsState::Error(err) => empty_hint(format!("读取 fonts.db 失败：{err}")),
            FontRowsState::Loaded if self.font_rows.is_empty() => empty_hint(
                "fonts.db 里还没有字体：在下方填好「索引字体目录」后点「重建索引」。".to_string(),
            ),
            FontRowsState::Loaded if group_count == 0 => {
                empty_hint(format!("没有匹配“{}”的字体。", query.trim()))
            }
            FontRowsState::Loaded => {
                let mut rows: Vec<AnyElement> = Vec::new();
                for (group_index, group) in groups.iter().enumerate() {
                    let row = group.row;
                    let path = if row.face_index == 0 {
                        row.path.clone()
                    } else {
                        format!("{}#{}", row.path, row.face_index)
                    };
                    // 同一个 path 合并成一组：字体级字段在组内合并为一个纵向撑满的单元格，
                    // 每个 alias 单独一行，占两列。
                    let aliases: Vec<(&str, &str)> = if group.aliases.is_empty() {
                        vec![("—", "—")]
                    } else {
                        group.aliases.clone()
                    };
                    let mut alias_cells: Vec<AnyElement> = Vec::new();
                    let mut normalized_alias_cells: Vec<AnyElement> = Vec::new();
                    for (alias, normalized_alias) in &aliases {
                        alias_cells
                            .push(cell_text(Some((*alias).to_string()), 4).into_any_element());
                        normalized_alias_cells.push(
                            cell_text(Some((*normalized_alias).to_string()), 5).into_any_element(),
                        );
                    }
                    rows.push(
                        h_flex()
                            .w_full()
                            .items_stretch()
                            .border_b_1()
                            .border_color(border)
                            .when(group_index % 2 == 1, |this| this.bg(muted.opacity(0.06)))
                            .child(font_cell(Some(path), 0))
                            .child(font_cell(Some(row.display_name.clone()), 1))
                            .child(font_cell(Some(row.normalized_name.clone()), 2))
                            .child(font_cell(Some(row.inferred_weight.to_string()), 3))
                            .child(v_flex().flex_none().w(px(columns[4])).children(alias_cells))
                            .child(
                                v_flex()
                                    .flex_none()
                                    .w(px(columns[5]))
                                    .children(normalized_alias_cells),
                            )
                            .into_any_element(),
                    );
                }
                v_flex().w_full().children(rows).into_any_element()
            }
        };

        // 横向滚动包住表头和行区，纵向滚动只作用于行区；两条滚动条常驻
        // 表格边缘，竖滚轮滚行区、Shift+滚轮横向滚动。
        let table_area = div()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .child(
                // 右下留出常驻滚动条的位置，滚动条画在外层，不遮住文字。
                div().size_full().pr(px(12.)).pb(px(12.)).child(
                    div()
                        .id("font-scroll-x")
                        .size_full()
                        .overflow_x_scroll()
                        .restrict_scroll_to_axis()
                        .track_scroll(&self.font_scroll_x)
                        .child(
                            v_flex()
                                .min_w(px(table_width))
                                .w_full()
                                .h_full()
                                .child(header)
                                .child(
                                    div().flex_1().min_h_0().child(
                                        div()
                                            .id("font-scroll-y")
                                            .size_full()
                                            .overflow_y_scroll()
                                            .restrict_scroll_to_axis()
                                            .track_scroll(&self.font_scroll_y)
                                            .child(rows_content),
                                    ),
                                ),
                        ),
                ),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom(px(12.))
                    .right_0()
                    .w(px(12.))
                    .child(
                        Scrollbar::vertical(&self.font_scroll_y)
                            .mode(ScrollbarMode::Always)
                            .viewport_from_layout(),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right(px(12.))
                    .bottom_0()
                    .h(px(12.))
                    .child(
                        Scrollbar::horizontal(&self.font_scroll_x)
                            .mode(ScrollbarMode::Always)
                            .viewport_from_layout(),
                    ),
            );

        let toolbar = h_flex()
            .flex_none()
            .items_center()
            .gap_2()
            .child(div().text_size(px(12.)).text_color(muted).child("显示前"))
            .child(
                // 下拉选择器：触发框带边框，和右侧搜索框同高同族；菜单宽度默认跟随
                // 触发框（`Length::Auto`），展开后左右边缘都和触发框对齐。
                div().w(px(FONT_LIMIT_SELECT_W)).child(
                    Select::new(&self.font_limit_select)
                        .id("font-limit")
                        .accessibility_label("显示前字体条数"),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.font_search_input)),
            )
            .child({
                let mut info = if query_lower.is_empty() {
                    format!("共 {group_count} 个字体")
                } else {
                    format!("匹配 {group_count} 个字体")
                };
                if group_count > shown {
                    info.push_str(&format!("；只显示前 {shown} 个字体"));
                }
                div().text_size(px(11.5)).text_color(muted).child(info)
            });

        let col_move = entity.clone();
        let col_up = entity.clone();
        let col_up_out = entity.clone();
        let table_card = div()
            .w_full()
            .rounded(px(10.))
            .border_1()
            .border_color(border)
            .bg(card_background(cx))
            .p_3()
            .flex_1()
            .min_h(px(220.))
            .flex()
            .flex_col()
            .gap_2()
            .on_mouse_move(move |event, _, cx| {
                let x = f32::from(event.position.x);
                col_move.update(cx, |this, cx| {
                    if this.drag_font_col(x) {
                        cx.notify();
                    }
                });
            })
            .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                col_up.update(cx, |this, _| this.end_font_col_drag());
            })
            .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                col_up_out.update(cx, |this, _| this.end_font_col_drag());
            })
            .child(toolbar)
            .child(table_area);

        v_flex()
            .id("fonts-page")
            .p_4()
            .gap_3()
            .h_full()
            .overflow_y_scroll()
            .child(Self::page_header(cx, "字体", "字体索引与字体库维护"))
            // 重建索引时的进度卡（和表格一起看得到进度）。
            .when_some(self.task_progress_card(cx), |this, card| this.child(card))
            .child(table_card)
            .child(Self::card(
                cx,
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(92.))
                                    .flex_none()
                                    .text_size(px(13.5))
                                    .child("fonts.db 目录"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .on_drop::<ExternalPaths>(move |files, window, cx| {
                                        db_entity.update(cx, |this, cx| {
                                            this.adopt_dropped_paths(
                                                &db_state,
                                                DropMode::Replace,
                                                &files.0,
                                                window,
                                                cx,
                                            );
                                            this.persist(cx);
                                        });
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
                            .w_full()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(92.))
                                    .flex_none()
                                    .text_size(px(13.5))
                                    .child("索引字体目录"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .on_drop::<ExternalPaths>(move |files, window, cx| {
                                        index_entity.update(cx, |this, cx| {
                                            this.adopt_dropped_paths(
                                                &index_state,
                                                DropMode::Append,
                                                &files.0,
                                                window,
                                                cx,
                                            );
                                        });
                                    })
                                    .drag_over::<ExternalPaths>(|style, _, _, cx| {
                                        style
                                            .border_1()
                                            .border_color(cx.theme().primary.opacity(0.7))
                                    })
                                    .child(Input::new(&self.index_fontpaths_input)),
                            ),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(92.))
                                    .flex_none()
                                    .text_size(px(13.5))
                                    .child("重建字体索引"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(11.5))
                                    .text_color(muted)
                                    .child("扫描上面的索引字体目录重建 fonts.db；输出到 fonts.db 所在目录"),
                            )
                            .child(Button::new("rebuild").primary().label("重建索引").on_click(
                                move |_, _, cx| {
                                    run_build_clicked(&rebuild_entity, cx);
                                },
                            )),
                    ),
            ))
            .into_any_element()
    }
}
