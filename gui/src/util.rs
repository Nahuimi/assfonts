//! 不依赖界面状态的纯工具函数。

use crate::prelude::*;

/// Split a free-form extension list (`mkv, mp4` / `mkv;mp4` / `.mkv`) into
/// individual entries; blanks are dropped and `.` prefixes are tolerated by
/// `ExtConfig::new`.
pub(crate) fn split_exts(value: &str) -> Vec<String> {
    value
        .split([';', '；', ',', '，', ' ', '\t', '\n'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// Lossy file name for display; never empty.
pub(crate) fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

pub(crate) fn format_output_format(index: usize) -> OutputFormat {
    match index % FORMATS.len() {
        1 => OutputFormat::Mks,
        2 => OutputFormat::Mkv,
        _ => OutputFormat::Embed,
    }
}

pub(crate) fn split_paths(value: &str) -> Vec<PathBuf> {
    value
        .split([';', '；'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Resolve the auto theme choice from the OS appearance.
pub(crate) fn resolve_theme_mode(choice: usize, cx: &App) -> ThemeMode {
    match choice % THEME_CHOICES.len() {
        1 => ThemeMode::Light,
        2 => ThemeMode::Dark,
        _ => match cx.window_appearance() {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeMode::Dark,
            _ => ThemeMode::Light,
        },
    }
}

/// `SystemTime` → `YYYYMMDD-HHMMSS`（UTC），用于日志文件名。
pub(crate) fn utc_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// Howard Hinnant 的 civil-from-days：把 1970-01-01 起的天数换成年月日。
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    (y + i64::from(m <= 2), m as u32, d as u32)
}

/// 估算一段文字在 12px 字号下的像素宽度（ASCII 与 CJK 分开近似）。
pub(crate) fn estimate_text_width(text: &str) -> f32 {
    text.chars()
        .map(|character| if character.is_ascii() { 6.8 } else { 12.2 })
        .sum::<f32>()
        + 20.
}
