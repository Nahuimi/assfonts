#[derive(Debug, Clone)]
pub struct EmbeddedFont {
    pub font_name: String,
    pub bytes: Vec<u8>,
}

pub fn uuencode(input: &[u8], insert_linebreaks: bool) -> String {
    let size = input.len();
    let mut output = String::with_capacity((size * 4).div_ceil(3) + size / 80 * 2);
    let mut written = 0usize;

    let mut pos = 0usize;
    while pos < size {
        let remaining = size - pos;
        let src0 = input[pos];
        let src1 = if remaining > 1 { input[pos + 1] } else { 0 };
        let src2 = if remaining > 2 { input[pos + 2] } else { 0 };

        let dst = [
            src0 >> 2,
            ((src0 & 0x3) << 4) | ((src1 & 0xF0) >> 4),
            ((src1 & 0xF) << 2) | ((src2 & 0xC0) >> 6),
            src2 & 0x3F,
        ];

        let write_count = usize::min(remaining + 1, 4);
        for value in dst.iter().take(write_count) {
            output.push((value + 33) as char);
            written += 1;
            if insert_linebreaks && written == 80 && pos + 3 < size {
                written = 0;
                output.push('\n');
            }
        }

        pos += 3;
    }

    output
}

pub fn render_ass_with_fonts(original: &str, fonts: &[EmbeddedFont]) -> String {
    if fonts.is_empty() {
        return original.to_string();
    }

    let lines: Vec<String> = original.lines().map(|x| x.to_string()).collect();
    let cleaned = remove_existing_fonts_section(&lines);
    let mut output = String::new();

    for line in cleaned.iter() {
        output.push_str(line);
        output.push('\n');
    }

    output.push_str("\n[Fonts]\n");
    for font in fonts {
        output.push_str("fontname: ");
        output.push_str(&font.font_name);
        output.push('\n');
        output.push_str(&uuencode(&font.bytes, true));
        output.push('\n');
    }

    output
}

/// Decode UUEncoded text back to bytes (inverse of [`uuencode`], aligned
/// with the ASFMKV Python `assDecodeUUE`): each 4-character group holds
/// 24 bits; a trailing 2-group yields 1 byte and a 3-group yields 2.
pub fn uudecode(input: &str) -> Vec<u8> {
    let sextets: Vec<u8> = input
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| ((c as u8).wrapping_sub(33)) & 0x3F)
        .collect();

    let mut out = Vec::with_capacity(sextets.len() * 3 / 4);
    let full_groups = sextets.len() / 4 * 4;
    for chunk in sextets[..full_groups].chunks(4) {
        let value = (chunk[0] as u32) << 18
            | (chunk[1] as u32) << 12
            | (chunk[2] as u32) << 6
            | chunk[3] as u32;
        out.extend_from_slice(&[(value >> 16) as u8, (value >> 8) as u8, value as u8]);
    }

    match sextets.len() - full_groups {
        2 => {
            let value = ((sextets[full_groups] as u32) << 6) | sextets[full_groups + 1] as u32;
            out.push((value >> 4) as u8);
        }
        3 => {
            let value = (sextets[full_groups] as u32) << 12
                | (sextets[full_groups + 1] as u32) << 6
                | sextets[full_groups + 2] as u32;
            out.extend_from_slice(&[(value >> 10) as u8, ((value >> 2) & 0xFF) as u8]);
        }
        _ => {}
    }

    out
}

/// Extract font attachments from an ASS `[Fonts]` section as
/// `(attachment name, decoded bytes)` pairs. The `[Graphics]` section is
/// ignored (images, not fonts).
pub fn extract_embedded_fonts(content: &str) -> Vec<(String, Vec<u8>)> {
    let mut fonts = Vec::new();
    let mut in_fonts_section = false;
    let mut current: Option<(String, String)> = None;

    let flush = |current: &mut Option<(String, String)>, fonts: &mut Vec<(String, Vec<u8>)>| {
        if let Some((name, data)) = current.take() {
            fonts.push((name, uudecode(&data)));
        }
    };

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            flush(&mut current, &mut fonts);
            in_fonts_section = trimmed.eq_ignore_ascii_case("[Fonts]");
            continue;
        }
        if !in_fonts_section {
            continue;
        }

        if trimmed.to_ascii_lowercase().starts_with("fontname:") {
            flush(&mut current, &mut fonts);
            let split_at = trimmed.to_ascii_lowercase().find(':').unwrap_or(0);
            let name = trimmed[split_at + 1..].trim().to_string();
            current = Some((name, String::new()));
        } else if !trimmed.is_empty()
            && let Some((_, data)) = current.as_mut()
        {
            data.push_str(trimmed);
        }
    }
    flush(&mut current, &mut fonts);
    fonts
}

fn remove_existing_fonts_section(lines: &[String]) -> Vec<String> {
    let mut output = Vec::with_capacity(lines.len());
    let mut skipping = false;

    for line in lines {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if trimmed.eq_ignore_ascii_case("[fonts]") {
                skipping = true;
                continue;
            }
            if skipping {
                skipping = false;
            }
        }

        if !skipping {
            output.push(line.clone());
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::{EmbeddedFont, extract_embedded_fonts, render_ass_with_fonts, uudecode, uuencode};

    #[test]
    fn uuencode_matches_known_result() {
        let encoded = uuencode(b"abc", false);
        assert_eq!(encoded, "97*D");
    }

    #[test]
    fn uudecode_roundtrips_and_matches_known_vector() {
        assert_eq!(uudecode("97*D"), b"abc");
        for sample in [
            &b""[..],
            b"a",
            b"ab",
            b"abc",
            b"abcd",
            b"hello embedded font world".as_slice(),
        ] {
            assert_eq!(uudecode(&uuencode(sample, true)), sample);
            assert_eq!(uudecode(&uuencode(sample, false)), sample);
        }
    }

    #[test]
    fn extracts_fonts_section_only() {
        let font = EmbeddedFont {
            font_name: "demo.ttf_0.ttf".to_string(),
            bytes: b"font-bytes-here".to_vec(),
        };
        let rendered = render_ass_with_fonts(
            "[Script Info]\nTitle: demo\n[Events]\nDialogue: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,hi",
            &[font],
        );

        let extracted = extract_embedded_fonts(&rendered);
        assert_eq!(extracted.len(), 1);
        assert_eq!(extracted[0].0, "demo.ttf_0.ttf");
        assert_eq!(extracted[0].1, b"font-bytes-here");

        // Non-font attachments are ignored.
        let graphics = "[Graphics]\nfilename: img.png\nAAAA\n[Events]\n";
        assert!(extract_embedded_fonts(graphics).is_empty());
    }

    #[test]
    fn inserts_fonts_section_at_end() {
        let source = "[Script Info]\nTitle: demo\n[Events]\nDialogue: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,hi";
        let fonts = vec![EmbeddedFont {
            font_name: "demo.ttf_0.ttf".to_string(),
            bytes: b"abc".to_vec(),
        }];

        let rendered = render_ass_with_fonts(source, &fonts);
        assert!(rendered.ends_with("\n[Fonts]\nfontname: demo.ttf_0.ttf\n97*D\n"));
        assert!(rendered.contains("[Events]"));
    }
}
