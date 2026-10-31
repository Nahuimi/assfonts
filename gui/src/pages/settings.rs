//! 设置页：分组设置卡片。

use crate::prelude::*;

impl AssfontsApp {
    pub(crate) fn render_settings(&self, cx: &mut Context<Self>) -> Div {
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
            TrackNameStyle::English => "英文",
            TrackNameStyle::Chinese => "中文",
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
                        h_flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .py_1p5()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
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
                            .gap_3()
                            .py_1p5()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_size(px(13.5)).child("字幕轨命名"))
                                    .child(
                                        div()
                                            .text_size(px(11.5))
                                            .text_color(muted_foreground(cx))
                                            .child(
                                                "mks/mkv 字幕轨标题用哪个后缀名；轨道语言标记按语言后缀自动设置",
                                            ),
                                    ),
                            )
                            .child(
                                Button::new("track-name-style")
                                    .label(track_label)
                                    .tooltip("英文：KTXP-TC；中文：极影字幕社-繁体（语言标记：简体→zh-Hans、繁体→zh-Hant、日语→jpn、英语→eng）")
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
                        "输出字幕写入对应视频所在目录；关闭则原地重命名（多目录时推荐开启）。复制模式下始终写入视频目录",
                        !self.rename_keep_in_place,
                        |this, value| this.rename_keep_in_place = !value,
                    ))
                    .child(row(
                        "rename-copy",
                        "复制而非移动",
                        "保留原字幕文件，在视频目录写入一份对齐后的副本",
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
