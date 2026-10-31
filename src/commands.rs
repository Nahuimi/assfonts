use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    hash::{Hash, Hasher},
    path::PathBuf,
};

use color_eyre::eyre::WrapErr;
use dashmap::DashMap;
use rayon::prelude::*;
use rusqlite::params;
use serde::Serialize;
use ttf_parser::Face;

use crate::{
    ass::{AssAnalysis, FontRequest, analyze_ass},
    bail,
    cli::{BuildOptions, MuxerChoice, OutputFormat, RunOptions},
    db,
    embed::{EmbeddedFont, extract_embedded_fonts, render_ass_with_fonts},
    error::{AssfontsError, Result},
    font::{
        FontRecord, discover_fonts, discover_from_file, extract_face_as_standalone_sfnt,
        match_best_font, normalize_font_name, supported_font_extension,
    },
    mux,
    rename::{self, RenameKey},
    subset,
};

#[derive(Debug, Serialize)]
struct RunReport {
    input: PathBuf,
    output_ass: Option<PathBuf>,
    missing_fonts: Vec<String>,
    missing_sample_index: Vec<usize>,
    error_index: Vec<usize>,
    glyph_coverage: Vec<GlyphCoverageReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mux: Option<MuxReport>,
}

#[derive(Debug, Clone, Serialize)]
struct MuxReport {
    output: PathBuf,
    muxer: String,
    exit_code: i32,
    media: Option<PathBuf>,
    subtitles: Vec<PathBuf>,
    attachments: Vec<PathBuf>,
}

#[derive(Debug, Serialize)]
struct GlyphCoverageReport {
    requested_font: String,
    requested_bold: i32,
    requested_italic: bool,
    resolved_font_file: PathBuf,
    required_count: usize,
    supported_count: usize,
    missing_sample: Vec<String>,
    original_bytes: usize,
    subset_bytes: usize,
    error: Option<String>,
}

/// Per-face result of a font check (`commands::check_subtitles`).
#[derive(Debug, Clone)]
pub struct CheckFontReport {
    pub display_name: String,
    pub path: PathBuf,
    pub face_index: u32,
    /// Font requests that resolved to this face, e.g. `Arial (bold=700, italic=false)`.
    pub requests: Vec<String>,
    pub required_count: usize,
    pub supported_count: usize,
    /// Characters this face cannot render (sorted).
    pub missing_chars: Vec<char>,
}

/// Aggregated result of [`check_subtitles`]: what each subtitle needs, which
/// fonts are missing, and which matched fonts lack glyphs.
#[derive(Debug, Default, Clone)]
pub struct CheckSummary {
    /// Number of `.ass`/`.ssa` files found in the inputs.
    pub subtitle_count: usize,
    /// Distinct font requests across all subtitles.
    pub font_request_count: usize,
    /// Distinct unresolved font requests, sorted.
    pub missing_fonts: Vec<String>,
    /// Per resolved font face, merged across all subtitles.
    pub font_reports: Vec<CheckFontReport>,
    /// Subtitle files or fonts that could not be read.
    pub error_count: usize,
}

impl CheckSummary {
    /// Total number of characters missing from matched fonts.
    pub fn missing_glyph_total(&self) -> usize {
        self.font_reports
            .iter()
            .map(|report| report.missing_chars.len())
            .sum()
    }

    /// Number of matched fonts that are missing at least one glyph.
    pub fn incomplete_font_count(&self) -> usize {
        self.font_reports
            .iter()
            .filter(|report| !report.missing_chars.is_empty())
            .count()
    }
}

/// Merge target for one resolved font face while walking subtitles.
struct CheckFontAggregate {
    record: FontRecord,
    requests: BTreeSet<String>,
    required_chars: HashSet<char>,
}

struct FontProcessResult {
    embedded_font: EmbeddedFont,
    coverage: Option<GlyphCoverageReport>,
}

type ResolvedFontKey = (PathBuf, u32);

/// Progress and cancellation surface for the run functions.
///
/// The CLI passes a no-op reporter; the GUI forwards messages to its log
/// panel and exposes a cancel button that flips the flag checked between
/// units of work (per input file / per mux group).
pub trait ProgressReporter: Send + Sync {
    /// Human-readable progress line.
    fn report(&self, message: String);
    /// Structured progress for the GUI progress card.
    ///
    /// `stage` names the step being worked on (shown above the bar) and
    /// `done`/`total` are item counts. `total == 0` means the total is not
    /// known yet, so the GUI shows an indeterminate bar. Reporters that only
    /// keep the log (CLI, tests) can rely on the no-op default.
    fn progress(&self, _stage: &str, _done: usize, _total: usize) {}
    /// Return true to abort the run at the next checkpoint.
    fn cancelled(&self) -> bool;
}

/// No-op reporter for the CLI.
pub struct SilentReporter;

impl ProgressReporter for SilentReporter {
    fn report(&self, _message: String) {}

    fn cancelled(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone)]
struct PendingFontProcess {
    request: FontRequest,
    normalized_name: String,
    record: FontRecord,
    required_chars: HashSet<char>,
}

struct FontProcessContext<'a> {
    font_bytes_cache: &'a DashMap<PathBuf, Vec<u8>>,
}

pub fn run_build(options: BuildOptions, reporter: &dyn ProgressReporter) -> Result<()> {
    reporter.report("扫描字体目录…".to_string());
    reporter.progress("扫描字体目录", 0, 0);
    validate_fontpaths(&options.fontpaths)?;

    fs::create_dir_all(&options.output)?;
    let mut conn = db::open_and_prepare_db(&options.output)?;

    // Collect all files to scan
    let mut found_files = Vec::new();
    for root in &options.fontpaths {
        for entry in walkdir::WalkDir::new(root)
            .follow_links(true)
            .into_iter()
            .filter_map(std::result::Result::ok)
        {
            let path = entry.path();
            if !path.is_file() || !supported_font_extension(path) {
                continue;
            }

            let abs_path = std::path::absolute(path)?;
            let metadata = fs::metadata(path)?;
            let size = metadata.len() as i64;
            let mtime = metadata
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;

            found_files.push((path.to_path_buf(), abs_path, size, mtime));
        }
    }

    let tx = conn.transaction()?;
    let font_total = found_files.len();
    reporter.progress("写入字体索引", 0, font_total);
    {
        let mut stmt_check =
            tx.prepare("SELECT mtime, file_size FROM fonts WHERE path = ?1 LIMIT 1")?;
        let mut stmt_del = tx.prepare("DELETE FROM fonts WHERE path = ?1")?;
        let mut stmt_ins_font = tx.prepare(
            "INSERT INTO fonts (path, face_index, display_name, normalized_name, inferred_weight, is_italic, mtime, file_size)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
        )?;
        let mut stmt_ins_alias = tx.prepare(
            "INSERT INTO aliases (path, face_index, alias, normalized_alias)
             VALUES (?1, ?2, ?3, ?4)",
        )?;

        let mut current_valid_paths = std::collections::HashSet::new();

        for (index, (raw_path, abs_path, size, mtime)) in found_files.into_iter().enumerate() {
            reporter.progress("写入字体索引", index, font_total);
            let abs_path_str = abs_path.to_string_lossy().to_string();
            current_valid_paths.insert(abs_path_str.clone());

            let mut rows = stmt_check.query(params![abs_path_str])?;
            let mut matches = false;
            if let Some(row) = rows.next()? {
                let db_mtime: i64 = row.get(0)?;
                let db_size: i64 = row.get(1)?;
                if db_mtime == mtime && db_size == size {
                    matches = true;
                }
            }

            if matches {
                continue;
            }

            // Delete existing records first (cascades to aliases)
            stmt_del.execute(params![abs_path_str])?;

            let mut records = discover_from_file(&raw_path);
            for r in &mut records {
                r.path = abs_path.clone();

                stmt_ins_font.execute(params![
                    abs_path_str,
                    r.face_index,
                    r.display_name,
                    r.normalized_name,
                    r.inferred_weight,
                    if r.is_italic { 1 } else { 0 },
                    mtime,
                    size
                ])?;

                for alias in &r.aliases {
                    stmt_ins_alias.execute(params![
                        abs_path_str,
                        r.face_index,
                        alias,
                        normalize_font_name(alias)
                    ])?;
                }
            }
        }

        // Clean up files no longer existing on disk
        let mut stmt_all_paths = tx.prepare("SELECT DISTINCT path FROM fonts")?;
        let mut rows = stmt_all_paths.query([])?;
        let mut paths_to_delete = Vec::new();
        while let Some(row) = rows.next()? {
            let db_path: String = row.get(0)?;
            if !current_valid_paths.contains(&db_path) {
                paths_to_delete.push(db_path);
            }
        }

        for path in paths_to_delete {
            stmt_del.execute(params![path])?;
        }
    }
    tx.commit()?;
    reporter.progress("写入字体索引", font_total, font_total);
    Ok(())
}

pub fn run_process(options: RunOptions, reporter: &dyn ProgressReporter) -> Result<()> {
    if options.videos.is_some() && options.format == OutputFormat::Embed {
        bail!("--video is only valid with --format mks or --format mkv");
    }

    match options.format {
        OutputFormat::Embed => run_process_embed(options, reporter),
        OutputFormat::Mks | OutputFormat::Mkv => run_process_mux(options, reporter),
    }
}

/// Font-only validation run: reads the inputs, resolves every font request,
/// and reports required fonts, unresolved fonts, and glyphs missing from the
/// matched fonts — without writing any output file.
///
/// The per-subtitle report goes through `reporter`; the aggregate is returned
/// for callers that need it (the GUI 「测试」 button only streams the report).
pub fn check_subtitles(
    inputs: &[PathBuf],
    options: &RunOptions,
    reporter: &dyn ProgressReporter,
) -> Result<CheckSummary> {
    let ass_files = collect_input_files(inputs, &["ass", "ssa"])?;
    if ass_files.is_empty() {
        reporter.report("没有可检查的 .ass/.ssa 字幕（字体检查只支持 ASS/SSA）。".to_string());
        return Ok(CheckSummary::default());
    }

    reporter.report(format!("待检查 {} 个字幕文件", ass_files.len()));
    let mut summary = CheckSummary {
        subtitle_count: ass_files.len(),
        ..Default::default()
    };

    // Pass 1: read and analyze every subtitle, collecting the font names the
    // font source has to resolve before matching can start.
    let mut loaded: Vec<(PathBuf, String)> = Vec::new();
    let mut requested_names: HashSet<String> = HashSet::new();
    let mut requests_all: BTreeSet<String> = BTreeSet::new();
    let read_total = ass_files.len();
    for (index, (input, _)) in ass_files.iter().enumerate() {
        reporter.progress("读取字幕", index, read_total);
        if reporter.cancelled() {
            bail!(AssfontsError::Cancelled);
        }
        match load_and_preprocess_ass(input) {
            Ok(content) => {
                for request in analyze_ass(&content).font_requests.keys() {
                    requested_names.insert(request.font_name.clone());
                    requests_all.insert(format_request(request));
                }
                loaded.push((input.clone(), content));
            }
            Err(err) => {
                summary.error_count += 1;
                reporter.report(format!("[错误] 读取 {} 失败：{err}", input.display()));
            }
        }
    }
    summary.font_request_count = requests_all.len();

    let contents: Vec<String> = loaded.iter().map(|(_, content)| content.clone()).collect();
    let (embedded_fonts, _embedded_guard) = collect_embedded_font_records(options, &contents)?;
    // A check run stays useful without a configured font source: treat the
    // source as empty so that every request shows up as missing.
    let fonts = match load_fonts_for_run(options, &requested_names, embedded_fonts) {
        Ok(fonts) => fonts,
        Err(AssfontsError::MissingFontSource(_)) => {
            reporter.report(
                "未找到字体来源（字体目录 / fonts.db / 内嵌字体），所有字体按缺失处理。"
                    .to_string(),
            );
            Vec::new()
        }
        Err(err) => return Err(err),
    };

    let font_bytes_cache: DashMap<PathBuf, Vec<u8>> = DashMap::new();
    let mut missing_fonts: BTreeSet<String> = BTreeSet::new();
    let mut aggregated: HashMap<ResolvedFontKey, CheckFontAggregate> = HashMap::new();
    let total = loaded.len();

    // Pass 2: per-subtitle report; coverage is measured against the real font
    // bytes and merged per face for the final summary.
    for (index, (input, content)) in loaded.iter().enumerate() {
        if reporter.cancelled() {
            bail!(AssfontsError::Cancelled);
        }
        reporter.report(format!(
            "[{}/{}] 检查 {}",
            index + 1,
            total,
            input.display()
        ));
        reporter.progress("检查字形", index, total);

        let analysis = analyze_ass(content);
        if analysis.font_requests.is_empty() {
            reporter.report("  未发现字体请求（不是 ASS/SSA 字幕或没有对白）。".to_string());
            continue;
        }

        let requests: Vec<String> = analysis.font_requests.keys().map(format_request).collect();
        reporter.report(format!(
            "  需要字体（{}）：{}",
            requests.len(),
            requests.join("、")
        ));

        let mut grouped: Vec<(FontRecord, BTreeSet<String>, HashSet<char>)> = Vec::new();
        let mut grouped_index: HashMap<ResolvedFontKey, usize> = HashMap::new();
        let mut missing_here: Vec<String> = Vec::new();
        for (request, required_chars) in &analysis.font_requests {
            match match_best_font(&fonts, request) {
                Some(record) => {
                    let key = make_resolved_key(record);
                    if let Some(&existing) = grouped_index.get(&key) {
                        grouped[existing].1.insert(format_request(request));
                        grouped[existing].2.extend(required_chars.iter().copied());
                    } else {
                        grouped_index.insert(key, grouped.len());
                        grouped.push((
                            record.clone(),
                            BTreeSet::from([format_request(request)]),
                            required_chars.clone(),
                        ));
                    }
                }
                None => missing_here.push(format_request(request)),
            }
        }

        if !missing_here.is_empty() {
            missing_fonts.extend(missing_here.iter().cloned());
            reporter.report(format!(
                "  ✗ 缺少字体（{}）：{}",
                missing_here.len(),
                missing_here.join("、")
            ));
        }

        for (record, request_names, required_chars) in grouped {
            let bytes = match get_or_load_font_bytes(&record.path, &font_bytes_cache) {
                Ok(bytes) => bytes,
                Err(err) => {
                    summary.error_count += 1;
                    reporter.report(format!(
                        "  [错误] 读取字体 {} 失败：{err}",
                        record.path.display()
                    ));
                    continue;
                }
            };
            let (required_count, _, mut missing_chars) =
                measure_font_coverage(&bytes, record.face_index, Some(&required_chars), usize::MAX);
            missing_chars.sort_unstable();
            report_font_coverage(reporter, &record, required_count, &missing_chars);

            let entry = aggregated
                .entry(make_resolved_key(&record))
                .or_insert_with(|| CheckFontAggregate {
                    record: record.clone(),
                    requests: BTreeSet::new(),
                    required_chars: HashSet::new(),
                });
            entry.requests.extend(request_names);
            entry.required_chars.extend(required_chars);
        }
    }

    for aggregate in aggregated.into_values() {
        let bytes = match get_or_load_font_bytes(&aggregate.record.path, &font_bytes_cache) {
            Ok(bytes) => bytes,
            // Already reported per subtitle above.
            Err(_) => continue,
        };
        let (required_count, supported_count, mut missing_chars) = measure_font_coverage(
            &bytes,
            aggregate.record.face_index,
            Some(&aggregate.required_chars),
            usize::MAX,
        );
        missing_chars.sort_unstable();
        summary.font_reports.push(CheckFontReport {
            display_name: aggregate.record.display_name.clone(),
            path: aggregate.record.path.clone(),
            face_index: aggregate.record.face_index,
            requests: aggregate.requests.into_iter().collect(),
            required_count,
            supported_count,
            missing_chars,
        });
    }
    summary.font_reports.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.face_index.cmp(&right.face_index))
    });
    summary.missing_fonts = missing_fonts.into_iter().collect();

    reporter.report(format!(
        "检查汇总：{} 个字幕；字体请求 {} 个；命中字体 {} 个；缺少字体 {} 个；缺字字体 {} 个（共 {} 字）。",
        summary.subtitle_count,
        summary.font_request_count,
        summary.font_reports.len(),
        summary.missing_fonts.len(),
        summary.incomplete_font_count(),
        summary.missing_glyph_total(),
    ));
    if summary.missing_fonts.is_empty() && summary.incomplete_font_count() == 0 {
        reporter.report("字体与字形全部齐备。".to_string());
    }
    if summary.error_count > 0 {
        reporter.report(format!(
            "有 {} 项读取失败，详见上方日志。",
            summary.error_count
        ));
    }

    Ok(summary)
}

/// One `✓ 字体（路径#face）…` log line for a matched face.
fn report_font_coverage(
    reporter: &dyn ProgressReporter,
    record: &FontRecord,
    required_count: usize,
    missing_chars: &[char],
) {
    let target = format!(
        "{}（{}#{}）",
        record.display_name,
        record.path.display(),
        record.face_index
    );
    if missing_chars.is_empty() {
        reporter.report(format!("  ✓ {target}：{required_count} 字全部可用"));
        return;
    }

    const LOG_CHAR_LIMIT: usize = 200;
    let shown: Vec<String> = missing_chars
        .iter()
        .take(LOG_CHAR_LIMIT)
        .map(|character| format!("{character:?}"))
        .collect();
    let more = if missing_chars.len() > LOG_CHAR_LIMIT {
        format!("…（仅列前 {LOG_CHAR_LIMIT} 字）")
    } else {
        String::new()
    };
    reporter.report(format!(
        "  ✓ {target}：缺 {} 字（共 {} 字）：{}{more}",
        missing_chars.len(),
        required_count,
        shown.join(" ")
    ));
}

fn run_process_embed(options: RunOptions, reporter: &dyn ProgressReporter) -> Result<()> {
    validate_output_dir(&options.output)?;
    if reporter.cancelled() {
        bail!(AssfontsError::Cancelled);
    }

    let ass_files = collect_input_files(&options.inputs, &["ass", "ssa"])?;
    reporter.report(format!("待处理 {} 个字幕文件", ass_files.len()));

    let mut requested_font_names = HashSet::new();
    let mut loaded_contents: Vec<String> = Vec::new();
    for (input, _) in &ass_files {
        if let Ok(content) = load_and_preprocess_ass(input) {
            for request in analyze_ass(&content).font_requests.keys() {
                requested_font_names.insert(request.font_name.clone());
            }
            loaded_contents.push(content);
        }
    }

    let (embedded_fonts, _embedded_guard) =
        collect_embedded_font_records(&options, &loaded_contents)?;
    let fonts = load_fonts_for_run(&options, &requested_font_names, embedded_fonts)?;
    let font_bytes_cache: DashMap<PathBuf, Vec<u8>> = DashMap::new();
    let total = ass_files.len();

    let reports: Vec<RunReport> = ass_files
        .par_iter()
        .enumerate()
        .map(|(index, (input, base_dir))| {
            if reporter.cancelled() {
                return Err(AssfontsError::Cancelled);
            }
            reporter.report(format!(
                "[{}/{}] 处理 {}",
                index + 1,
                total,
                input.display()
            ));
            reporter.progress("处理字幕", index + 1, total);
            process_single_input(
                input,
                base_dir,
                &options,
                &fonts,
                &font_bytes_cache,
                reporter,
            )
            .wrap_err(format!("ass path: {}", input.display()))
            .map_err(AssfontsError::from)
        })
        .collect::<Result<Vec<_>>>()?;

    if reporter.cancelled() {
        bail!(AssfontsError::Cancelled);
    }

    if options.report {
        let report_path = options.output.join("run-report.json");
        fs::write(report_path, serde_json::to_vec_pretty(&reports)?)?;
    }

    Ok(())
}

fn process_single_input(
    input: &std::path::Path,
    base_dir: &std::path::Path,
    options: &RunOptions,
    fonts: &[FontRecord],
    font_bytes_cache: &DashMap<PathBuf, Vec<u8>>,
    _reporter: &dyn ProgressReporter,
) -> Result<RunReport> {
    let processed_content = load_and_preprocess_ass(input)?;

    let analysis = analyze_ass(&processed_content);
    let mut embedded_fonts = Vec::new();
    let mut glyph_coverage = Vec::new();
    let (mut grouped, missing_fonts) = build_pending_processes(&analysis, fonts);
    for pending in &mut grouped {
        expand_compat_chars(&mut pending.required_chars, options.char_compatible);
    }

    if options.strict && !missing_fonts.is_empty() {
        bail!(AssfontsError::MissingFonts(
            input.to_path_buf(),
            missing_fonts
        ));
    }

    let context = FontProcessContext { font_bytes_cache };

    for pending in grouped {
        let processed = process_font_request(
            options,
            &pending.request,
            &pending.required_chars,
            &pending.normalized_name,
            &pending.record,
            &context,
        )?;

        embedded_fonts.push(processed.embedded_font);
        if let Some(coverage) = processed.coverage {
            glyph_coverage.push(coverage);
        }
    }

    let output_ass = write_output_ass(
        options,
        input,
        base_dir,
        &processed_content,
        &embedded_fonts,
    )?;

    let mut missing_sample_index = Vec::new();
    let mut error_index = Vec::new();
    for (i, coverage) in glyph_coverage.iter().enumerate() {
        if !coverage.missing_sample.is_empty() {
            missing_sample_index.push(i);
        }
        if coverage.error.is_some() {
            error_index.push(i);
        }
    }

    Ok(RunReport {
        input: input.to_path_buf(),
        output_ass,
        missing_fonts,
        missing_sample_index,
        error_index,
        glyph_coverage,
        mux: None,
    })
}

fn collect_input_files(inputs: &[PathBuf], extensions: &[&str]) -> Result<Vec<(PathBuf, PathBuf)>> {
    let matches = |path: &std::path::Path| -> bool {
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| {
                extensions
                    .iter()
                    .any(|allowed| ext.eq_ignore_ascii_case(allowed))
            })
            .unwrap_or(false)
    };

    let mut files = Vec::new();
    for input in inputs {
        if input.is_dir() {
            for entry in walkdir::WalkDir::new(input) {
                let entry =
                    entry.wrap_err(format!("failed to scan directory: {}", input.display()))?;
                if entry.file_type().is_file() && matches(entry.path()) {
                    files.push((entry.path().to_path_buf(), input.clone()));
                }
            }
        } else if input.is_file() && matches(input) {
            let base_dir = input.parent().map(|p| p.to_path_buf()).unwrap_or_default();
            files.push((input.clone(), base_dir));
        }
    }
    Ok(files)
}

fn run_process_mux(options: RunOptions, reporter: &dyn ProgressReporter) -> Result<()> {
    validate_output_dir(&options.output)?;
    if reporter.cancelled() {
        bail!(AssfontsError::Cancelled);
    }

    let input_files = collect_input_files(&options.inputs, &["ass", "ssa"])?;
    let sub_files: Vec<PathBuf> = input_files.into_iter().map(|(path, _)| path).collect();
    if sub_files.is_empty() {
        bail!("no ASS/SSA subtitle files found in the given inputs");
    }

    // mks 和 mkv 都按媒体文件分组：一个视频 + 多条字幕 → 一个输出文件
    // （mks 只是不把视频轨封进去，文件名同样用视频名）。没有视频输入时，
    // 每条字幕各自成一个输出。
    let groups: Vec<(Option<PathBuf>, Vec<PathBuf>)> = match options.format {
        OutputFormat::Mks | OutputFormat::Mkv => {
            let videos = match &options.videos {
                Some(video_inputs) => collect_media_files(video_inputs)?,
                None if options.format == OutputFormat::Mkv => {
                    bail!("--format mkv requires --video");
                }
                None => Vec::new(),
            };
            if videos.is_empty() {
                if options.format == OutputFormat::Mkv {
                    bail!("no media files found in --video inputs");
                }
                sub_files.iter().map(|s| (None, vec![s.clone()])).collect()
            } else {
                let (mut matched, mut unmatched) =
                    mux::job::match_media_groups(&videos, &sub_files);
                if matched.is_empty() {
                    // 字幕名和媒体名对不上时（例如 `01.chs_jpn.ass` 配
                    // `[组名] 标题 [01][...].mkv`），退回 SubRenamer 式按顺序配对。
                    let (by_order, order_unmatched) =
                        mux::job::match_media_groups_by_order(&videos, &sub_files);
                    matched = by_order;
                    unmatched = order_unmatched;
                }
                for sub in &unmatched {
                    reporter.report(format!("警告：字幕 \"{}\" 没有配到媒体文件", sub.display()));
                }
                if matched.is_empty() {
                    bail!("no subtitle matched any media file by name");
                }
                let mut groups: Vec<(Option<PathBuf>, Vec<PathBuf>)> = matched
                    .into_iter()
                    .map(|(video, subs)| (Some(video), subs))
                    .collect();
                if options.format == OutputFormat::Mks {
                    // mks 不丢弃任何字幕：没配到视频的各自封一个文件。
                    groups.extend(unmatched.into_iter().map(|sub| (None, vec![sub])));
                }
                groups
            }
        }
        OutputFormat::Embed => unreachable!("embed output is handled by run_process_embed"),
    };

    let mut requested_font_names = HashSet::new();
    let mut analyses: HashMap<PathBuf, AssAnalysis> = HashMap::new();
    let mut loaded_contents: Vec<String> = Vec::new();
    let analysis_total = sub_files.len();
    reporter.progress("分析字幕", 0, analysis_total);
    for (index, sub) in sub_files.iter().enumerate() {
        reporter.progress("分析字幕", index, analysis_total);
        let content = load_and_preprocess_ass(sub)?;
        let analysis = analyze_ass(&content);
        for request in analysis.font_requests.keys() {
            requested_font_names.insert(request.font_name.clone());
        }
        analyses.insert(sub.clone(), analysis);
        loaded_contents.push(content);
    }

    let (embedded_fonts, _embedded_guard) =
        collect_embedded_font_records(&options, &loaded_contents)?;
    let fonts = load_fonts_for_run(&options, &requested_font_names, embedded_fonts)?;
    let font_bytes_cache: DashMap<PathBuf, Vec<u8>> = DashMap::new();
    let context = FontProcessContext {
        font_bytes_cache: &font_bytes_cache,
    };
    let muxer = mux::create_muxer_with(
        muxer_kind(options.muxer),
        options.muxer_path.as_deref(),
        options.ffprobe_path.as_deref(),
    )?;
    let language_spec = options
        .language
        .as_deref()
        .map(mux::job::parse_language_spec)
        .unwrap_or_default();

    let mut reports: Vec<RunReport> = Vec::new();
    let group_total = groups.len();
    for (index, (media, subs)) in groups.iter().enumerate() {
        reporter.progress("封装", index, group_total);
        if reporter.cancelled() {
            bail!(AssfontsError::Cancelled);
        }
        let group_name = media
            .as_ref()
            .map(|m| m.display().to_string())
            .unwrap_or_else(|| subs[0].display().to_string());
        reporter.report(format!("封装 {}（{} 条字幕）", group_name, subs.len()));
        let group_reports = mux_group(
            media,
            subs,
            &analyses,
            &options,
            &fonts,
            &context,
            muxer.as_ref(),
            &language_spec,
            reporter,
        )?;
        reports.extend(group_reports);
    }

    if options.report {
        let report_path = options.output.join("run-report.json");
        fs::write(report_path, serde_json::to_vec_pretty(&reports)?)?;
    }

    Ok(())
}

fn muxer_kind(choice: MuxerChoice) -> mux::MuxerKind {
    match choice {
        MuxerChoice::Mkvmerge => mux::MuxerKind::Mkvmerge,
        MuxerChoice::Ffmpeg => mux::MuxerKind::Ffmpeg,
    }
}

fn collect_media_files(inputs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let input_files = collect_input_files(inputs, mux::MEDIA_EXTENSIONS)?;
    Ok(input_files.into_iter().map(|(path, _)| path).collect())
}

#[allow(clippy::too_many_arguments)]
fn mux_group(
    media: &Option<PathBuf>,
    subs: &[PathBuf],
    analyses: &HashMap<PathBuf, AssAnalysis>,
    options: &RunOptions,
    fonts: &[FontRecord],
    context: &FontProcessContext<'_>,
    muxer: &dyn mux::Muxer,
    language_spec: &mux::job::LanguageSpec,
    reporter: &dyn ProgressReporter,
) -> Result<Vec<RunReport>> {
    let mut per_sub_missing = Vec::with_capacity(subs.len());
    let mut pending_lists = Vec::with_capacity(subs.len());
    for sub in subs {
        let Some(analysis) = analyses.get(sub) else {
            bail!(AssfontsError::Font(format!(
                "missing analysis for {}",
                sub.display()
            )));
        };
        let (pending, missing) = build_pending_processes(analysis, fonts);
        if options.strict && !missing.is_empty() {
            bail!(AssfontsError::MissingFonts(sub.clone(), missing));
        }
        per_sub_missing.push(missing);
        pending_lists.push(pending);
    }
    let mut merged = merge_pending(pending_lists);
    for pending in &mut merged {
        expand_compat_chars(&mut pending.required_chars, options.char_compatible);
    }

    let group_stem = media
        .as_ref()
        .and_then(|m| m.file_stem())
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .or_else(|| {
            subs.first()
                .and_then(|s| s.file_stem())
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "output".to_string());

    let fonts_dir = options.output.join(format!("{group_stem}.fonts"));
    fs::create_dir_all(&fonts_dir)?;

    let mut attachments = Vec::new();
    let mut font_outputs: HashMap<ResolvedFontKey, (PathBuf, Option<String>)> = HashMap::new();
    let mut used_names = HashSet::new();
    for pending in &merged {
        if pending.required_chars.is_empty() {
            continue;
        }
        let original = get_or_load_font_bytes(&pending.record.path, context.font_bytes_cache)?;
        let mut bytes = match try_subset_font(
            &original,
            pending.record.face_index,
            Some(&pending.required_chars),
        ) {
            Ok(bytes) => bytes,
            Err(err) => {
                if options.strict && !options.allow_error_fonts {
                    return Err(AssfontsError::Other(
                        color_eyre::eyre::Report::new(err).wrap_err(format!(
                            "failed to subset font {} (face {})",
                            pending.record.path.display(),
                            pending.record.face_index
                        )),
                    ));
                }
                original.clone()
            }
        };

        let extension = mux::sfnt_extension(&bytes);
        let (crc, base) = if options.rename_fonts {
            let chars: String = pending.required_chars.iter().collect();
            let value = rename::compute_subset_crc(&chars, &pending.request.font_name);
            match rename::rename_font_names(&bytes, &value) {
                Ok(renamed_bytes) => {
                    bytes = renamed_bytes;
                    let sanitized: String = pending
                        .request
                        .font_name
                        .chars()
                        .map(|c| match c {
                            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
                            _ => c,
                        })
                        .collect();
                    let file_base = format!("{sanitized}.{value}");
                    (Some(value), file_base)
                }
                // Internal rename failed: keep the original name table and do
                // not rewrite the ASS, or references would break.
                Err(_) => (None, attachment_name_base(&pending.record, &mut used_names)),
            }
        } else {
            (None, attachment_name_base(&pending.record, &mut used_names))
        };

        let path = fonts_dir.join(format!("{base}.{extension}"));
        fs::write(&path, &bytes)?;
        attachments.push(path.clone());
        font_outputs.insert(make_resolved_key(&pending.record), (path, crc));
    }

    // 轨道命名（字幕组/语言后缀）与默认字幕：GUI 从 `gui.toml` 注入，
    // CLI 未注入时回落到内置表。
    let track_categories: Vec<mux::job::SuffixCategory> = if options.track_categories.is_empty() {
        mux::job::default_categories().to_vec()
    } else {
        options.track_categories.clone()
    };
    let track_groups: Vec<mux::job::FansubGroup> = if options.track_groups.is_empty() {
        mux::job::default_fansub_groups()
    } else {
        options.track_groups.clone()
    };
    let track_meta_options = mux::job::TrackMetaOptions {
        categories: &track_categories,
        groups: &track_groups,
        overrides: &options.track_overrides,
        style: options.track_name_style,
        fansub_enabled: options.track_fansub_suffix,
        language_enabled: options.track_language_suffix,
    };

    let mut tracks = Vec::with_capacity(subs.len());
    for (index, sub) in subs.iter().enumerate() {
        let sub_stem = sub
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("subtitle")
            .to_string();
        let comment = match media
            .as_ref()
            .and_then(|m| m.file_stem())
            .and_then(|s| s.to_str())
        {
            // 顺序配对时字幕名往往不带视频名前缀，此时退回字幕自身的注释，
            // 轨道命名（字幕组/语言后缀）才不会整段失效。
            Some(media_stem) => mux::job::subtitle_comment(&sub_stem, media_stem)
                .or_else(|| mux::job::standalone_comment(&sub_stem)),
            None => mux::job::standalone_comment(&sub_stem),
        };
        let meta = mux::job::subtitle_track_meta(comment.as_deref(), &track_meta_options);
        // 语言标记：显式 --language 优先；否则用分类推导（简体/简日/简英 →
        // zh-Hans，繁体/繁日/繁英 → zh-Hant，日语 → jpn，英语 → eng），最后
        // 回落到旧的注释映射。
        let language = if language_spec.is_explicit() {
            mux::job::language_for_comment(comment.as_deref(), language_spec)
        } else {
            meta.language
                .or_else(|| mux::job::language_for_comment(comment.as_deref(), language_spec))
        };
        let is_default = if options.default_track_keys.is_empty() {
            match &options.default_track {
                Some(wanted) => {
                    let wanted = wanted.trim().trim_start_matches('.').to_lowercase();
                    comment
                        .as_deref()
                        .map(|value| value.to_lowercase() == wanted)
                        .unwrap_or(false)
                }
                None => index == 0,
            }
        } else {
            let key = mux::job::naming_key(comment.as_deref(), &track_categories);
            options
                .default_track_keys
                .iter()
                .any(|wanted| wanted.eq_ignore_ascii_case(&key))
        };

        // With --rename-fonts, rewrite each subtitle's style names and \fn
        // tags to the CRC names and mux the rewritten copy.
        let track_path = if options.rename_fonts {
            let mut renames: HashMap<RenameKey, String> = HashMap::new();
            let mut subset_records: Vec<(String, String)> = Vec::new();
            if let Some(analysis) = analyses.get(sub) {
                for request in analysis.font_requests.keys() {
                    if let Some(record) = match_best_font(fonts, request)
                        && let Some((_, Some(new_name))) =
                            font_outputs.get(&make_resolved_key(record))
                    {
                        renames.insert(
                            RenameKey::new(
                                request.font_name.as_str(),
                                request.bold,
                                request.italic,
                            ),
                            new_name.clone(),
                        );
                        subset_records.push((new_name.clone(), request.font_name.clone()));
                    }
                }
            }
            if renames.is_empty() {
                sub.clone()
            } else {
                let content = load_and_preprocess_ass(sub)?;
                let renamed = rename::rename_ass_fonts(&content, &renames);
                let renamed = rename::write_subset_records(&renamed, &subset_records);
                let subset_path = fonts_dir.join(format!("{sub_stem}.subset.ass"));
                if options.utf8_sig {
                    let mut out = Vec::with_capacity(renamed.len() + 3);
                    out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
                    out.extend_from_slice(renamed.as_bytes());
                    fs::write(&subset_path, out)?;
                } else {
                    fs::write(&subset_path, renamed)?;
                }
                subset_path
            }
        } else {
            sub.clone()
        };

        tracks.push(mux::SubTrack {
            path: track_path,
            title: meta.title.or(comment),
            language,
            is_default,
        });
    }

    let media_stem = media.as_ref().and_then(|video| {
        video
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
    });
    let output_name = match (options.format, &media_stem) {
        (OutputFormat::Mkv, Some(stem)) => format!("{stem}.muxed.mkv"),
        (_, Some(stem)) => format!("{stem}.mks"),
        (_, None) => format!("{group_stem}.mks"),
    };
    let output = mux::job::resolve_output_path(options.output.join(output_name));
    let title = options.title.clone().or_else(|| {
        media
            .as_ref()
            .and_then(|m| m.file_stem())
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
    });

    let mut job = mux::MuxJob::new(output.clone());
    // mks 只封装字幕轨：视频只用于分组和命名，不写进输出。
    job.media = if options.format == OutputFormat::Mkv {
        media.clone()
    } else {
        None
    };
    job.subtitles = tracks;
    job.attachments = attachments;
    job.title = title;
    job.keep_subs = options.keep_subs;
    job.keep_attachments = options.keep_attachments;

    let result = muxer.mux(&job, &fonts_dir)?;

    if result.exit_code != 0 {
        let tail: Vec<&str> = result.log.lines().rev().take(15).collect();
        let warning = format!(
            "{} finished with warnings (exit code {}):\n{}",
            muxer.kind().as_str(),
            result.exit_code,
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        );
        eprintln!("{warning}");
        reporter.report(warning);
    }

    if !options.keep_temp {
        let _ = fs::remove_dir_all(&fonts_dir);
    }

    let mux_report = MuxReport {
        output: result.output,
        muxer: muxer.kind().as_str().to_string(),
        exit_code: result.exit_code,
        media: media.clone(),
        subtitles: subs.to_vec(),
        attachments: job.attachments,
    };

    Ok(subs
        .iter()
        .zip(per_sub_missing)
        .map(|(sub, missing)| RunReport {
            input: sub.clone(),
            output_ass: None,
            missing_fonts: missing,
            missing_sample_index: Vec::new(),
            error_index: Vec::new(),
            glyph_coverage: Vec::new(),
            mux: Some(mux_report.clone()),
        })
        .collect())
}

fn merge_pending(lists: Vec<Vec<PendingFontProcess>>) -> Vec<PendingFontProcess> {
    let mut grouped: Vec<PendingFontProcess> = Vec::new();
    let mut grouped_index: HashMap<ResolvedFontKey, usize> = HashMap::new();
    for list in lists {
        for pending in list {
            let key = make_resolved_key(&pending.record);
            upsert_pending_process(
                &mut grouped,
                &mut grouped_index,
                key,
                &pending.request,
                &pending.normalized_name,
                &pending.record,
                &pending.required_chars,
            );
        }
    }
    grouped
}

fn attachment_name_base(record: &FontRecord, used: &mut HashSet<String>) -> String {
    let base: String = record
        .display_name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect();
    let base = base.trim();
    let base = if base.is_empty() { "font" } else { base };

    let candidate = format!("{base}[{}]", record.face_index);
    if used.insert(candidate.clone()) {
        return candidate;
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    record.path.hash(&mut hasher);
    let hashed = format!("{base}[{}]-{:08x}", record.face_index, hasher.finish());
    used.insert(hashed.clone());
    hashed
}

fn load_and_preprocess_ass(input: &std::path::Path) -> Result<String> {
    let bytes =
        fs::read(input).wrap_err_with(|| format!("failed to read file: {}", input.display()))?;
    let decoded = decode_ass_bytes(&bytes)?;
    // Revert previous CRC renames (via `; Font Subset:` records) so font
    // matching sees the original names; fresh records are written on output.
    Ok(rename::apply_subset_records_reverse(&decoded))
}

/// Decode raw ASS/SSA bytes: UTF-8 (with or without BOM) first, then fall
/// back to chardetng detection (GBK/Big5/Shift-JIS etc. via encoding_rs).
fn decode_ass_bytes(bytes: &[u8]) -> Result<String> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        let text = String::from_utf8(bytes[3..].to_vec())
            .map_err(|_| AssfontsError::Font("invalid UTF-8 after BOM".to_string()))?;
        return Ok(text);
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(text.to_string());
    }

    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(bytes, true);
    let encoding = detector.guess(None, true);
    let (decoded, _, had_errors) = encoding.decode(bytes);
    if had_errors {
        eprintln!(
            "warning: decoded non-UTF-8 input as {} with replacement characters",
            encoding.name()
        );
    }
    Ok(decoded.into_owned())
}

/// Expand the required character set for higher renderer compatibility
/// (mirrors the Python tool's `char_compatible`): always keep ASCII letters,
/// and keep all digits as soon as any digit appears.
fn expand_compat_chars(chars: &mut HashSet<char>, enabled: bool) {
    if !enabled {
        return;
    }
    if chars.iter().any(|c| c.is_ascii_digit()) {
        chars.extend('0'..='9');
    }
    chars.extend('a'..='z');
    chars.extend('A'..='Z');
}

fn write_output_ass(
    options: &RunOptions,
    input: &std::path::Path,
    base_dir: &std::path::Path,
    processed_content: &str,
    embedded_fonts: &[EmbeddedFont],
) -> Result<Option<PathBuf>> {
    let path = output_ass_path(&options.output, input, base_dir);
    let rendered = render_ass_with_fonts(processed_content, embedded_fonts);

    if !options.force && fs::exists(&path)? {
        bail!(AssfontsError::FileExists(path));
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if options.utf8_sig {
        let mut out = Vec::with_capacity(rendered.len() + 3);
        out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        out.extend_from_slice(rendered.as_bytes());
        fs::write(&path, out)?;
    } else {
        fs::write(&path, rendered.as_bytes())?;
    }
    Ok(Some(path))
}

fn process_font_request(
    options: &RunOptions,
    request: &FontRequest,
    required_chars: &HashSet<char>,
    normalized_name: &str,
    record: &FontRecord,
    context: &FontProcessContext<'_>,
) -> Result<FontProcessResult> {
    let extension = record
        .path
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("font");

    let name_base = record
        .path
        .file_stem()
        .and_then(|x| x.to_str())
        .unwrap_or(normalized_name);

    let original_bytes = get_or_load_font_bytes(&record.path, context.font_bytes_cache)?;
    let subset_bytes = try_subset_font(&original_bytes, record.face_index, Some(required_chars));

    let embed_name = format!("{}[{}].{}", name_base, record.face_index, extension);

    let (coverage, subset_bytes) = build_coverage_report(
        options,
        request,
        record,
        required_chars,
        &original_bytes,
        subset_bytes,
    )?;

    Ok(FontProcessResult {
        embedded_font: EmbeddedFont {
            font_name: embed_name,
            bytes: subset_bytes,
        },
        coverage,
    })
}

fn build_coverage_report(
    options: &RunOptions,
    request: &FontRequest,
    record: &FontRecord,
    required_chars: &HashSet<char>,
    original_bytes: &[u8],
    subset_bytes: Result<Vec<u8>>,
) -> Result<(Option<GlyphCoverageReport>, Vec<u8>)> {
    let (subset_bytes, error) = match subset_bytes {
        Ok(bytes) => (bytes, None),
        Err(err) => {
            if options.strict && !options.allow_error_fonts {
                return Err(err);
            }
            (original_bytes.to_vec(), Some(err))
        }
    };

    if !options.strict && !options.report {
        return Ok((None, subset_bytes));
    }

    let coverage_face_index = record.face_index;
    let (required_count, supported_count, missing_sample) =
        measure_font_coverage(&subset_bytes, coverage_face_index, Some(required_chars), 12);
    let missing_sample: Vec<String> = missing_sample
        .into_iter()
        .map(|character| character.to_string())
        .collect();

    if (options.strict && !options.allow_missing_sample) && !missing_sample.is_empty() {
        return Err(AssfontsError::Font(format!(
            "Font '{}' is missing required characters: {:?}",
            record.display_name, missing_sample
        )));
    }

    let coverage = match options.report {
        true => Some(GlyphCoverageReport {
            requested_font: request.font_name.clone(),
            requested_bold: request.bold,
            requested_italic: request.italic,
            resolved_font_file: record.path.clone(),
            required_count,
            supported_count,
            missing_sample,
            original_bytes: original_bytes.len(),
            subset_bytes: subset_bytes.len(),
            error: error.map(|e| e.to_string()),
        }),
        false => None,
    };

    Ok((coverage, subset_bytes))
}

fn make_resolved_key(record: &FontRecord) -> ResolvedFontKey {
    (record.path.clone(), record.face_index)
}

fn build_pending_processes(
    analysis: &AssAnalysis,
    fonts: &[FontRecord],
) -> (Vec<PendingFontProcess>, Vec<String>) {
    let mut grouped: Vec<PendingFontProcess> = Vec::new();
    let mut grouped_index: HashMap<ResolvedFontKey, usize> = HashMap::new();
    let mut missing_fonts = Vec::new();

    for (request, required_chars) in &analysis.font_requests {
        let normalized = normalize_font_name(&request.font_name);
        if let Some(record) = match_best_font(fonts, request) {
            let resolved_key = make_resolved_key(record);

            upsert_pending_process(
                &mut grouped,
                &mut grouped_index,
                resolved_key,
                request,
                &normalized,
                record,
                required_chars,
            );
        } else {
            missing_fonts.push(format_request(request));
        }
    }

    (grouped, missing_fonts)
}

fn upsert_pending_process(
    grouped: &mut Vec<PendingFontProcess>,
    grouped_index: &mut HashMap<ResolvedFontKey, usize>,
    resolved_key: ResolvedFontKey,
    request: &FontRequest,
    normalized_name: &str,
    record: &FontRecord,
    required_chars: &HashSet<char>,
) {
    if let Some(existing_index) = grouped_index.get(&resolved_key).copied() {
        grouped[existing_index]
            .required_chars
            .extend(required_chars.iter());
        return;
    }

    let index = grouped.len();
    grouped.push(PendingFontProcess {
        request: request.clone(),
        normalized_name: normalized_name.to_string(),
        record: record.clone(),
        required_chars: required_chars.clone(),
    });
    grouped_index.insert(resolved_key, index);
}

fn get_or_load_font_bytes(
    path: &std::path::Path,
    cache: &DashMap<PathBuf, Vec<u8>>,
) -> Result<Vec<u8>> {
    if let Some(cached) = cache.get(path) {
        return Ok(cached.clone());
    }

    let loaded = fs::read(path)?;
    cache.insert(path.to_path_buf(), loaded.clone());
    Ok(loaded)
}

fn measure_font_coverage(
    bytes: &[u8],
    face_index: u32,
    required_chars: Option<&HashSet<char>>,
    sample_limit: usize,
) -> (usize, usize, Vec<char>) {
    let Some(required_chars) = required_chars else {
        return (0, 0, Vec::new());
    };

    let mut supported_count = 0usize;
    let mut missing_sample = Vec::new();

    let face = Face::parse(bytes, face_index).ok();
    for character in required_chars {
        let supported = face
            .as_ref()
            .and_then(|f| f.glyph_index(*character))
            .is_some();

        if supported {
            supported_count += 1;
        } else if missing_sample.len() < sample_limit {
            missing_sample.push(*character);
        }
    }

    (required_chars.len(), supported_count, missing_sample)
}

fn try_subset_font(
    original_bytes: &[u8],
    face_index: u32,
    required_chars: Option<&HashSet<char>>,
) -> Result<Vec<u8>> {
    let Some(required_chars) = required_chars else {
        return Ok(Vec::new());
    };

    if required_chars.is_empty() {
        return Ok(Vec::new());
    }

    let base_font = extract_face_as_standalone_sfnt(original_bytes, face_index)?;

    subset::subset(&base_font, required_chars)
}

fn format_request(request: &FontRequest) -> String {
    format!(
        "{} (bold={}, italic={})",
        request.font_name, request.bold, request.italic
    )
}

fn output_ass_path(
    output_dir: &std::path::Path,
    input: &std::path::Path,
    base_dir: &std::path::Path,
) -> PathBuf {
    if let Ok(relative) = input.strip_prefix(base_dir) {
        output_dir.join(relative)
    } else {
        let file_name = input
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("output.ass");
        output_dir.join(file_name)
    }
}

fn validate_fontpaths(paths: &[PathBuf]) -> Result<()> {
    for path in paths {
        if !path.exists() || !path.is_dir() {
            bail!(AssfontsError::MissingFontPath(path.clone()));
        }
    }
    Ok(())
}

fn load_fonts_for_run(
    options: &RunOptions,
    requested_names: &HashSet<String>,
    embedded_fonts: Vec<FontRecord>,
) -> Result<Vec<FontRecord>> {
    let db_fonts = load_fonts_from_db_on_demand(&options.dbpath, requested_names)?;
    let fontpaths = options.fontpaths.as_deref().unwrap_or(&[]);

    if fontpaths.is_empty() {
        if db_fonts.is_empty() && embedded_fonts.is_empty() {
            bail!(AssfontsError::MissingFontSource(options.dbpath.clone()));
        }
        // Embedded fonts win over the database.
        let mut merged = embedded_fonts;
        let mut existing = std::collections::HashSet::new();
        for font in &merged {
            existing.insert((font.path.clone(), font.face_index));
        }
        for font in db_fonts {
            if !existing.contains(&(font.path.clone(), font.face_index)) && font.path.exists() {
                merged.push(font);
            }
        }
        return Ok(merged);
    }

    validate_fontpaths(fontpaths)?;
    let scanned = discover_fonts(fontpaths);
    if scanned.is_empty() && db_fonts.is_empty() && embedded_fonts.is_empty() {
        bail!(AssfontsError::MissingFontSource(options.dbpath.clone()));
    }

    // Embedded fonts win over scanned and database fonts.
    let mut merged = embedded_fonts;
    let mut existing = std::collections::HashSet::new();
    for font in &merged {
        existing.insert((font.path.clone(), font.face_index));
    }

    for font in scanned {
        if !existing.contains(&(font.path.clone(), font.face_index)) {
            existing.insert((font.path.clone(), font.face_index));
            merged.push(font);
        }
    }

    for font in db_fonts {
        if !existing.contains(&(font.path.clone(), font.face_index)) && font.path.exists() {
            existing.insert((font.path.clone(), font.face_index));
            merged.push(font);
        }
    }

    Ok(merged)
}

/// Extract fonts embedded in the inputs' `[Fonts]` sections into a temp
/// directory and turn them into font records. Controlled by
/// `--use-embedded-fonts` (default on): the returned guard removes the
/// temp directory when dropped (i.e. when the run finishes).
fn collect_embedded_font_records(
    options: &RunOptions,
    loaded_contents: &[String],
) -> Result<(Vec<FontRecord>, Option<EmbeddedFontTempDir>)> {
    if !options.use_embedded_fonts {
        return Ok((Vec::new(), None));
    }

    let mut records = Vec::new();
    let mut seen = HashSet::new();
    let mut dir_used = false;
    let temp_dir = embedded_fonts_temp_dir();

    for content in loaded_contents {
        for (name, bytes) in extract_embedded_fonts(content) {
            if bytes.is_empty() || bytes.len() < 12 || !seen.insert(name.clone()) {
                continue;
            }
            if !dir_used {
                fs::create_dir_all(&temp_dir)?;
                dir_used = true;
            }
            let safe: String = name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            let path = temp_dir.join(safe);
            fs::write(&path, &bytes)?;
            records.extend(discover_from_file(&path));
        }
    }

    if records.is_empty() {
        return Ok((Vec::new(), None));
    }
    Ok((records, Some(EmbeddedFontTempDir(temp_dir))))
}

fn embedded_fonts_temp_dir() -> PathBuf {
    std::env::temp_dir().join("assfonts-embedded-fonts")
}

/// Removes the embedded-fonts temp directory when the run finishes.
struct EmbeddedFontTempDir(PathBuf);

impl Drop for EmbeddedFontTempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn load_fonts_from_db_on_demand(
    dbpath: &std::path::Path,
    requested_names: &HashSet<String>,
) -> Result<Vec<FontRecord>> {
    let db_file = dbpath.join("fonts.db");
    if !db_file.exists() {
        return Ok(Vec::new());
    }

    if requested_names.is_empty() {
        return Ok(Vec::new());
    }

    let conn = crate::db::open_connection(&db_file)?;

    let normalized_names: Vec<String> = requested_names
        .iter()
        .map(|name| normalize_font_name(name))
        .collect();

    let mut records = Vec::new();
    for chunk in normalized_names.chunks(200) {
        let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT DISTINCT f.path, f.face_index, f.display_name, f.normalized_name, f.inferred_weight, f.is_italic 
             FROM fonts f
             LEFT JOIN aliases a ON f.path = a.path AND f.face_index = a.face_index
             WHERE f.normalized_name IN ({placeholders}) OR a.normalized_alias IN ({placeholders})"
        );

        let mut stmt = conn.prepare(&sql)?;

        let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(chunk.len() * 2);
        for val in chunk {
            params.push(val);
        }
        for val in chunk {
            params.push(val);
        }

        let mut rows = stmt.query(rusqlite::params_from_iter(params))?;
        while let Some(row) = rows.next()? {
            let path_str: String = row.get(0)?;
            let face_index: u32 = row.get(1)?;
            let display_name: String = row.get(2)?;
            let normalized_name: String = row.get(3)?;
            let inferred_weight: i32 = row.get(4)?;
            let is_italic_val: i32 = row.get(5)?;

            let mut stmt_aliases =
                conn.prepare("SELECT alias FROM aliases WHERE path = ?1 AND face_index = ?2")?;
            let mut alias_rows = stmt_aliases.query(params![path_str, face_index])?;
            let mut aliases = Vec::new();
            while let Some(alias_row) = alias_rows.next()? {
                let alias: String = alias_row.get(0)?;
                aliases.push(alias);
            }

            records.push(FontRecord {
                display_name,
                normalized_name,
                path: PathBuf::from(path_str),
                face_index,
                inferred_weight,
                is_italic: is_italic_val != 0,
                aliases,
            });
        }
    }

    Ok(records)
}

fn validate_output_dir(path: &std::path::Path) -> Result<()> {
    if path.exists() && !path.is_dir() {
        bail!(AssfontsError::InvalidOutputDir(path.to_path_buf()));
    }
    fs::create_dir_all(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, path::PathBuf};

    use super::{
        PendingFontProcess, ResolvedFontKey, decode_ass_bytes, expand_compat_chars,
        make_resolved_key, upsert_pending_process,
    };
    use crate::{ass::FontRequest, font::FontRecord};

    #[test]
    fn decodes_utf8_with_and_without_bom() {
        let plain = b"[Script Info]\nTitle: demo";
        assert_eq!(
            decode_ass_bytes(plain).unwrap(),
            "[Script Info]\nTitle: demo"
        );

        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice(b"[Script Info]");
        // BOM is stripped so the first section header parses.
        assert_eq!(decode_ass_bytes(&bom).unwrap(), "[Script Info]");
    }

    #[test]
    fn decodes_gbk_fallback() {
        // "中文测试" in GBK
        let gbk: &[u8] = &[0xD6, 0xD0, 0xCE, 0xC4, 0xB2, 0xE2, 0xCA, 0xD4, 0x0A];
        let decoded = decode_ass_bytes(gbk).unwrap();
        assert_eq!(decoded.trim_end(), "中文测试");
    }

    #[test]
    fn expand_compat_chars_keeps_ascii_letters() {
        let mut chars: HashSet<char> = ['你', '好'].into_iter().collect();
        expand_compat_chars(&mut chars, true);
        assert!(chars.contains(&'a') && chars.contains(&'Z'));
        assert!(!chars.contains(&'0'));

        let mut with_digit: HashSet<char> = ['第', '1', '话'].into_iter().collect();
        expand_compat_chars(&mut with_digit, true);
        assert!(with_digit.contains(&'0') && with_digit.contains(&'9'));

        let mut disabled: HashSet<char> = ['a'].into_iter().collect();
        expand_compat_chars(&mut disabled, false);
        assert_eq!(disabled.len(), 1);
    }

    struct CancelledReporter;

    impl super::ProgressReporter for CancelledReporter {
        fn report(&self, _message: String) {}

        fn cancelled(&self) -> bool {
            true
        }
    }

    fn base_options(output: PathBuf) -> crate::cli::RunOptions {
        crate::cli::RunOptions {
            inputs: vec![PathBuf::from("examples/sample.ass")],
            output,
            fontpaths: None,
            dbpath: PathBuf::from("nonexistent-db"),
            strict: false,
            allow_missing_sample: true,
            allow_error_fonts: true,
            report: false,
            force: true,
            format: crate::cli::OutputFormat::Embed,
            videos: None,
            muxer: crate::cli::MuxerChoice::Mkvmerge,
            language: None,
            default_track: None,
            track_name_style: crate::mux::job::TrackNameStyle::English,
            track_fansub_suffix: true,
            track_language_suffix: true,
            track_categories: Vec::new(),
            track_groups: Vec::new(),
            track_overrides: std::collections::BTreeMap::new(),
            default_track_keys: Vec::new(),
            title: None,
            keep_subs: false,
            keep_attachments: false,
            use_embedded_fonts: false,
            utf8_sig: false,
            char_compatible: false,
            rename_fonts: true,
            keep_temp: false,
            muxer_path: None,
            ffprobe_path: None,
        }
    }

    #[test]
    fn run_process_respects_cancellation() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let output = std::env::temp_dir().join(format!("assfonts-cancel-{unique}"));
        let options = base_options(output);

        let err = super::run_process(options, &CancelledReporter).unwrap_err();
        assert!(matches!(err, crate::error::AssfontsError::Cancelled));
    }

    #[test]
    fn run_process_reports_progress() {
        use std::sync::Mutex;

        struct RecordingReporter {
            messages: Mutex<Vec<String>>,
        }
        impl super::ProgressReporter for RecordingReporter {
            fn report(&self, message: String) {
                self.messages.lock().unwrap().push(message);
            }

            fn cancelled(&self) -> bool {
                false
            }
        }

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let output = std::env::temp_dir().join(format!("assfonts-progress-{unique}"));
        let mut options = base_options(output.clone());
        // Use the platform's standard system font directory so the test does
        // not depend on `C:/Windows/Fonts` (which only exists on Windows).
        let system_fonts = if cfg!(windows) {
            PathBuf::from("C:/Windows/Fonts")
        } else if cfg!(target_os = "macos") {
            PathBuf::from("/System/Library/Fonts")
        } else {
            PathBuf::from("/usr/share/fonts")
        };
        options.fontpaths = Some(vec![system_fonts]);

        let reporter = RecordingReporter {
            messages: Mutex::new(Vec::new()),
        };
        super::run_process(options, &reporter).unwrap();
        let messages = reporter.messages.lock().unwrap();
        assert!(messages.iter().any(|m| m.contains("待处理")));
        assert!(messages.iter().any(|m| m.contains("处理 examples")));

        let _ = std::fs::remove_dir_all(output);
    }

    #[test]
    fn resolved_key_includes_path_and_face_index() {
        let record = FontRecord {
            display_name: "Test Font".to_string(),
            normalized_name: "testfont".to_string(),
            path: PathBuf::from("C:/fonts/test.ttf"),
            face_index: 2,
            inferred_weight: 400,
            is_italic: false,
            aliases: vec!["Test Font".to_string()],
        };

        let key = make_resolved_key(&record);
        assert_eq!(key.0, PathBuf::from("C:/fonts/test.ttf"));
        assert_eq!(key.1, 2);
    }

    #[test]
    fn resolved_key_map_can_group_duplicate_fonts() {
        let key = (PathBuf::from("C:/fonts/test.ttf"), 0_u32);
        let mut grouped_index = std::collections::HashMap::new();
        grouped_index.insert(key.clone(), 0_usize);

        assert_eq!(grouped_index.get(&key), Some(&0_usize));
    }

    #[test]
    fn upsert_pending_process_merges_required_chars_for_same_key() {
        let mut grouped: Vec<PendingFontProcess> = Vec::new();
        let mut grouped_index: std::collections::HashMap<ResolvedFontKey, usize> =
            std::collections::HashMap::new();

        let record = FontRecord {
            display_name: "Test Font".to_string(),
            normalized_name: "testfont".to_string(),
            path: PathBuf::from("C:/fonts/test.ttf"),
            face_index: 0,
            inferred_weight: 400,
            is_italic: false,
            aliases: vec!["Test Font".to_string()],
        };
        let key = make_resolved_key(&record);

        let request = FontRequest {
            font_name: "Test Font".to_string(),
            bold: 400,
            italic: false,
        };

        let mut chars_a = HashSet::new();
        chars_a.insert('A');
        let mut chars_b = HashSet::new();
        chars_b.insert('B');

        upsert_pending_process(
            &mut grouped,
            &mut grouped_index,
            key.clone(),
            &request,
            "testfont",
            &record,
            &chars_a,
        );
        upsert_pending_process(
            &mut grouped,
            &mut grouped_index,
            key,
            &request,
            "testfont",
            &record,
            &chars_b,
        );

        assert_eq!(grouped.len(), 1);
        assert!(grouped[0].required_chars.contains(&'A'));
        assert!(grouped[0].required_chars.contains(&'B'));
    }

    #[test]
    fn check_subtitles_reports_missing_fonts_without_writing_output() {
        use std::sync::Mutex;

        struct RecordingReporter {
            messages: Mutex<Vec<String>>,
            progress: Mutex<Vec<(String, usize, usize)>>,
        }
        impl super::ProgressReporter for RecordingReporter {
            fn report(&self, message: String) {
                self.messages.lock().unwrap().push(message);
            }

            fn progress(&self, stage: &str, done: usize, total: usize) {
                self.progress
                    .lock()
                    .unwrap()
                    .push((stage.to_string(), done, total));
            }

            fn cancelled(&self) -> bool {
                false
            }
        }

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        // Empty font directory + no fonts.db: every request has to be reported
        // as missing instead of failing the check.
        let empty_fonts = std::env::temp_dir().join(format!("assfonts-empty-fonts-{unique}"));
        std::fs::create_dir_all(&empty_fonts).unwrap();
        let output = std::env::temp_dir().join(format!("assfonts-check-output-{unique}"));

        let mut options = base_options(output.clone());
        options.fontpaths = Some(vec![empty_fonts.clone()]);

        let inputs = vec![PathBuf::from("examples/sample.ass")];
        let reporter = RecordingReporter {
            messages: Mutex::new(Vec::new()),
            progress: Mutex::new(Vec::new()),
        };
        let summary = super::check_subtitles(&inputs, &options, &reporter).unwrap();

        assert_eq!(summary.subtitle_count, 1);
        assert!(summary.font_request_count > 0);
        assert!(summary.font_reports.is_empty());
        assert!(!summary.missing_fonts.is_empty());
        assert!(
            summary
                .missing_fonts
                .iter()
                .any(|name| name.contains("Arial"))
        );

        let messages = reporter.messages.lock().unwrap();
        assert!(messages.iter().any(|line| line.contains("需要字体")));
        assert!(messages.iter().any(|line| line.contains("缺少字体")));
        assert!(messages.iter().any(|line| line.contains("检查汇总")));
        drop(messages);

        // The GUI progress card is driven by these stages, so a check run has
        // to report them with a known total.
        let progress = reporter.progress.lock().unwrap();
        assert!(
            progress
                .iter()
                .any(|(stage, done, total)| stage == "读取字幕" && *done == 0 && *total == 1)
        );
        assert!(
            progress
                .iter()
                .any(|(stage, _done, total)| stage == "检查字形" && *total == 1)
        );
        drop(progress);

        // A check run must not create the output directory or any file.
        assert!(!output.exists());
        let _ = std::fs::remove_dir_all(&empty_fonts);
    }
}
