//! Minimal markdown renderer for assistant chat messages. Supports the
//! subset models actually emit: headings, bullet / numbered lists, fenced
//! code blocks, and inline **bold**, *italic* and `code` spans. Anything
//! else falls through as plain text.

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, TextStyle};

use crate::theme;

/// Colors + fonts resolved once per message.
struct Style {
    body:    FontId,
    text:    Color32,
    strong:  Color32,
    code:    Color32,
    code_bg: Color32,
}

pub fn render(ui: &mut egui::Ui, pal: &theme::Palette, text: &str) {
    ui.spacing_mut().item_spacing.y = 4.0;
    let style = Style {
        body:    TextStyle::Body.resolve(ui.style()),
        text:    pal.text_primary,
        strong:  ui.visuals().strong_text_color(),
        code:    pal.key,
        code_bg: pal.bg_search,
    };

    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();

        // ── fenced code block ──
        if trimmed.starts_with("```") {
            let mut code = String::new();
            for l in lines.by_ref() {
                if l.trim_start().starts_with("```") {
                    break;
                }
                code.push_str(l);
                code.push('\n');
            }
            code_block(ui, pal, code.trim_end_matches('\n'));
            continue;
        }

        if trimmed.is_empty() {
            ui.add_space(2.0);
            continue;
        }

        // ── heading ──
        if let Some((level, body)) = heading(trimmed) {
            let size = match level {
                1 => 17.0,
                2 => 15.5,
                _ => 14.0,
            };
            let mut job = LayoutJob::default();
            inline_spans(&mut job, &style, body, Some(FontId::proportional(size)), true);
            ui.add_space(2.0);
            ui.label(job);
            continue;
        }

        // ── list item ──
        let indent = (line.len() - trimmed.len()) as f32 / 2.0;
        if let Some(body) = bullet(trimmed) {
            list_item(ui, &style, indent, "•  ", body);
            continue;
        }
        if let Some((num, body)) = numbered(trimmed) {
            list_item(ui, &style, indent, &format!("{num}. "), body);
            continue;
        }

        // ── plain paragraph line ──
        let mut job = LayoutJob::default();
        inline_spans(&mut job, &style, line, None, false);
        ui.label(job);
    }
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|&b| b == b'#').count();
    if (1..=6).contains(&level) {
        line[level..].strip_prefix(' ').map(|rest| (level, rest))
    } else {
        None
    }
}

fn bullet(line: &str) -> Option<&str> {
    for pre in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(pre) {
            return Some(rest);
        }
    }
    None
}

fn numbered(line: &str) -> Option<(&str, &str)> {
    let digits = line.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 || digits > 3 {
        return None;
    }
    line[digits..]
        .strip_prefix(". ")
        .map(|rest| (&line[..digits], rest))
}

fn list_item(ui: &mut egui::Ui, style: &Style, indent: f32, marker: &str, body: &str) {
    let mut job = LayoutJob::default();
    job.append(
        marker,
        8.0 + indent * 12.0,
        TextFormat {
            font_id: style.body.clone(),
            color: style.text,
            ..Default::default()
        },
    );
    inline_spans(&mut job, style, body, None, false);
    ui.label(job);
}

fn code_block(ui: &mut egui::Ui, pal: &theme::Palette, code: &str) {
    egui::Frame::new()
        .fill(pal.bg_search)
        .stroke(egui::Stroke::new(1.0_f32, pal.border))
        .corner_radius(4.0)
        .inner_margin(egui::Margin::same(6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new(code)
                    .monospace()
                    .color(pal.text_primary),
            );
        });
}

/// Append `text` to `job`, resolving `code`, **bold** and *italic* spans.
fn inline_spans(
    job: &mut LayoutJob,
    style: &Style,
    text: &str,
    font: Option<FontId>,
    heading: bool,
) {
    let font = font.unwrap_or_else(|| style.body.clone());
    let mono = FontId::monospace((font.size - 1.0).max(10.0));
    let mut bold = heading;
    let mut italic = false;
    let mut buf = String::new();

    let flush = |job: &mut LayoutJob, buf: &mut String, bold: bool, italic: bool| {
        if buf.is_empty() {
            return;
        }
        let mut fmt = TextFormat {
            font_id: font.clone(),
            color: if bold { style.strong } else { style.text },
            ..Default::default()
        };
        fmt.italics = italic;
        job.append(buf, 0.0, fmt);
        buf.clear();
    };

    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // `code span`
        if bytes[i] == b'`' {
            if let Some(end) = text[i + 1..].find('`') {
                flush(job, &mut buf, bold, italic);
                job.append(
                    &text[i + 1..i + 1 + end],
                    0.0,
                    TextFormat {
                        font_id: mono.clone(),
                        color: style.code,
                        background: style.code_bg,
                        ..Default::default()
                    },
                );
                i += end + 2;
                continue;
            }
        }
        // **bold**
        if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'*') {
            flush(job, &mut buf, bold, italic);
            bold = !bold;
            i += 2;
            continue;
        }
        // *italic*
        if bytes[i] == b'*' {
            flush(job, &mut buf, bold, italic);
            italic = !italic;
            i += 1;
            continue;
        }
        let ch_len = text[i..].chars().next().map_or(1, char::len_utf8);
        buf.push_str(&text[i..i + ch_len]);
        i += ch_len;
    }
    flush(job, &mut buf, bold, italic);
}
