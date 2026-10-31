//! 自绘侧边栏、主题页脚与主题切换。

use crate::prelude::*;

pub(crate) const THEME_CHOICES: [&str; 3] = ["自动", "亮色", "暗色"];

pub(crate) fn sidebar_width(pinned: bool, hovered: bool) -> f32 {
    if pinned || hovered { 210. } else { 48. }
}

impl AssfontsApp {
    /// 自绘侧边栏：布局完全自控。固定（pinned）时始终展开；未固定时收起成
    /// 48px 图标条，鼠标悬停时以绝对定位浮层展开并盖住页面内容（不挤动布局）。
    pub(crate) fn render_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity();
        let overlay = !self.sidebar_pinned;
        let width = sidebar_width(self.sidebar_pinned, self.sidebar_hovered);
        let collapsed = width <= 48.;
        let sidebar_bg = cx.theme().sidebar;
        let sidebar_fg = cx.theme().sidebar_foreground;
        let sidebar_border = cx.theme().sidebar_border;
        let sidebar_accent = cx.theme().sidebar_accent;
        let sidebar_accent_fg = cx.theme().sidebar_accent_foreground;

        let make_toggle = || {
            let toggle_entity = entity.clone();
            Button::new("sidebar-toggle")
                .ghost()
                .icon(Icon::new(if self.sidebar_pinned {
                    IconName::Pin
                } else {
                    IconName::PinOff
                }))
                .tooltip(if self.sidebar_pinned {
                    "取消固定（鼠标移开自动收起）"
                } else {
                    "固定侧边栏"
                })
                .on_click(move |_, _, cx| {
                    toggle_entity.update(cx, |this, cx| {
                        this.sidebar_pinned = !this.sidebar_pinned;
                        // 刚取消固定时指针还停在侧边栏上：先保持展开，离开后再收起。
                        if !this.sidebar_pinned {
                            this.sidebar_hovered = true;
                        }
                        this.persist(cx);
                        cx.notify();
                    });
                })
        };

        // 标题行高度恒定：收起时仅固定按钮居中，展开时是标题 + 固定按钮。
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
                            if page == Page::Fonts {
                                // 字体页表格需要最新的 fonts.db 快照。
                                this.font_rows_dirty = true;
                            }
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

        let root = v_flex()
            .id("app-sidebar")
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
            .child(self.render_theme_footer(cx));

        // 未固定：脱离文档流，展开/收起不会挤动右侧页面。
        let root = if overlay {
            root.absolute().top_0().bottom_0().left_0()
        } else {
            root
        };

        // 鼠标进入 48px 图标条就展开；离开展开后的区域才收起（布局变化时
        // gpui 会重新触发 hover，所以来回移动不会卡在中间态）。
        let hover_entity = cx.entity().downgrade();
        root.on_hover(move |hovered, _, cx| {
            let hovered = *hovered;
            let _ = hover_entity.update(cx, |this, cx| {
                if this.sidebar_hovered != hovered {
                    this.sidebar_hovered = hovered;
                    cx.notify();
                }
            });
        })
        .into_any_element()
    }

    /// 侧边栏底部的主题切换：图标随当前生效模式变化，点击在 自动/亮色/暗色 间循环。
    pub(crate) fn render_theme_footer(&self, cx: &mut Context<Self>) -> Div {
        let entity = cx.entity();
        let collapsed = !(self.sidebar_pinned || self.sidebar_hovered);
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

    pub(crate) fn cycle_theme(&mut self, cx: &mut App) {
        self.theme_choice = (self.theme_choice + 1) % THEME_CHOICES.len();
        let mode = resolve_theme_mode(self.theme_choice, cx);
        Theme::change(mode, None, cx);
        self.persist(cx);
    }
}
