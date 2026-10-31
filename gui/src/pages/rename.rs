//! 重命名页：扫描配对、规则/覆盖项、复查、应用与撤销。

use gpui_kit::prelude::FluentBuilder as _;

use crate::prelude::*;

/// Rename table rows rendered at once; large folders stay responsive because
/// the page scroll is capped instead of building thousands of elements.
pub(crate) const RENAME_ROWS_SHOWN: usize = 600;

/// 重命名表格三列的初始像素宽度；表头右缘可拖拽调整，不持久化。
pub(crate) const RENAME_COL_DEFAULTS: [f32; 3] = [200., 230., 270.];

pub(crate) const RENAME_COL_MIN: f32 = 90.;

/// 「应用重命名」前的复查结果。
#[derive(Debug, Default)]
pub(crate) struct RenameReview {
    /// 可以执行的行：(源字幕, 目标路径)。
    pub(crate) queued: Vec<(PathBuf, PathBuf)>,
    /// 源文件已变化或消失、会跳过的行。
    pub(crate) stale: Vec<PathBuf>,
    /// 目标文件现在已存在（或与本批其他行重名）、会跳过的行。
    pub(crate) conflicts: Vec<PathBuf>,
    /// 复查过程中已占用的目标路径，用来发现批内重名。
    pub(crate) claimed: BTreeSet<PathBuf>,
}

impl AssfontsApp {
    /// Extension lists from `gui.toml` (empty lists fall back to the library
    /// defaults, so a broken config cannot disable recognition).
    pub(crate) fn ext_config(&self) -> ExtConfig {
        ExtConfig::new(split_exts(&self.video_exts), split_exts(&self.sub_exts))
    }

    pub(crate) fn rename_options(&self) -> RenameOptions {
        RenameOptions {
            // 复制模式产物就是给封装用的「视频目录下的新字幕」，所以总是放到
            // 视频旁边；「字幕原地」开关只影响直接重命名。
            move_to_video_dir: !self.rename_keep_in_place || self.rename_copy_mode,
            fansub_suffix: self.fansub_suffix,
            language_suffix: self.language_suffix,
        }
    }

    /// 右键“自定义…”：把该行切换成行内输入框，预填当前生效的后缀。
    pub(crate) fn begin_custom_edit(
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
    pub(crate) fn commit_custom_edit(&mut self, cx: &mut Context<Self>) {
        let Some((kind, key)) = self.rename_edit_key.take() else {
            return;
        };
        let value = self.input_value(&self.custom_suffix_input, cx);
        self.apply_override(kind, key, value, cx);
    }

    /// 右键菜单设置/清除某个归类键的字幕组或语言后缀，然后重建表格。
    /// 覆盖项按归类键生效，所以同类的其他字幕会自动跟着变。
    pub(crate) fn apply_override(
        &mut self,
        kind: u8,
        key: String,
        value: String,
        cx: &mut Context<Self>,
    ) {
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
    pub(crate) fn refresh_rename(&mut self, cx: &mut Context<Self>) {
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
    pub(crate) fn drop_into_rename(&mut self, files: &[PathBuf], cx: &mut Context<Self>) {
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

    pub(crate) fn clear_rename(&mut self, cx: &mut Context<Self>) {
        self.rename_roots.clear();
        self.rename_scan = None;
        self.rename_rows.clear();
        self.rename_edit_key = None;
        self.push_log("[重命名] 已清空".to_string());
        self.persist(cx);
        cx.notify();
    }

    pub(crate) fn rename_summary(&self) -> RenameSummary {
        RenameSummary::of(&self.rename_rows)
    }

    /// 表头把手按下：记录列号、按下位置和该列当前宽度。拖动最后一列后它从
    /// “吃掉剩余宽度”改为固定宽度，否则拖不动。
    pub(crate) fn start_column_drag(&mut self, column: usize, x: f32, columns: [f32; 3]) {
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
    pub(crate) fn drag_column(&mut self, x: f32) -> bool {
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

    pub(crate) fn end_column_drag(&mut self) {
        self.rename_col_drag = None;
    }

    /// 「应用重命名」：先复查（源文件是否还是扫描时的、目标是否被占用），
    /// 有变化就弹确认框点名跳过，再在后台线程逐行执行并推进度。
    pub(crate) fn apply_renames(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let review = self.review_renames();
        if review.queued.is_empty() {
            self.push_log("[重命名] 没有可执行的行".to_string());
            if !review.stale.is_empty() || !review.conflicts.is_empty() {
                self.push_log(format!(
                    "[重命名] 跳过 {} 个目标冲突、{} 个源文件已变化/消失的行",
                    review.conflicts.len(),
                    review.stale.len()
                ));
            }
            self.push_toast(Notification::warning("没有可执行的行").title("重命名"));
            cx.notify();
            return;
        }
        if review.conflicts.is_empty() && review.stale.is_empty() {
            self.start_apply_renames(review.queued, cx);
            return;
        }

        // 有冲突/源文件变化：点名列出，确认后只执行仍然有效的行。
        let mut lines: Vec<String> = Vec::new();
        if !review.conflicts.is_empty() {
            lines.push(format!(
                "{} 个目标文件已存在，会跳过：",
                review.conflicts.len()
            ));
            lines.extend(
                review
                    .conflicts
                    .iter()
                    .take(5)
                    .map(|path| format!("  · {}", display_name(path))),
            );
        }
        if !review.stale.is_empty() {
            lines.push(format!(
                "{} 个源文件已变化或消失，会跳过：",
                review.stale.len()
            ));
            lines.extend(
                review
                    .stale
                    .iter()
                    .take(5)
                    .map(|path| format!("  · {}", display_name(path))),
            );
        }
        let body = lines.join("\n");
        let queued = review.queued;
        let count = queued.len();
        let entity = cx.entity();
        window.open_alert_dialog(cx, move |dialog, _window, _cx| {
            let entity = entity.clone();
            let queued = queued.clone();
            dialog
                .title(format!("继续执行重命名（{count} 个）？"))
                .description(div().text_size(px(12.)).child(body.clone()))
                .show_cancel(true)
                .on_ok(move |_, _window, cx| {
                    entity.update(cx, |this, cx| {
                        this.start_apply_renames(queued.clone(), cx);
                    });
                    true
                })
        });
    }

    /// 点击「应用重命名」前的复查：源文件快照、目标占用、批内重名。
    pub(crate) fn review_renames(&self) -> RenameReview {
        let mut review = RenameReview::default();
        for row in &self.rename_rows {
            for entry in &row.entries {
                if entry.status != RenameStatus::Ready {
                    continue;
                }
                let unchanged = match entry.source {
                    Some(stamp) => stamp.still_matches(&entry.sub),
                    None => entry.sub.exists(),
                };
                if !unchanged {
                    review.stale.push(entry.sub.clone());
                    continue;
                }
                if entry.target.exists() || !review.claimed.insert(entry.target.clone()) {
                    review.conflicts.push(entry.target.clone());
                    continue;
                }
                review
                    .queued
                    .push((entry.sub.clone(), entry.target.clone()));
            }
        }
        review
    }

    /// 后台线程逐行重命名/复制，进度写进 `task_progress`，完成后弹提示。
    pub(crate) fn start_apply_renames(
        &mut self,
        queued: Vec<(PathBuf, PathBuf)>,
        cx: &mut Context<Self>,
    ) {
        if queued.is_empty() {
            return;
        }
        let total = queued.len();
        let stage = if self.rename_copy_mode {
            "复制"
        } else {
            "重命名"
        };
        self.logs.clear();
        self.running = true;
        self.task_progress = Some(TaskProgress {
            title: format!("应用重命名（{total}）"),
            stage: stage.to_string(),
            done: 0,
            total,
        });
        let copy_mode = self.rename_copy_mode;
        let (tx, rx) = smol::channel::unbounded::<TaskMessage>();
        std::thread::spawn(move || {
            let verb = if copy_mode { "复制" } else { "重命名" };
            let stage = if copy_mode { "复制" } else { "重命名" };
            let mut done = 0usize;
            let mut failed = 0usize;
            let mut undo: Vec<CopyRecord> = Vec::new();
            for (index, (sub, target)) in queued.iter().enumerate() {
                let result = if copy_mode {
                    std::fs::copy(sub, target).map(|_| ())
                } else {
                    std::fs::rename(sub, target)
                };
                match result {
                    Ok(()) => {
                        done += 1;
                        if copy_mode && let Some(record) = CopyRecord::of(target.clone()) {
                            undo.push(record);
                        }
                        let _ = tx.send_blocking(TaskMessage::Line(format!(
                            "[重命名] {verb}: {} → {}",
                            display_name(sub),
                            display_name(target)
                        )));
                    }
                    Err(err) => {
                        failed += 1;
                        let _ = tx.send_blocking(TaskMessage::Line(format!(
                            "[重命名] {verb}失败 {}: {err}",
                            display_name(sub)
                        )));
                    }
                }
                let _ = tx.send_blocking(TaskMessage::Progress {
                    stage: stage.to_string(),
                    done: index + 1,
                    total,
                });
            }
            let _ = tx.send_blocking(TaskMessage::RenameFinished { done, failed, undo });
        });

        cx.spawn(async move |weak, cx| {
            while let Ok(message) = rx.recv().await {
                match message {
                    TaskMessage::Line(line) => {
                        let _ = weak.update(cx, |this, cx| {
                            this.push_log(line);
                            cx.notify();
                        });
                    }
                    TaskMessage::Progress { stage, done, total } => {
                        let _ = weak.update(cx, |this, cx| {
                            if let Some(progress) = this.task_progress.as_mut() {
                                // `处理字幕` 走 par_iter，完成数可能乱序到达，取已见最大值。
                                let done = if progress.stage == stage {
                                    progress.done.max(done)
                                } else {
                                    done
                                };
                                progress.stage = stage;
                                progress.done = done;
                                progress.total = total;
                            }
                            cx.notify();
                        });
                    }
                    TaskMessage::RenameFinished { done, failed, undo } => {
                        let _ = weak.update(cx, |this, cx| {
                            this.finish_apply_renames(done, failed, undo, cx);
                        });
                        break;
                    }
                }
            }
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn finish_apply_renames(
        &mut self,
        done: usize,
        failed: usize,
        undo: Vec<CopyRecord>,
        cx: &mut Context<Self>,
    ) {
        self.running = false;
        self.task_progress = None;
        self.push_log(format!("[重命名] 完成：成功 {done} 个，失败 {failed} 个"));
        if self.rename_copy_mode && !undo.is_empty() {
            self.rename_undo = undo;
            let count = self.rename_undo.len();
            self.push_log(format!(
                "[重命名] 已记录 {count} 个复制出的文件，可用「撤销上次复制」删除"
            ));
        } else {
            self.rename_undo.clear();
        }
        let note = if failed == 0 {
            Notification::success(format!("成功 {done} 个")).title("重命名")
        } else {
            Notification::warning(format!("成功 {done} 个，失败 {failed} 个，详见日志"))
                .title("重命名")
        };
        self.push_toast(note);
        self.refresh_rename(cx);
        self.persist(cx);
    }

    /// 「撤销上次复制」：只删上次复制写出、且大小/mtime 没变过的文件。
    pub(crate) fn request_undo_copy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running || self.rename_undo.is_empty() {
            return;
        }
        let count = self.rename_undo.len();
        let lines: Vec<String> = self
            .rename_undo
            .iter()
            .take(5)
            .map(|record| format!("  · {}", display_name(&record.path)))
            .collect();
        let body = lines.join("\n");
        let entity = cx.entity();
        window.open_alert_dialog(cx, move |dialog, _window, _cx| {
            let entity = entity.clone();
            dialog
                .title(format!("撤销上次复制（{count} 个文件）？"))
                .description(div().text_size(px(12.)).child(format!(
                    "将删除这些复制出来的字幕，原字幕不受影响：\n{body}"
                )))
                .show_cancel(true)
                .on_ok(move |_, _window, cx| {
                    entity.update(cx, |this, cx| this.undo_last_copy(cx));
                    true
                })
        });
    }

    /// 删除上次复制写出的文件；大小/mtime 变过的保留不动。
    pub(crate) fn undo_last_copy(&mut self, cx: &mut Context<Self>) {
        let records = std::mem::take(&mut self.rename_undo);
        let mut removed = 0usize;
        let mut kept = 0usize;
        let mut failed = 0usize;
        for record in records {
            if !record.still_ours() {
                kept += 1;
                continue;
            }
            match std::fs::remove_file(&record.path) {
                Ok(()) => removed += 1,
                Err(err) => {
                    failed += 1;
                    self.push_log(format!(
                        "[重命名] 撤销失败 {}: {err}",
                        display_name(&record.path)
                    ));
                }
            }
        }
        if kept > 0 {
            self.push_log(format!("[重命名] {kept} 个文件已被修改或删除，保留不删"));
        }
        self.push_log(format!(
            "[重命名] 撤销完成：删除 {removed} 个，保留 {kept} 个，失败 {failed} 个"
        ));
        self.push_toast(
            Notification::success(format!("已删除 {removed} 个复制出的字幕")).title("撤销复制"),
        );
        self.refresh_rename(cx);
        cx.notify();
    }

    // -- rendering helpers -------------------------------------------------

    pub(crate) fn render_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
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
        // 可用宽度只算布局宽度：未固定时侧边栏是浮层，悬停展开不会改变页面宽度。
        let sidebar = if self.sidebar_pinned { 210. } else { 48. };
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
        let undo_entity = entity.clone();
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
                    // 复制模式留下的「撤销上次复制」只在有记录时出现。
                    .when(!self.rename_undo.is_empty(), |this| {
                        this.child(
                            Button::new("rename-undo-copy")
                                .ghost()
                                .label("撤销上次复制")
                                .on_click(move |_, window, cx| {
                                    undo_entity.update(cx, |this, cx| {
                                        this.request_undo_copy(window, cx);
                                    });
                                }),
                        )
                    })
                    .child(
                        Button::new("rename-apply")
                            .primary()
                            .label(apply_label)
                            .disabled(!can_apply)
                            .on_click(move |_, window, cx| {
                                apply_entity.update(cx, |this, cx| {
                                    this.apply_renames(window, cx);
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
            // 应用重命名/复制时的进度卡。
            .when_some(self.task_progress_card(cx), |this, card| this.child(card))
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
}
