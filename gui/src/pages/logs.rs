//! 日志页：逐行渲染、滚动跟随与保存日志。

use gpui_kit::prelude::FluentBuilder as _;

use crate::prelude::*;

impl AssfontsApp {
    /// 把日志写到程序目录下的 `assfonts-log-<UTC 时间>.log`。
    pub(crate) fn save_log(&mut self, cx: &mut Context<Self>) {
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

    /// 日志页：独立展示处理/封装/重命名日志，可清空或保存到程序目录。
    pub(crate) fn render_logs(&self, cx: &mut Context<Self>) -> Div {
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
}
