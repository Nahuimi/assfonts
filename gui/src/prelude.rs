//! 共享导入与 crate 内 re-export：所有模块统一 `use crate::prelude::*;`。

pub(crate) use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub(crate) use assfonts::{
    cli::{BuildOptions, MuxerChoice, OutputFormat, RunOptions},
    commands::ProgressReporter,
    mux::{
        self,
        job::{
            FansubGroup, RenameEntry, RenameNaming, RenameOptions, RenameRow, RenameStatus,
            RenameSummary, SuffixCategory, SuffixOverride, SuffixRule, TrackNameStyle,
            build_rename_rows, default_fansub_groups, default_suffix_categories,
        },
        scan::{
            DEFAULT_SUBTITLE_EXTENSIONS, DEFAULT_VIDEO_EXTENSIONS, ExtConfig, ScanResult,
            scan_paths,
        },
    },
};
pub(crate) use gpui_kit::{
    ExternalPaths,
    assets::{AllAssets, IconName},
    component::{
        ActiveTheme, TitleBar, WindowExt,
        button::{Button, ButtonVariants},
        input::{Input, InputEvent, InputState},
        menu::{ContextMenuExt, PopupMenu, PopupMenuItem},
        notification::Notification,
        progress::Progress,
        scroll::{Scrollbar, ScrollbarMode},
        select::{Select, SelectEvent, SelectState},
        switch::Switch,
        theme::{Theme, ThemeMode},
        *,
    },
    prelude::FluentBuilder as _,
    *,
};

// 本 crate 的模块统一经 prelude 进入作用域；各模块只需 `use crate::prelude::*;`。
pub(crate) use crate::app::*;
pub(crate) use crate::{
    pages::{fonts::*, process::*, rename::*},
    settings::*,
    sidebar::*,
    task::*,
    util::*,
    widgets::*,
};
