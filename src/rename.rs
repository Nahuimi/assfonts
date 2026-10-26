//! CRC-based subsetted font renaming (D7).
//!
//! Mirrors the ASFMKV Python tool: NameIDs 1/3/4/6 of the sfnt `name` table
//! are replaced entirely with an 8-digit uppercase CRC hex string, and the
//! ASS styles plus `\fn` override tags are rewritten to reference that name.
//! Controlled by `--rename-fonts` in mks/mkv mode.

use std::collections::HashMap;
use std::sync::LazyLock;

use crate::{
    error::{AssfontsError, Result},
    font::normalize_font_name,
};

const NAME_TABLE_TAG: u32 = u32::from_be_bytes(*b"name");
const HEAD_TABLE_TAG: u32 = u32::from_be_bytes(*b"head");
const CHECKSUM_ADJUSTMENT_MAGIC: u32 = 0xB1B0_AFBA;

/// Lookup key matching a font request: normalized name + bold/italic state.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RenameKey {
    pub name: String,
    pub bold: i32,
    pub italic: bool,
}

impl RenameKey {
    pub fn new(font_name: &str, bold: i32, italic: bool) -> Self {
        Self {
            name: normalize_font_name(font_name),
            bold: normalize_bold(bold),
            italic,
        }
    }
}

fn normalize_bold(value: i32) -> i32 {
    if value == -1 || value == 1 {
        700
    } else if value <= 0 {
        400
    } else {
        value
    }
}

fn crc32_table() -> &'static [u32; 256] {
    static TABLE: LazyLock<[u32; 256]> = LazyLock::new(|| {
        let mut table = [0u32; 256];
        for (index, entry) in table.iter_mut().enumerate() {
            let mut crc = index as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    0xEDB8_8320 ^ (crc >> 1)
                } else {
                    crc >> 1
                };
            }
            *entry = crc;
        }
        table
    });
    &TABLE
}

pub fn crc32(data: &[u8]) -> u32 {
    let table = crc32_table();
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc = table[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

/// Deterministic 8-digit uppercase CRC for one subsetted face,
/// derived from its required characters and font name.
pub fn compute_subset_crc(required_chars: &str, font_name: &str) -> String {
    let mut chars: Vec<char> = required_chars.chars().collect();
    chars.sort_unstable();
    chars.dedup();
    let mut data: Vec<u8> = chars.into_iter().collect::<String>().into_bytes();
    data.extend_from_slice(font_name.as_bytes());
    format!("{:08X}", crc32(&data))
}

#[derive(Debug, Clone)]
struct NameRecord {
    platform: u16,
    encoding: u16,
    language: u16,
    name_id: u16,
    value: Vec<u8>,
}

fn be16(data: &[u8], at: usize) -> Option<u16> {
    let raw = data.get(at..at + 2)?;
    Some(u16::from_be_bytes([raw[0], raw[1]]))
}

fn be32(data: &[u8], at: usize) -> Option<u32> {
    let raw = data.get(at..at + 4)?;
    Some(u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

/// Replace the sfnt `name` table so that NameIDs 1/3/4/6 carry `new_name`.
/// Malformed or format-1 name tables pass through unchanged.
pub fn rename_font_names(font_bytes: &[u8], new_name: &str) -> Result<Vec<u8>> {
    if font_bytes.len() < 12 {
        return Err(AssfontsError::Font("sfnt too short".to_string()));
    }
    let version = be32(font_bytes, 0).unwrap_or(0);
    if version != 0x0001_0000 && version != 0x4F54_544F && version != 0x7472_7565 {
        return Err(AssfontsError::Font(format!(
            "unsupported sfnt version 0x{version:08X}"
        )));
    }

    let num_tables = be16(font_bytes, 4).unwrap_or(0) as usize;
    let mut tables: Vec<(u32, usize, usize)> = Vec::with_capacity(num_tables);
    for index in 0..num_tables {
        let record = 12 + index * 16;
        let Some(tag) = be32(font_bytes, record) else {
            return Err(AssfontsError::Font("truncated table directory".to_string()));
        };
        let offset = be32(font_bytes, record + 8).unwrap_or(0) as usize;
        let length = be32(font_bytes, record + 12).unwrap_or(0) as usize;
        if offset + length > font_bytes.len() {
            return Err(AssfontsError::Font("table out of bounds".to_string()));
        }
        tables.push((tag, offset, length));
    }

    let Some(&(_, name_offset, name_length)) = tables.iter().find(|(tag, _, _)| *tag == NAME_TABLE_TAG)
    else {
        return Err(AssfontsError::Font("font has no name table".to_string()));
    };
    let name_data = &font_bytes[name_offset..name_offset + name_length];
    let Some((_, records)) = parse_name_table(name_data) else {
        return Ok(font_bytes.to_vec());
    };

    let renamed = build_name_table(&rename_name_records(records, new_name));

    // head.checkSumAdjustment must be zeroed before computing the font checksum.
    let mut rebuilt: Vec<(u32, Vec<u8>)> = tables
        .iter()
        .map(|&(tag, offset, length)| {
            let mut data = font_bytes[offset..offset + length].to_vec();
            if tag == HEAD_TABLE_TAG && data.len() >= 12 {
                data[8..12].copy_from_slice(&0u32.to_be_bytes());
            }
            if tag == NAME_TABLE_TAG {
                data = renamed.clone();
            }
            (tag, data)
        })
        .collect();

    let mut out = Vec::with_capacity(font_bytes.len() + renamed.len());
    out.extend_from_slice(&font_bytes[0..12]);
    for (tag, data) in &rebuilt {
        out.extend_from_slice(&tag.to_be_bytes());
        out.extend_from_slice(&table_checksum(data).to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // offset, patched below
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    }

    let mut head_offset_in_out = None;
    for (index, (tag, data)) in rebuilt.iter_mut().enumerate() {
        let offset = out.len() as u32;
        out[12 + index * 16 + 8..12 + index * 16 + 12].copy_from_slice(&offset.to_be_bytes());
        if *tag == HEAD_TABLE_TAG && data.len() >= 12 {
            head_offset_in_out = Some(out.len() + 8);
        }
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    if let Some(head_at) = head_offset_in_out {
        let adjustment = CHECKSUM_ADJUSTMENT_MAGIC.wrapping_sub(crc32(&out));
        out[head_at..head_at + 4].copy_from_slice(&adjustment.to_be_bytes());
    }

    Ok(out)
}

fn parse_name_table(data: &[u8]) -> Option<(u16, Vec<NameRecord>)> {
    let format = be16(data, 0)?;
    if format != 0 {
        return None;
    }
    let count = be16(data, 2)? as usize;
    let string_offset = be16(data, 4)? as usize;

    let mut records = Vec::with_capacity(count);
    for index in 0..count {
        let record = 6 + index * 12;
        let platform = be16(data, record)?;
        let encoding = be16(data, record + 2)?;
        let language = be16(data, record + 4)?;
        let name_id = be16(data, record + 6)?;
        let length = be16(data, record + 8)? as usize;
        let offset = be16(data, record + 10)? as usize;
        let start = string_offset + offset;
        let value = data.get(start..start + length)?.to_vec();
        records.push(NameRecord {
            platform,
            encoding,
            language,
            name_id,
            value,
        });
    }
    Some((format, records))
}

fn rename_name_records(records: Vec<NameRecord>, new_name: &str) -> Vec<NameRecord> {
    records
        .into_iter()
        .map(|record| {
            let renamed = matches!(record.name_id, 1 | 3 | 4 | 6);
            if !renamed {
                return record;
            }
            let encoded = match (record.platform, record.encoding) {
                (0, _) | (3, 0 | 1 | 10) => new_name
                    .encode_utf16()
                    .flat_map(u16::to_be_bytes)
                    .collect::<Vec<u8>>(),
                (1, 0) => new_name.as_bytes().to_vec(),
                _ => record.value.clone(),
            };
            NameRecord {
                value: encoded,
                ..record
            }
        })
        .collect()
}

fn build_name_table(records: &[NameRecord]) -> Vec<u8> {
    let string_offset = 6 + records.len() * 12;
    let mut out = Vec::with_capacity(string_offset + records.iter().map(|r| r.value.len()).sum::<usize>());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(records.len() as u16).to_be_bytes());
    out.extend_from_slice(&(string_offset as u16).to_be_bytes());

    let mut string_data = Vec::new();
    for record in records {
        out.extend_from_slice(&record.platform.to_be_bytes());
        out.extend_from_slice(&record.encoding.to_be_bytes());
        out.extend_from_slice(&record.language.to_be_bytes());
        out.extend_from_slice(&record.name_id.to_be_bytes());
        out.extend_from_slice(&(record.value.len() as u16).to_be_bytes());
        out.extend_from_slice(&(string_data.len() as u16).to_be_bytes());
        string_data.extend_from_slice(&record.value);
    }
    out.extend_from_slice(&string_data);
    out
}

fn table_checksum(data: &[u8]) -> u32 {
    let mut checksum = 0u32;
    for chunk in data.chunks(4) {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        checksum = checksum.wrapping_add(u32::from_be_bytes(word));
    }
    checksum
}

#[derive(Debug, Clone, Copy)]
struct StyleInfo<'a> {
    font_name: &'a str,
    bold: i32,
    italic: bool,
}

/// Rewrite ASS style font names and `\fn` override tags to the mapped names.
///
/// The map is keyed by normalized original font name + bold/italic state, as
/// resolved during processing. Unmapped names are left untouched.
pub fn rename_ass_fonts(content: &str, renames: &HashMap<RenameKey, String>) -> String {
    if renames.is_empty() {
        return content.to_string();
    }

    let styles = collect_styles(content);
    let mut out_lines: Vec<String> = Vec::new();
    let mut section = "";
    let mut style_fields: Option<Vec<String>> = None;
    let mut event_fields: Option<Vec<String>> = None;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = trimmed;
            out_lines.push(line.to_string());
            continue;
        }

        if is_styles_section(section) {
            if let Some(rest) = trimmed.strip_prefix("Format:") {
                style_fields = Some(parse_format_fields(rest));
                out_lines.push(line.to_string());
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("Style:") {
                out_lines.push(rename_style_line(rest, style_fields.as_ref(), renames));
                continue;
            }
        }

        if section.eq_ignore_ascii_case("[Events]") {
            if let Some(rest) = trimmed.strip_prefix("Format:") {
                event_fields = Some(parse_format_fields(rest));
                out_lines.push(line.to_string());
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("Dialogue:") {
                let fields = event_fields.as_ref();
                out_lines.push(format!("Dialogue:{}", rename_dialogue_line(rest, fields, &styles, renames)));
                continue;
            }
        }

        out_lines.push(line.to_string());
    }

    let mut out = out_lines.join("\n");
    if content.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn is_styles_section(section: &str) -> bool {
    section.eq_ignore_ascii_case("[V4+ Styles]") || section.eq_ignore_ascii_case("[V4 Styles]")
}

fn parse_format_fields(rest: &str) -> Vec<String> {
    rest.split(',')
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .map(|field| field.to_ascii_lowercase())
        .collect()
}

fn field_index(fields: Option<&Vec<String>>, name: &str, default: usize) -> Option<usize> {
    fields.and_then(|fields| {
        fields
            .iter()
            .position(|field| field == name)
            .or(Some(default))
    })
}

fn rename_style_line(
    rest: &str,
    style_fields: Option<&Vec<String>>,
    renames: &HashMap<RenameKey, String>,
) -> String {
    let field_count = style_fields.map(|fields| fields.len()).unwrap_or(23);
    let name_index = field_index(style_fields, "name", 0).unwrap_or(0);
    let font_index = field_index(style_fields, "fontname", 1).unwrap_or(1);
    let bold_index = field_index(style_fields, "bold", 7);
    let italic_index = field_index(style_fields, "italic", 8);

    let mut columns: Vec<String> = rest.splitn(field_count, ',').map(str::to_string).collect();
    if columns.len() <= font_index.max(name_index) {
        return format!("Style:{rest}");
    }

    let bold = bold_index
        .and_then(|index| columns.get(index))
        .and_then(|value| value.trim().parse::<i32>().ok())
        .map(normalize_bold)
        .unwrap_or(400);
    let italic = italic_index
        .and_then(|index| columns.get(index))
        .and_then(|value| value.trim().parse::<i32>().ok())
        .map(|value| matches!(value, -1 | 1))
        .unwrap_or(false);

    let font_value = columns[font_index].trim();
    let vertical = font_value.starts_with('@');
    let base = font_value.trim_start_matches('@');
    let key = RenameKey::new(base, bold, italic);
    if let Some(new_name) = renames.get(&key) {
        columns[font_index] = if vertical {
            format!("@{new_name}")
        } else {
            new_name.clone()
        };
    }

    format!("Style:{}", columns.join(","))
}

fn rename_dialogue_line(
    rest: &str,
    event_fields: Option<&Vec<String>>,
    styles: &HashMap<String, StyleInfo<'_>>,
    renames: &HashMap<RenameKey, String>,
) -> String {
    let field_count = event_fields.map(|fields| fields.len()).unwrap_or(10);
    let text_index = field_index(event_fields, "text", 9).unwrap_or(9);
    let style_index = field_index(event_fields, "style", 3).unwrap_or(3);

    let mut columns: Vec<String> = rest.splitn(field_count, ',').map(str::to_string).collect();
    if columns.len() <= text_index.max(style_index) {
        return rest.to_string();
    }

    let style_name = columns[style_index].trim();
    let renamed = rename_dialogue_text(&columns[text_index], style_name, styles, renames);
    columns[text_index] = renamed;
    columns.join(",")
}

fn rename_dialogue_text(
    text: &str,
    style_name: &str,
    styles: &HashMap<String, StyleInfo<'_>>,
    renames: &HashMap<RenameKey, String>,
) -> String {
    let default = StyleInfo {
        font_name: "Default",
        bold: 400,
        italic: false,
    };
    let style = styles.get(style_name).copied().unwrap_or(default);

    let mut current_font = style.font_name.to_string();
    let mut current_bold = style.bold;
    let mut current_italic = style.italic;

    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        let Some(close_rel) = after_open.find('}') else {
            out.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let block = &after_open[..close_rel];
        rest = &after_open[close_rel + 1..];

        if let Some(new_block) = rename_override_block(
            block,
            &mut current_font,
            &mut current_bold,
            &mut current_italic,
            style,
            styles,
            renames,
        ) {
            out.push('{');
            out.push_str(&new_block);
            out.push('}');
        } else {
            out.push('{');
            out.push_str(block);
            out.push('}');
        }
    }
    out.push_str(rest);
    out
}

/// Update the running style state from one override block and return the
/// block content with the `\fn` name replaced when applicable.
#[allow(clippy::too_many_arguments)]
fn rename_override_block(
    block: &str,
    current_font: &mut String,
    current_bold: &mut i32,
    current_italic: &mut bool,
    style: StyleInfo<'_>,
    styles: &HashMap<String, StyleInfo<'_>>,
    renames: &HashMap<RenameKey, String>,
) -> Option<String> {
    // Tokenize: ASS tags have no separator before their value (`\fnArial`),
    // so match the known `fn` prefix and treat any other leading letter as a
    // single-char tag; unknown tags fail value parsing and are ignored.
    let mut tokens: Vec<(&str, usize, usize)> = Vec::new(); // tag, value start, value end
    let bytes = block.as_bytes();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor] != b'\\' {
            cursor += 1;
            continue;
        }
        cursor += 1;
        if cursor >= bytes.len() {
            break;
        }
        let tag: &str = if bytes[cursor..].starts_with(b"fn") {
            cursor += 2;
            "fn"
        } else if bytes[cursor].is_ascii_alphabetic() {
            let start = cursor;
            cursor += 1;
            &block[start..cursor]
        } else {
            continue;
        };
        let value_start = cursor;
        while cursor < bytes.len() && bytes[cursor] != b'\\' {
            cursor += 1;
        }
        tokens.push((tag, value_start, cursor));
    }

    if tokens.is_empty() {
        return None;
    }

    let mut last_fn: Option<(usize, String)> = None; // token index, name (may be empty)
    let mut last_r: Option<usize> = None;

    for (index, (tag, value_start, value_end)) in tokens.iter().enumerate() {
        let value = block[*value_start..*value_end].trim();
        match *tag {
            "fn" => {
                if value.is_empty() {
                    *current_font = style.font_name.to_string();
                } else {
                    *current_font = value.to_string();
                }
                last_fn = Some((index, value.to_string()));
            }
            "b" => {
                if let Ok(parsed) = value.parse::<i32>() {
                    *current_bold = normalize_bold(parsed);
                }
            }
            "i" => {
                if let Ok(parsed) = value.parse::<i32>() {
                    *current_italic = matches!(parsed, -1 | 1);
                }
            }
            "r" => {
                let target = if value.is_empty() {
                    Some(style)
                } else {
                    styles.get(value).copied()
                };
                if let Some(target) = target {
                    *current_font = target.font_name.to_string();
                    *current_bold = target.bold;
                    *current_italic = target.italic;
                }
                last_r = Some(index);
            }
            _ => {}
        }
    }

    // The `\fn` name is only rewritten when it wins over a later `\r`.
    let (fn_index, fn_name) = last_fn?;
    if fn_name.is_empty() {
        return None;
    }
    if last_r.is_some_and(|r_index| r_index > fn_index) {
        return None;
    }

    let key = RenameKey {
        name: normalize_font_name(&fn_name),
        bold: *current_bold,
        italic: *current_italic,
    };
    let new_name = renames.get(&key)?;

    let (_, value_start, value_end) = tokens[fn_index];
    let mut new_block = String::with_capacity(block.len());
    new_block.push_str(&block[..value_start]);
    if fn_name.starts_with('@') {
        new_block.push('@');
    }
    new_block.push_str(new_name);
    new_block.push_str(&block[value_end..]);
    Some(new_block)
}

fn collect_styles(content: &str) -> HashMap<String, StyleInfo<'_>> {
    let mut styles = HashMap::new();
    let mut section = "";
    let mut fields: Option<Vec<String>> = None;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = trimmed;
            continue;
        }
        if !is_styles_section(section) {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Format:") {
            fields = Some(parse_format_fields(rest));
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("Style:") else {
            continue;
        };

        let field_count = fields.as_ref().map(|fields| fields.len()).unwrap_or(23);
        let name_index = field_index(fields.as_ref(), "name", 0).unwrap_or(0);
        let font_index = field_index(fields.as_ref(), "fontname", 1).unwrap_or(1);
        let bold_index = field_index(fields.as_ref(), "bold", 7);
        let italic_index = field_index(fields.as_ref(), "italic", 8);

        let columns: Vec<&str> = rest.splitn(field_count, ',').map(str::trim).collect();
        if columns.len() <= font_index.max(name_index) {
            continue;
        }
        let bold = bold_index
            .and_then(|index| columns.get(index))
            .and_then(|value| value.parse::<i32>().ok())
            .map(normalize_bold)
            .unwrap_or(400);
        let italic = italic_index
            .and_then(|index| columns.get(index))
            .and_then(|value| value.parse::<i32>().ok())
            .map(|value| matches!(value, -1 | 1))
            .unwrap_or(false);
        styles.insert(
            columns[name_index].to_string(),
            StyleInfo {
                font_name: columns[font_index],
                bold,
                italic,
            },
        );
    }
    styles
}

/// Insert `; Font Subset: <new> - <original>` lines right after the first
/// `[Script Info]` header line, recording which original font each CRC name
/// replaced (idempotency provenance, mirroring the ASFMKV Python tool).
pub fn write_subset_records(content: &str, records: &[(String, String)]) -> String {
    if records.is_empty() {
        return content.to_string();
    }
    let mut out: Vec<String> = Vec::new();
    let mut inserted = false;
    for line in content.lines() {
        out.push(line.to_string());
        if !inserted && line.trim().eq_ignore_ascii_case("[Script Info]") {
            for (new, original) in records {
                out.push(format!("; Font Subset: {new} - {original}"));
            }
            inserted = true;
        }
    }
    if !inserted {
        out.insert(0, "[Script Info]".to_string());
        for (offset, (new, original)) in records.iter().enumerate() {
            out.insert(1 + offset, format!("; Font Subset: {new} - {original}"));
        }
    }
    let mut text = out.join("\n");
    if content.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// Parse `; Font Subset: <new> - <original>` lines into `(new, original)` pairs.
/// A dash inside the original name is preserved (everything after the first
/// `- ` separator is the original, mirroring the Python parser).
pub fn parse_subset_records(content: &str) -> Vec<(String, String)> {
    content
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if !trimmed.starts_with(';') {
                return None;
            }
            let rest = trimmed[1..].trim();
            if !rest.to_ascii_lowercase().starts_with("font subset") {
                return None;
            }
            let after = rest["font subset".len()..].trim();
            let after = after.strip_prefix(':')?.trim();
            let (new, original) = after.split_once('-')?;
            Some((new.trim().to_string(), original.trim().to_string()))
        })
        .collect()
}

/// Map renamed fonts back to their original names using the
/// `; Font Subset: <new> - <original>` records, then strip those record
/// lines. Re-processing a renamed `.subset.ass` then matches the original
/// system font again (idempotent re-run); fresh records are written on output.
///
/// The records carry no bold/italic state, so each name pair is expanded to
/// all four weight/italic key combinations — matching uses the request's own
/// state, exactly like the original run.
pub fn apply_subset_records_reverse(content: &str) -> String {
    let records = parse_subset_records(content);
    if records.is_empty() {
        return content.to_string();
    }

    let mut renames: HashMap<RenameKey, String> = HashMap::new();
    for (new, original) in &records {
        for (bold, italic) in [(400, false), (700, false), (400, true), (700, true)] {
            renames.insert(RenameKey::new(new, bold, italic), original.clone());
        }
    }

    let restored = rename_ass_fonts(content, &renames);
    strip_subset_records(&restored)
}

/// Remove `; Font Subset:` comment lines.
pub fn strip_subset_records(content: &str) -> String {
    let mut out = content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !(trimmed.starts_with(';')
                && trimmed[1..]
                    .trim()
                    .to_ascii_lowercase()
                    .starts_with("font subset"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    if content.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        RenameKey, apply_subset_records_reverse, compute_subset_crc, crc32, parse_subset_records,
        rename_ass_fonts, rename_font_names, write_subset_records,
    };

    #[test]
    fn subset_records_reverse_restores_names() {
        // Rename Arial -> AAAA1111 on an italic style, then reverse it.
        let source = "[Script Info]\nTitle: demo\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, Bold, Italic\nStyle: Default,Arial,56,0,0\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,{\\fnArial}hi\n";
        let map: HashMap<RenameKey, String> =
            [(RenameKey::new("Arial", 400, false), "AAAA1111".to_string())]
                .into_iter()
                .collect();
        let renamed = rename_ass_fonts(source, &map);
        let recorded = write_subset_records(&renamed, &[("AAAA1111".to_string(), "Arial".to_string())]);
        assert!(recorded.contains("Style: Default,AAAA1111,"));
        assert!(recorded.contains("{\\fnAAAA1111}hi"));

        // Reverse: names restored, record lines gone.
        let restored = apply_subset_records_reverse(&recorded);
        assert!(restored.contains("Style: Default,Arial,"));
        assert!(restored.contains("{\\fnArial}hi"));
        assert!(parse_subset_records(&restored).is_empty());
    }

    #[test]
    fn subset_records_roundtrip() {
        let records = vec![
            ("AAAA1111".to_string(), "Source Han Sans".to_string()),
            ("BBBB2222".to_string(), "Arial".to_string()),
            (
                "CCCC3333".to_string(),
                "Weird-Name-With-Dashes".to_string(),
            ),
        ];
        let content = "[Script Info]\nTitle: demo\n[V4+ Styles]\n";
        let written = write_subset_records(content, &records);
        assert!(written.contains("; Font Subset: AAAA1111 - Source Han Sans"));
        assert!(written.starts_with("[Script Info]\n; Font Subset: AAAA1111"));

        assert_eq!(parse_subset_records(&written), records);
    }

    #[test]
    fn subset_records_insert_header_when_missing() {
        let records = vec![("AAAA1111".to_string(), "Arial".to_string())];
        let out = write_subset_records("Dialogue: x\n", &records);
        assert!(out.starts_with("[Script Info]\n; Font Subset: AAAA1111 - Arial\n"));
        assert_eq!(parse_subset_records(&out), records);
    }
    use std::collections::HashMap;

    fn renames(entries: &[(&str, i32, bool, &str)]) -> HashMap<RenameKey, String> {
        entries
            .iter()
            .map(|(name, bold, italic, new)| {
                (
                    RenameKey::new(name, *bold, *italic),
                    new.to_string(),
                )
            })
            .collect()
    }

    const SAMPLE: &str = "[Script Info]\nTitle: demo\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,56,&H00FFFFFF,&H000000FF,&H00000000,&H64000000,0,-1,0,0,100,100,0,0,1,3,2,2,24,24,36,1\nStyle: Alt,Times New Roman,24,&H00FFFFFF,&H000000FF,&H00000000,&H50000000,-1,0,0,0,100,100,0,0,1,3,2,2,24,24,36,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:02.50,Default,,0,0,0,,plain text\nDialogue: 0,0:00:02.50,0:00:05.00,Default,,0,0,0,,{\\fnArial}override\nDialogue: 0,0:00:02.50,0:00:05.00,Default,,0,0,0,,{\\b1\\fnArial}bold face\nDialogue: 0,0:00:02.50,0:00:05.00,Default,,0,0,0,,{\\fnArial\\rAlt}reset wins\nDialogue: 0,0:00:02.50,0:00:05.00,Default,,0,0,0,,{\\i1\\fn}empty fn\n";

    #[test]
    fn crc_is_deterministic_and_formatted() {
        let a = compute_subset_crc("hello", "Arial");
        let b = compute_subset_crc("hello", "Arial");
        let c = compute_subset_crc("hellp", "Arial");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 8);
        assert!(a.chars().all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_lowercase()));
    }

    #[test]
    fn crc32_matches_known_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn rewrites_styles_and_fn_tags() {
        let map = renames(&[
            ("Arial", 400, true, "AAAA1111"),
            ("Arial", 700, true, "BBBB2222"),
        ]);
        let out = rename_ass_fonts(SAMPLE, &map);

        // Style Default is italic (-1) -> AAAA1111; @ prefix preserved when present.
        assert!(out.contains("Style: Default,AAAA1111,"));
        // Style Alt (Times New Roman, normal) is not mapped.
        assert!(out.contains("Style: Alt,Times New Roman,"));
        // \fn with normal weight (bold state 400 from style) -> AAAA1111.
        assert!(out.contains("{\\fnAAAA1111}override"));
        // \b1 before \fn -> bold state -> BBBB2222.
        assert!(out.contains("{\\b1\\fnBBBB2222}bold face"));
        // \r after \fn wins -> \fn left untouched.
        assert!(out.contains("{\\fnArial\\rAlt}reset wins"));
        // Empty \fn is never rewritten.
        assert!(out.contains("{\\i1\\fn}empty fn"));
        // Literal text outside tags untouched.
        assert!(out.contains("Dialogue: 0,0:00:00.00,0:00:02.50,Default,,0,0,0,,plain text"));
    }

    #[test]
    fn rewrites_vertical_prefix() {
        let content = "[V4+ Styles]\nStyle: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,3,2,2,24,24,36,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,{\\fn@Arial}v\n";
        let map = renames(&[("Arial", 400, false, "CCCC3333")]);
        let out = rename_ass_fonts(content, &map);
        assert!(out.contains("{\\fn@CCCC3333}v"));
        // The style itself has no @ and is renamed without prefix.
        assert!(out.contains("Style: Default,CCCC3333,"));
    }

    #[test]
    fn unknown_names_are_untouched() {
        let map = renames(&[("Arial", 400, false, "AAAA1111")]);
        let out = rename_ass_fonts(SAMPLE, &map);
        // The Default style is italic (-1), so its key does not match this map.
        assert!(out.contains("Style: Default,Arial,"));
        // Style state for the override line is italic too -> untouched.
        assert!(out.contains("{\\fnArial}override"));
        // Bold state (700) is not mapped either.
        assert!(out.contains("{\\b1\\fnArial}bold face"));
    }

    fn be16(data: &[u8], at: usize) -> u16 {
        u16::from_be_bytes([data[at], data[at + 1]])
    }

    fn build_test_sfnt() -> Vec<u8> {
        // name table: one Windows record (3,1,0x409) with NameID 1 = "Test".
        let mut name = Vec::new();
        name.extend_from_slice(&0u16.to_be_bytes());
        name.extend_from_slice(&1u16.to_be_bytes());
        name.extend_from_slice(&6u16.to_be_bytes());
        name.extend_from_slice(&3u16.to_be_bytes());
        name.extend_from_slice(&1u16.to_be_bytes());
        name.extend_from_slice(&0x409u16.to_be_bytes());
        name.extend_from_slice(&1u16.to_be_bytes());
        name.extend_from_slice(&8u16.to_be_bytes());
        name.extend_from_slice(&0u16.to_be_bytes());
        name.extend_from_slice(&"Test".encode_utf16().flat_map(u16::to_be_bytes).collect::<Vec<u8>>());

        let mut head = vec![0u8; 54];
        head[8..12].copy_from_slice(&0x1234_5678u32.to_be_bytes());

        let mut out = Vec::new();
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        out.extend_from_slice(&2u16.to_be_bytes());
        out.extend_from_slice(&16u16.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        // tables sorted by tag: head, name
        let mut offset = 12 + 2 * 16;
        for (tag, data) in [(*b"head", &head), (*b"name", &name)] {
            out.extend_from_slice(&u32::from_be_bytes(tag).to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(&(offset as u32).to_be_bytes());
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            offset += (data.len() + 3) & !3;
        }
        for (_, data) in [(*b"head", &head), (*b"name", &name)] {
            out.extend_from_slice(data);
            while out.len() % 4 != 0 {
                out.push(0);
            }
        }
        out
    }

    #[test]
    fn renames_sfnt_name_table() {
        let font = build_test_sfnt();
        let renamed = rename_font_names(&font, "ABCD1234").unwrap();

        // Locate the name table in the output.
        let num_tables = be16(&renamed, 4) as usize;
        let mut name_at = None;
        let mut head_at = None;
        for index in 0..num_tables {
            let record = 12 + index * 16;
            let tag = u32::from_be_bytes([
                renamed[record],
                renamed[record + 1],
                renamed[record + 2],
                renamed[record + 3],
            ]);
            let offset = u32::from_be_bytes([
                renamed[record + 8],
                renamed[record + 9],
                renamed[record + 10],
                renamed[record + 11],
            ]) as usize;
            match &tag.to_be_bytes() {
                b"name" => name_at = Some(offset),
                b"head" => head_at = Some(offset),
                _ => {}
            }
        }
        let name_at = name_at.expect("name table present");
        let count = be16(&renamed, name_at + 2) as usize;
        let string_offset = be16(&renamed, name_at + 4) as usize;
        assert_eq!(count, 1);
        let record = name_at + 6;
        assert_eq!(be16(&renamed, record + 6), 1); // NameID 1
        let length = be16(&renamed, record + 8) as usize;
        let start = name_at + string_offset + be16(&renamed, record + 10) as usize;
        let value = &renamed[start..start + length];
        let decoded: String = value
            .chunks(2)
            .map(|pair| char::from_u32(u16::from_be_bytes([pair[0], pair[1]]) as u32).unwrap())
            .collect();
        assert_eq!(decoded, "ABCD1234");

        // head.checkSumAdjustment satisfies: adjustment + checksum(font) == MAGIC
        // where the checksum is taken over the font with the field zeroed.
        let head_at = head_at.expect("head table present");
        let adjustment = u32::from_be_bytes([
            renamed[head_at + 8],
            renamed[head_at + 9],
            renamed[head_at + 10],
            renamed[head_at + 11],
        ]);
        let mut zeroed = renamed.clone();
        zeroed[head_at + 8..head_at + 12].copy_from_slice(&0u32.to_be_bytes());
        assert_eq!(adjustment, 0xB1B0_AFBAu32.wrapping_sub(crc32(&zeroed)));
    }
}
