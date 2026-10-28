# assfonts

English | [简体中文](README_zh_CN.md)

A Rust CLI tool for ASS subtitle font indexing, matching, subsetting, and embedding.

## Features

- **Font Indexing**: Scan font directories and build a JSON index for fast lookups
- **Smart Font Matching**: Match ASS font requests to available fonts with bold/italic support
- **Font Subsetting**: Reduce font file size by extracting only required glyphs
- **Font Embedding**: Embed subsetted fonts directly into ASS files as `[Fonts]` section
- **Multi-file Processing**: Process multiple ASS files in parallel
- **TTC/OTC Support**: Extract individual faces from TrueType/OpenType Collection fonts

## Installation

### From Source

```bash
git clone https://github.com/kotorimiku/assfonts.git
cd assfonts
cargo build --release
```

## Usage

### Build Font Index

Build a font index file for fast font lookups:

```bash
assfonts build -f /path/to/fonts -d /path/to/db
```

This creates `fonts.db` in the specified directory.

Options:
- `-f, --fontpath`: Font directories to scan (required, supports multiple paths)
- `-o, --output`: Output directory for `fonts.db` (default: current directory)

### Process ASS Files

Embed fonts into ASS files:

```bash
assfonts -i input.ass -o output_dir -f /path/to/fonts
```

Process multiple files or directories:

```bash
assfonts -i /path/to/ass/files -o output_dir -f /path/to/fonts -d /path/to/db
```

Options:
- `-i, --input`: Input ASS/SSA files or directories (required, supports multiple paths)
- `-o, --output`: Output directory (default: current directory)
- `-f, --fontpath`: Font directories to scan (optional)
- `-d, --dbpath`: Directory containing `fonts.db` (default: current directory)
- `-s, --strict`: Fail on font usage errors (default: true)
- `--allow-missing-sample`: Allow missing character samples
- `--allow-missing-fonts`: Allow missing fonts
- `--report`: Generate `run-report.json` after processing
- `--force`: Overwrite existing output files
- `--format embed|mks|mkv`: Output format (default: `embed`). `mks` muxes each subtitle with its subsetted fonts into a standalone Matroska file; `mkv` muxes media + subtitles + fonts.
- `--video <path>...`: Media files or directories for `--format mkv` (paired with subtitles by file name)
- `--muxer mkvmerge|ffmpeg`: External muxer used by mks/mkv (default: `mkvmerge`)
- `--language <spec>`: Subtitle language, e.g. `chi` (all tracks) or `.sc=chi;.tc=chi` comment mappings (built-in defaults for `.sc/.tc/.chs/.cht/...` apply first)
- `--default-track <comment>`: Subtitle filename comment (part after the media name) to mark as default
- `--track-name-style english|chinese`: Track title style for mks/mkv (default: `english`, e.g. `KTXP.TC`; `chinese`, e.g. `极影字幕社.繁体`)
- `--track-fansub-suffix <BOOL>`: Include the fansub-group suffix in track names (default: true)
- `--track-language-suffix <BOOL>`: Include the language suffix in track names (default: true)
- `--title <title>`: Override the container title (default: media file name)
- `--keep-subs` / `--keep-attachments`: Keep original subtitle tracks/attachments of the input media
- `--use-embedded-fonts <BOOL>`: Use fonts embedded in the input `[Fonts]` section as the highest-priority font source, so re-processing an embedded ASS needs no installed fonts (default: true)
- `--rename-fonts <BOOL>`: Rename subsetted fonts with a CRC suffix and rewrite ASS references to avoid same-family face ambiguity (default: true)
- `--utf8-sig`: Write output ASS files with a UTF-8 BOM
- `--char-compatible`: Keep all ASCII letters (and all digits when any digit is used) in subsetted fonts for renderer compatibility
- `--keep-temp`: Keep the intermediate subsetted font files after muxing (removed on success by default)

## Examples

### Basic Usage

```bash
# Process a single ASS file
assfonts -i video.ass -o ./out -f C:/Windows/Fonts

# Process all ASS files in a directory
assfonts -i ./subtitles -o ./output -f /usr/share/fonts

# Use pre-built font index
assfonts build -f /usr/share/fonts -d ~/.assfonts
assfonts -i video.ass -o ./out -d ~/.assfonts
```

### With Report Generation

```bash
assfonts -i video.ass -o ./out -f C:/Windows/Fonts --report
```

The report (`run-report.json`) includes:
- Missing fonts
- Glyph coverage statistics
- Font file size reduction

### Relaxed Mode

Allow missing fonts and continue processing:

```bash
assfonts -i video.ass -o ./out -f C:/Windows/Fonts --allow-missing-fonts --allow-missing-sample
```

### Mux Into MKV

Mux a video with subtitle(s) and subsetted fonts as attachments (requires `mkvmerge`):

```bash
# Subtitles named show.sc.ass / show.tc.ass pair with show.mkv by name;
# languages come from the built-in comment map (.sc/.tc/.chs/.cht -> chi)
assfonts -i show.sc.ass -i show.tc.ass -o ./out -f C:/Windows/Fonts --format mkv --video show.mkv

# Standalone subtitle-only Matroska file
assfonts -i show.sc.ass -o ./out -f C:/Windows/Fonts --format mks
```

## Documentation

- [Project index](PROJECT_INDEX.md) — task-to-document routing
- [Porting plan: MKS/MKV muxing + gpui-kit GUI](docs/context/PLAN.md) — active development plan
- [Current status](docs/context/NOW.md)

## Related Projects

- [assfonts](https://github.com/wyzdwdz/assfonts) - Original C++ implementation

## License

MIT License - see [LICENSE](LICENSE) for details.
