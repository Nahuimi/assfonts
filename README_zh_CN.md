# assfonts

简体中文 | [English](README.md)

一个用 Rust 编写的 ASS 字幕字体索引、匹配、子集化和嵌入的命令行工具。

## 功能特性

- **字体索引**：扫描字体目录并生成 JSON 索引文件，便于快速查询
- **智能字体匹配**：根据 ASS 字体请求匹配可用字体，支持粗体/斜体样式
- **字体子集化**：仅提取所需的字形，大幅减小字体文件体积
- **字体嵌入**：将子集化后的字体直接嵌入到 ASS 文件的 `[Fonts]` 区域
- **多文件处理**：并行处理多个 ASS 文件
- **TTC/OTC 支持**：支持从 TrueType/OpenType Collection 字体中提取单个字体面

## 安装

### 从源码编译

```bash
git clone https://github.com/kotorimiku/assfonts.git
cd assfonts
cargo build --release
```

## 使用方法

### 构建字体索引

生成字体索引文件以便快速查询：

```bash
assfonts build -f /path/to/fonts -d /path/to/db
```

这将在指定目录中创建 `fonts.db` 文件。

参数：
- `-f, --fontpath`：要扫描的字体目录（必需，支持多个路径）
- `-o, --output`：`fonts.db` 的输出目录（默认：当前目录）

### 处理 ASS 文件

将字体嵌入到 ASS 文件中：

```bash
assfonts -i input.ass -o output_dir -f /path/to/fonts
```

处理多个文件或目录：

```bash
assfonts -i /path/to/ass/files -o output_dir -f /path/to/fonts -d /path/to/db
```

参数：
- `-i, --input`：输入 ASS/SSA 文件或目录（必需，支持多个路径）
- `-o, --output`：输出目录（默认：当前目录）
- `-f, --fontpath`：要扫描的字体目录（可选）
- `-d, --dbpath`：包含 `fonts.db` 的目录（默认：当前目录）
- `-s, --strict`：遇到字体错误时失败（默认：true）
- `--allow-missing-sample`：允许缺失字符样本
- `--allow-missing-fonts`：允许缺失字体
- `--report`：处理后生成 `run-report.json` 报告
- `--force`：覆盖已存在的输出文件
- `--format embed|mks|mkv`：输出格式（默认 `embed`）。`mks` 把字幕与其子集化字体封装成独立 Matroska 文件，按视频分组、用视频文件名命名（一个视频一个文件，不含视频轨；没配到视频的字幕各自封装）；`mkv` 将视频 + 字幕 + 字体封装为 MKV
- `--video <path>...`：`--format mkv` 的媒体文件或目录（按文件名与字幕配对）
- `--muxer mkvmerge|ffmpeg`：mks/mkv 使用的封装器（默认 `mkvmerge`）
- `--language <spec>`：字幕语言，如 `chi`（全部轨道）或 `.sc=chi;.tc=chi` 注释映射（内置 `.sc/.tc/.chs/.cht/...` 映射优先生效）
- `--default-track <comment>`：指定为默认轨的字幕注释（文件名中媒体名之后的部分）
- `--track-name-style english|chinese`：mks/mkv 轨道标题风格（默认 `english`，如 `KTXP-TC`；`chinese` 如 `极影字幕社-繁体`）
- `--track-fansub-suffix <BOOL>`：轨道名是否包含字幕组后缀（默认 true）
- `--track-language-suffix <BOOL>`：轨道名是否包含语言后缀（默认 true）
- `--title <title>`：覆盖容器标题（默认为媒体文件名）
- `--keep-subs` / `--keep-attachments`：保留输入媒体中原有的字幕轨/附件
- `--use-embedded-fonts <BOOL>`：把输入字幕 `[Fonts]` 段内嵌的字体作为最高优先级字体源，二次处理已内嵌字幕无需系统安装字体（默认 true）
- `--rename-fonts <BOOL>`：用 CRC 后缀重命名子集字体并同步改写 ASS 引用，避免同族多字面匹配歧义（默认 true）
- `--utf8-sig`：输出的 ASS 文件带 UTF-8 BOM
- `--char-compatible`：子集字体恒保留 a-z/A-Z，出现任意数字时保留全部 0-9（兼容部分渲染器）
- `--keep-temp`：封装后保留中间子集字体文件（默认成功后删除）

## 示例

### 基本用法

```bash
# 处理单个 ASS 文件
assfonts -i video.ass -o ./out -f C:/Windows/Fonts

# 处理目录中的所有 ASS 文件
assfonts -i ./subtitles -o ./output -f /usr/share/fonts

# 使用预构建的字体索引
assfonts build -f /usr/share/fonts -d ~/.assfonts
assfonts -i video.ass -o ./out -d ~/.assfonts
```

### 生成报告

```bash
assfonts -i video.ass -o ./out -f C:/Windows/Fonts --report
```

报告（`run-report.json`）包含：
- 缺失字体列表
- 字形覆盖率统计
- 字体文件体积压缩信息

### 宽松模式

允许缺失字体并继续处理：

```bash
assfonts -i video.ass -o ./out -f C:/Windows/Fonts --allow-missing-fonts --allow-missing-sample
```

### 封装为 MKV

将视频、字幕与子集化字体（作为附件）封装到 MKV（需要 `mkvmerge`）：

```bash
# 字幕 show.sc.ass / show.tc.ass 按文件名与 show.mkv 配对；
# 语言取自内置注释映射（.sc/.tc/.chs/.cht -> chi）
assfonts -i show.sc.ass -i show.tc.ass -o ./out -f C:/Windows/Fonts --format mkv --video show.mkv

# 仅字幕 + 字体的独立 Matroska 文件
assfonts -i show.sc.ass -o ./out -f C:/Windows/Fonts --format mks
```

## 文档

- [项目索引](PROJECT_INDEX.md) — 任务到文档的路由
- [移植计划：MKS/MKV 封装 + gpui-kit GUI](docs/context/PLAN.md) — 进行中的开发计划
- [当前状态](docs/context/NOW.md)

## 相关项目

- [assfonts](https://github.com/wyzdwdz/assfonts) - 原始 C++ 实现

## 许可证

MIT License - 详见 [LICENSE](LICENSE)。
