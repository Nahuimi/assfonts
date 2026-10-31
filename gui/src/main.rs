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

mod app;
mod pages;
mod prelude;
mod settings;
mod sidebar;
mod task;
mod util;
mod widgets;

use crate::prelude::*;

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
    use crate::{
        pages::fonts::{font_row_matches, group_font_rows},
        sidebar::sidebar_width,
        task::{CopyRecord, TaskProgress},
        widgets::missing_chars_range,
    };

    #[test]
    fn task_progress_uses_real_counts_and_stays_indeterminate_without_a_total() {
        let known = TaskProgress {
            title: "处理".to_string(),
            stage: "处理字幕".to_string(),
            done: 3,
            total: 4,
        };
        assert_eq!(known.percent(), Some(75.0));
        assert_eq!(known.counters().as_deref(), Some("3/4"));

        let unknown = TaskProgress {
            title: "重建索引".to_string(),
            stage: "扫描字体目录".to_string(),
            done: 0,
            total: 0,
        };
        assert_eq!(unknown.percent(), None);
        assert_eq!(unknown.counters(), None);
    }

    #[test]
    fn copy_record_only_claims_files_left_untouched() {
        let dir = std::env::temp_dir().join(format!("assfonts-copy-record-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("sub.ass");
        std::fs::write(&path, b"[Script Info]\n").expect("write");

        let record = CopyRecord::of(path.clone()).expect("record");
        assert!(record.still_ours());

        // 大小变化后不再是「我们复制出来的那份」。
        std::fs::write(&path, b"[Script Info]\nTitle: changed\n").expect("rewrite");
        assert!(!record.still_ours());

        // 文件被删掉同样不再认领。
        std::fs::remove_file(&path).expect("remove");
        assert!(!record.still_ours());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn font_row_matches_path_names_aliases_and_weight() {
        let row = assfonts::db::FontIndexRow {
            path: "D:\\fonts\\Reed.ttf".to_string(),
            face_index: 0,
            display_name: "Reed".to_string(),
            normalized_name: "reed".to_string(),
            inferred_weight: 500,
            aliases: vec![assfonts::db::FontIndexAlias {
                alias: "Reed Bold".to_string(),
                normalized_alias: "reedbold".to_string(),
            }],
        };
        assert!(font_row_matches(&row, ""));
        assert!(font_row_matches(&row, "reed"));
        assert!(font_row_matches(&row, "bold"));
        assert!(font_row_matches(&row, "500"));
        assert!(font_row_matches(&row, "fonts"));
        assert!(!font_row_matches(&row, "missing"));
    }

    #[test]
    fn group_font_rows_merges_same_path_and_keeps_alias_per_row() {
        let row = |face_index: u32, display_name: &str, alias: &str| assfonts::db::FontIndexRow {
            path: "D:\\\\fonts\\\\Reed.ttf".to_string(),
            face_index,
            display_name: display_name.to_string(),
            normalized_name: display_name.to_lowercase(),
            inferred_weight: 500,
            aliases: vec![assfonts::db::FontIndexAlias {
                alias: alias.to_string(),
                normalized_alias: alias.to_lowercase().replace(' ', ""),
            }],
        };
        let rows = vec![
            row(0, "Reed", "Reed"),
            row(0, "Reed", "Reed Bold"),
            row(1, "Reed Italic", "Reed Italic"),
        ];
        let groups = group_font_rows(&rows, "", usize::MAX);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].aliases.len(), 3);
        assert_eq!(groups[0].aliases[1].0, "Reed Bold");

        let limited = group_font_rows(&rows, "", 1);
        assert_eq!(limited.len(), 1);
    }

    #[test]
    fn group_font_rows_filters_by_query_and_limit_counts_fonts() {
        let row = |path: &str, alias: &str| assfonts::db::FontIndexRow {
            path: path.to_string(),
            face_index: 0,
            display_name: path.to_string(),
            normalized_name: path.to_lowercase(),
            inferred_weight: 400,
            aliases: vec![assfonts::db::FontIndexAlias {
                alias: alias.to_string(),
                normalized_alias: alias.to_lowercase(),
            }],
        };
        let rows = vec![row("A.ttf", "Alpha"), row("B.ttf", "Beta")];
        let matched = group_font_rows(&rows, "beta", usize::MAX);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].row.path, "B.ttf");

        // limit 按字体组截断，而不是按 alias 行数。
        let limited = group_font_rows(&rows, "", 1);
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].row.path, "A.ttf");
    }

    #[test]
    fn sidebar_width_expands_when_pinned_or_hovered() {
        assert_eq!(sidebar_width(true, false), 210.);
        assert_eq!(sidebar_width(true, true), 210.);
        assert_eq!(sidebar_width(false, true), 210.);
        assert_eq!(sidebar_width(false, false), 48.);
    }

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
