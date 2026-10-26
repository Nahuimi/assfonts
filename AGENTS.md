# assfonts Workspace Instructions

## Scope

- This repository is a Rust CLI for ASS subtitle font indexing, matching, subsetting, and embedding.
- Prefer small, surgical changes. Keep module boundaries intact instead of moving logic across files unless the task requires it.
- Do not edit generated or local-output directories such as `target/` and `out/`.

## Commands

- Build: `cargo build`
- Check: `cargo check`
- Clippy: `cargo clippy`
- Test: `cargo test`
- Show CLI help: `cargo run -- -h`
- Normal CLI processing uses the flattened `RunOptions` on the root command (no subcommand):
  ```bash
  cargo run -- -i examples/sample.ass -o out -f C:/Windows/Fonts
  ```
- Font index build uses the explicit `build` subcommand:
  ```bash
  cargo run -- build -f C:/Windows/Fonts -o ./.db
  ```

## Architecture

- `src/main.rs` wires clap parsing to two entry paths: `build` subcommand and the default processing flow.
- `src/cli.rs` defines the CLI surface. **There is no `run` subcommand**; normal processing is the default command path.
- `src/commands.rs` is the orchestration layer for indexing, per-file processing, reporting, cache usage, and validation. `run_process` dispatches on `--format`: `embed` (original pipeline) or `mks`/`mkv` (mux pipeline with per-media grouping and shared subset fonts).
- `src/ass.rs` parses ASS styles, dialogue overrides, and collects font requests with bold/italic state.
- `src/font.rs` discovers fonts, normalizes names, matches requests, and extracts TTC/OTC faces as standalone sfnt.
- `src/subset.rs` performs font subsetting via `allsorts`.
- `src/embed.rs` renders the `[Fonts]` section and uuencodes embedded data; `uudecode`/`extract_embedded_fonts` turn an input's `[Fonts]` section back into font bytes (temp-dir font source, highest priority).
- `src/mux/` performs `.mks`/`.mkv` muxing through external tools: `job.rs` (pure helpers: media-subtitle pairing — prefix / anchored / SubRenamer-style order pairing, track comments, output-name dedup, language mapping, rename suffix rules `SuffixRule`), `mkvmerge.rs` (JSON option file adapter), `ffmpeg.rs` (argument builder + ffprobe), `mod.rs` (`Muxer` trait, PATH-only tool resolution via `find_on_path` — no subprocess probing, media extension list, sfnt flavor sniffing).
- `src/rename.rs` implements CRC-based subsetted font renaming (D7): rewrites the sfnt `name` table (NameIDs 1/3/4/6) and rewrites ASS style names plus `\fn` override tags; gated by `--rename-fonts` in mks/mkv mode.
- `src/error.rs` is the shared error surface using `thiserror` and the crate-wide `Result<T>` alias.
- Input decoding (`commands.rs::decode_ass_bytes`): UTF-8 first (BOM stripped), then chardetng + encoding_rs fallback (GBK/Big5/Shift-JIS...).

## CLI Options (Current Implementation)

### Default Command (RunOptions)

| Flag | Default | Description |
|------|---------|-------------|
| `-i, --input` | (required) | Input ASS files (one or more) |
| `-o, --output` | `.` | Output directory |
| `-f, --fontpath` | (optional) | Font directories to scan |
| `-d, --dbpath` | `.` | Directory containing `fonts.db` |
| `-s, --strict` | `true` | Fail on font usage errors |
| `--allow-missing-sample` | `false` | Allow missing character samples |
| `--allow-missing-fonts` | `false` | Allow missing fonts |
| `--report` | `false` | Generate `run-report.json` after processing |
| `--force` | `false` | Overwrite existing output files (embed mode) |
| `--format` | `embed` | Output format: `embed`, `mks`, or `mkv` |
| `--video` | (none) | Media files/directories for `--format mkv` |
| `--muxer` | `mkvmerge` | External muxer for mks/mkv (`mkvmerge` or `ffmpeg`) |
| `--language` | (none) | Subtitle language: fixed code or `.comment=code;...` mappings |
| `--default-track` | (first) | Subtitle comment to mark as default track |
| `--title` | (media name) | Override container title |
| `--keep-subs` | `false` | Keep original subtitle tracks of the input media |
| `--keep-attachments` | `false` | Keep original attachments of the input media |
| `--use-embedded-fonts` | `true` | Use fonts embedded in the input `[Fonts]` section as the highest-priority font source |
| `--rename-fonts` | `true` | Rename subsetted fonts with a CRC suffix and rewrite ASS references (mks/mkv mode) |
| `--utf8-sig` | `false` | Write output ASS files with a UTF-8 BOM |
| `--char-compatible` | `false` | Keep all ASCII letters (and all digits when any digit is used) in subsetted fonts |
| `--keep-temp` | `false` | Keep the intermediate `.fonts/` directory after muxing |

mks/mkv mode groups subtitles per media file by case-insensitive stem-prefix matching, shares subset fonts across the group, muxes fonts as Matroska attachments, and deletes the temporary `<output>.fonts/` directory on success (`--keep-temp` keeps it; failures also keep it). Mux outputs auto-dedup with `#1`, `#2`, … suffixes.

### build Subcommand (BuildOptions)

| Flag | Default | Description |
|------|---------|-------------|
| `-f, --fontpath` | (required) | Font directories to scan (one or more) |
| `-o, --output` | `.` | Output directory for `fonts.db` |

## GUI

- `gui/` is a workspace member (`assfonts-gui`) built on `gpui-kit` 0.8; it depends on this crate as a library and contains no business logic. Layout follows the FluxDown reference (sidebar nav + setting cards); the sidebar collapses to icon-only via `SidebarToggleButton`.
- gpui-kit API notes for 0.8.0 (differs from its main-branch README): `Button::primary()` comes from the `ButtonVariants` trait (import it); `.rounded()` takes a length; `App::notify` takes an `EntityId` while `Context::notify()` is zero-arg; `v_flex`/`h_flex` free functions come from `gpui_kit::component::*`; `SidebarMenu::new()`; `white`/`black` are ambiguous under double glob imports — import them explicitly from `gpui_kit`; `IconName` (full Lucide catalog) comes from `gpui_kit::assets`; `Switch::on_click` receives `&bool`; `ThemeMode` only has Light/Dark (no System — auto means `cx.window_appearance()` detection); theme colors come from `cx.theme()` via `ActiveTheme`.
- **Content scrolling**: use native `v_flex().id("...").flex_1().min_h_0().overflow_y_scroll()` (Stateful element required — `.id()` first). **Critical**: `h_flex()` installs `items_center` — a column inside it takes its *content* height (centered, overflowing both ways), so any column owning a header/scroll region in an `h_flex` row MUST get `.h_full()` (documented in gpui-base `StyledExt::h_flex`; omitting it silently breaks scrolling and clips the page). The component `ScrollableElement::overflow_scrollbar()` wrapper and the component `Sidebar` (asymmetric collapsed paddings) are not used; the sidebar is drawn by hand.
- **Icons**: register `gpui_kit::assets::AllAssets` (full Lucide catalog) via `with_assets` — the default `Assets` only bundles the ~106 component icons, so any other `IconName` silently renders blank (`AssetSource::load` errors). `gui/tests/asset_probe.rs` guards the icons the UI uses.
- Custom title bar: use `TitleBar::new()` + `WindowOptions { ..TitleBar::window_options() }` (draws Windows min/max/close itself).
- The GUI bin sets `#![windows_subsystem = "windows"]` (no console host); cycle-button indices must be normalized with `% len` at increment time (a raw index panics render).
- GUI settings persist as TOML in a portable `gui.toml` **next to the executable** (`GuiSettings` in `gui/src/main.rs`); save on every mutation point via `AssfontsApp::persist(cx)`.
- Input decoding is idempotent for renamed outputs: `load_and_preprocess_ass` runs `rename::apply_subset_records_reverse` so `; Font Subset:` records map CRC names back to originals and stale record lines are stripped.
- **File drag-drop**: the platform emits `FileDropEvent::Entered { paths: ExternalPaths(..) }` (an OLE `IDropTarget` implementation), and the internal drag stores that payload — drop listeners MUST be typed `.on_drop::<ExternalPaths>` (dispatch matches `drag.value.as_ref().type_id()`, the *inner* type; `Vec<PathBuf>` or `Arc<Vec<PathBuf>>` compile but silently never fire). Read paths via `&files.0` (`SmallVec<PathBuf>`); write values via `InputState::replace_all(text, window, cx)`. The component `Sidebar` is not used; the sidebar is drawn by hand.
- `black()`/`white()` opacity helpers: prefer semantic theme tokens later; current cards use `black().opacity(0.25)` over the dark theme.

## Output Structure

Given `-o ./out` and input `sample.ass`:

- `out/sample.ass` — processed ASS with embedded fonts
- `out/run-report.json` — processing report (if `--report`)

## Conventions

- Follow the existing error-handling pattern: return `Result<T>` and use `AssfontsError` variants instead of ad hoc strings where a concrete variant already exists.
- Preserve the current import/layout style. The repository uses `rustfmt.toml` with edition 2024 and crate-level import grouping.
- Prefer extending the current pipeline helpers in `src/commands.rs` over duplicating per-input orchestration logic elsewhere.
- Keep font-name matching behavior compatible with `normalize_font_name` in `src/font.rs`.
- Add or update focused unit tests near the affected module when behavior changes.

## Dependencies

- `clap` — CLI argument parsing (derive feature)
- `regex` — pattern matching
- `serde` / `serde_json` — serialization for index, reports, and mkvmerge option files
- `thiserror` — error type derivation
- `walkdir` — recursive directory traversal
- `ttf-parser` — font parsing and metadata extraction
- `allsorts` — font subsetting
- `rayon` — parallel processing
- `dashmap` — concurrent hash map for font byte caching

External tools (mks/mkv mode only): `mkvmerge` (primary) or `ffmpeg` + `ffprobe` (fallback), probed at runtime.

## Pitfalls

- **README.md / README_zh_CN.md**: current as of 2026-10-25 (verified against `src/cli.rs`; an older revision referenced a `run` subcommand and nonexistent options — keep the READMEs in sync with `cli.rs` when the CLI surface changes).
- In debug builds, `src/test.rs` can intercept the first CLI argument `analyze_ass`; keep that in mind when debugging argument parsing.
- TTC/OTC handling depends on `face_index`; avoid collapsing multi-face fonts into path-only identity.
- `src/commands.rs` uses `rayon` and `DashMap` for cross-file processing and font-byte caching. Be careful not to introduce shared-state assumptions that break parallel execution.
- The `--force` flag logic is inverted: without `--force`, the code returns `FileExists` error if output already exists.

## Key Files

- `src/commands.rs` — orchestration, validation, reporting, and parallel processing
- `src/ass.rs` — ASS parsing and override-tag handling (`\fn`, `\b`, `\i`, `\r`)
- `src/font.rs` — font discovery, matching, TTC/OTC extraction
- `src/subset.rs` — allsorts subsetting wrapper
- `src/embed.rs` — uuencoding and `[Fonts]` section generation
- `src/mux/` — mks/mkv muxing adapters (mkvmerge JSON option file, ffmpeg args) and pure mux-job helpers
- `src/error.rs` — project-standard error design
- `examples/sample.ass` — minimal in-repo end-to-end sample input

## Validation

- `cargo test` passes in the current workspace
- After code changes, prefer validating with `cargo test` and only add broader commands when the task specifically needs them

## 文档读写规则

适用于本项目所有文档工作（代码协作规则见上文各节）。

### 读取规则

- 按当前问题选择资料，不因文件存在、可能有用或被索引链接就读取；不默认读取所有项目文件，也不默认全文读取选中的文件。定位到文件后，继续定位相关章节、条目、函数或配置项。
- 优先使用已有有效信息；需要查找时，按“索引或限定范围搜索 → 标题、符号或关键词 → 命中片段及必要上下文”的顺序读取。
- 只有证据不足、存在冲突或需要核对依赖时才扩大范围；信息足够即停止。不得连续分段读取变相遍历无关全文。
- 全文读取仅用于短小且全部相关的文件、完整理解适用规则，或明确需要整份审查的任务；不得因此扩展为全目录或全项目读取。
- 索引是条件路由，不是必读清单。历史、日志、依赖、产物和数据目录不默认读取；不读取密钥及无关私人数据。
- 不为节省上下文跳过适用规则，也不因片段过窄而断章取义；已加载且仍有效的内容不重复读取。

### 写入规则

- 仅在任务允许且信息有实质变化时更新；只读任务不改记录。
- 每类事实只设一个权威来源，其他位置使用简短摘要和链接。
- 修改前读取目标部分及必要上下文，局部修改使用定点编辑；不得根据局部阅读覆盖整份文件。
- 当前状态直接修订，有价值的旧过程归档到 `history/`；不反复追加矛盾快照，不保存完整聊天和操作流水账。
- 区分事实、计划、建议、已确认决定与待核实事项；区分代码完成、测试通过、安装验证、用户验收和发布授权。
- 命令已配置不等于执行成功，历史结果不等于当前证据，文档中的操作步骤不构成执行授权。
- 使用清晰标题和项目内相对链接；核验日期注明对象与范围，不写入秘密或个人设备私有路径。

## 文档结构

根目录：`AGENTS.md`（长期规则）、`PROJECT_INDEX.md`（任务路由）、`README.md` / `README_zh_CN.md`（使用说明）。专题文档在 `docs/context/`（NOW / PLAN / DECISIONS / RISKS / MAP / RUNBOOK），历史归档在 `history/`。各文件职责与更新时机见 `PROJECT_INDEX.md`。
