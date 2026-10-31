//! 跨页面复用的小部件：卡片/设置行、日志行、重命名表格行与右键菜单。

use crate::prelude::*;

/// 字幕单元格的右键菜单：字幕组后缀 / 语言后缀 / 设为默认字幕。
///
/// 覆盖项按归类键生效，所以给某条字幕设置后，其它视频上的同类字幕也会
/// 自动跟着变。
#[allow(clippy::too_many_arguments)]
pub(crate) fn subtitle_context_menu(
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

pub(crate) fn muted_foreground(cx: &App) -> Hsla {
    cx.theme().muted_foreground
}

/// 字体覆盖行里缺字列表的字节范围：`…字）：'a' 'b'` 中的 `'a' 'b'` 部分。
pub(crate) fn missing_chars_range(line: &str) -> Option<std::ops::Range<usize>> {
    if !line.contains("缺 ") || !line.contains(" 字（共 ") {
        return None;
    }
    let start = line.rfind("字）：")? + "字）：".len();
    (start < line.len()).then_some(start..line.len())
}

pub(crate) fn card_background(cx: &App) -> Hsla {
    cx.theme().background
}

pub(crate) fn card_border(cx: &App) -> Hsla {
    cx.theme().border
}

/// 表格行用到的主题色（集中传入，避免函数参数过多）。
#[derive(Clone, Copy)]
pub(crate) struct RenameRowColors {
    pub(crate) border: Hsla,
    pub(crate) muted: Hsla,
    pub(crate) foreground: Hsla,
    pub(crate) danger: Hsla,
    pub(crate) success: Hsla,
}

/// 渲染一行时共享的上下文（避免把一长串参数传给每一行）。
pub(crate) struct RenameRowContext<'a> {
    pub(crate) entity: &'a Entity<AssfontsApp>,
    pub(crate) columns: [f32; 3],
    pub(crate) style: RenameRowColors,
    pub(crate) groups: &'a [FansubGroup],
    pub(crate) categories: &'a [SuffixCategory],
    /// 当前「默认字幕」归类键，用于右键菜单勾选状态。
    pub(crate) default_key: Option<String>,
    /// 正在自定义后缀的行：`((0=字幕组 / 1=语言, 归类键), 输入框)`。
    pub(crate) edit: Option<(&'a (u8, String), &'a Entity<InputState>)>,
}

/// 字幕名后面的绿色标签：`澄空学园字幕组 简日双语`。
pub(crate) fn suffix_pill(text: String, colors: RenameRowColors) -> AnyElement {
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

pub(crate) fn rename_entry_row(
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

impl AssfontsApp {
    pub(crate) fn page_header(cx: &App, title: &'static str, desc: &'static str) -> Div {
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

    pub(crate) fn card(cx: &App, children: Div) -> Div {
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
    pub(crate) fn setting_row(
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
            .gap_3()
            .py_1p5()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
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

    /// 日志页的一行：缺少字体/读取失败整行标红；字体覆盖行只把缺字列表标红。
    pub(crate) fn log_line(
        cx: &App,
        line: &str,
        missing_chars: Option<std::ops::Range<usize>>,
    ) -> Div {
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
}
