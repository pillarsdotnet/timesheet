// Copyright (c) 2025 Robert August Vincent II <pillarsdotnet@gmail.com>
// Co-author: Claude-AI.

//! Fills a form-fillable PDF template.
//!
//! Appearance streams are generated here rather than left to the viewer's
//! `/NeedAppearances` handling, so the text is visible in every reader, when printed, and to
//! text extractors. That means laying the text out by hand: measuring it against the
//! Helvetica widths below, wrapping it, shrinking it to fit the cell, and emitting the
//! content stream that draws it.

use lopdf::{Dictionary, Document, Object, ObjectId, Stream, StringFormat};
use std::collections::HashMap;
use std::path::Path;

/// Advance widths (1/1000 em) for Helvetica/WinAnsiEncoding, code points 32..=255. Embedded
/// so that measuring text needs no font library at run time.
const HELVETICA_WIDTHS: [u16; 224] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667,
    611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500,
    222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
    350, 556, 350, 222, 556, 333, 1000, 556, 556, 333, 1000, 667, 333, 1000, 350, 611, 350, 350,
    222, 222, 333, 333, 350, 556, 1000, 333, 1000, 500, 333, 944, 350, 500, 667, 278, 333, 556,
    556, 556, 556, 260, 556, 333, 737, 370, 556, 584, 333, 737, 333, 400, 584, 333, 333, 333, 556,
    537, 278, 333, 333, 365, 556, 834, 834, 834, 611, 667, 667, 667, 667, 667, 667, 1000, 722, 667,
    667, 667, 667, 278, 278, 278, 278, 722, 722, 778, 778, 778, 778, 778, 584, 778, 722, 722, 722,
    722, 667, 667, 611, 556, 556, 556, 556, 556, 556, 889, 500, 556, 556, 556, 556, 278, 278, 278,
    278, 556, 556, 556, 556, 556, 556, 556, 584, 611, 556, 556, 556, 556, 500, 556, 500,
];

/// Text alignment, matching the PDF `/Q` field values.
#[derive(Clone, Copy, PartialEq)]
pub enum Align {
    Left = 0,
    Center = 1,
}

/// `/Ff` bit 13: the text field wraps across lines.
const MULTILINE_FLAG: i64 = 1 << 12;

/// Leading as a multiple of the font size, matching the ratio form viewers use.
const LEADING: f64 = 1.15;

/// Inset from the cell edge, in points.
const PAD: f64 = 2.0;

/// The Unicode characters that WinAnsiEncoding places in 0x80..=0x9F, where it and
/// Latin-1 disagree. Mapping them means a smart quote or an em dash pasted into an activity
/// name still renders instead of turning into a substitution mark.
const CP1252_HIGH: [(char, u8); 27] = [
    ('\u{20AC}', 0x80),
    ('\u{201A}', 0x82),
    ('\u{0192}', 0x83),
    ('\u{201E}', 0x84),
    ('\u{2026}', 0x85),
    ('\u{2020}', 0x86),
    ('\u{2021}', 0x87),
    ('\u{02C6}', 0x88),
    ('\u{2030}', 0x89),
    ('\u{0160}', 0x8A),
    ('\u{2039}', 0x8B),
    ('\u{0152}', 0x8C),
    ('\u{017D}', 0x8E),
    ('\u{2018}', 0x91),
    ('\u{2019}', 0x92),
    ('\u{201C}', 0x93),
    ('\u{201D}', 0x94),
    ('\u{2022}', 0x95),
    ('\u{2013}', 0x96),
    ('\u{2014}', 0x97),
    ('\u{02DC}', 0x98),
    ('\u{2122}', 0x99),
    ('\u{0161}', 0x9A),
    ('\u{203A}', 0x9B),
    ('\u{0153}', 0x9C),
    ('\u{017E}', 0x9E),
    ('\u{0178}', 0x9F),
];

/// Encodes text as WinAnsi bytes, the encoding the appearance streams and the embedded width
/// table both assume. A character with no WinAnsi equivalent becomes `?`.
pub fn to_winansi(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| {
            let code = c as u32;
            if (0x20..0x7F).contains(&code) || (0xA0..=0xFF).contains(&code) {
                return code as u8;
            }
            CP1252_HIGH
                .iter()
                .find(|(ch, _)| *ch == c)
                .map(|(_, b)| *b)
                .unwrap_or(b'?')
        })
        .collect()
}

/// Rendered width of WinAnsi `text` in points at `size`, for Helvetica.
fn text_width(text: &[u8], size: f64) -> f64 {
    let thousandths: u32 = text
        .iter()
        .map(|&b| {
            HELVETICA_WIDTHS
                .get(b.wrapping_sub(32) as usize)
                .copied()
                .unwrap_or(556) as u32
        })
        .sum();
    thousandths as f64 * size / 1000.0
}

/// Greedy word wrap, splitting any single word too long for the line.
fn wrap(text: &[u8], size: f64, width: f64) -> Vec<Vec<u8>> {
    let mut lines: Vec<Vec<u8>> = Vec::new();
    for paragraph in text.split(|&b| b == b'\n') {
        let mut current: Vec<u8> = Vec::new();
        for word in paragraph.split(|b| b.is_ascii_whitespace()) {
            if word.is_empty() {
                continue;
            }
            if current.is_empty() {
                current.extend_from_slice(word);
            } else {
                let mut trial = current.clone();
                trial.push(b' ');
                trial.extend_from_slice(word);
                if text_width(&trial, size) > width {
                    lines.push(std::mem::take(&mut current));
                    current.extend_from_slice(word);
                } else {
                    current = trial;
                }
            }
            // A word wider than the whole line is broken rather than left to overflow.
            while text_width(&current, size) > width && current.len() > 1 {
                let mut cut = current.len() - 1;
                while cut > 1 && text_width(&current[..cut], size) > width {
                    cut -= 1;
                }
                let rest = current.split_off(cut);
                lines.push(std::mem::take(&mut current));
                current = rest;
            }
        }
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(Vec::new());
    }
    lines
}

/// The largest font size in `[low, high]` at which the text fits, and the lines it produces.
///
/// A multi-line field wraps and is bounded by the cell height; a single-line field shrinks
/// until it fits the cell width. Sizes step down by half a point, as the reference
/// implementation does, so the result is stable rather than continuously varying.
fn fit(
    text: &[u8],
    width: f64,
    height: f64,
    low: f64,
    high: f64,
    multiline: bool,
) -> (f64, Vec<Vec<u8>>) {
    let mut size = high;
    loop {
        let lines = if multiline {
            wrap(text, size, width)
        } else {
            vec![text.to_vec()]
        };
        let fits = if multiline {
            lines.len() as f64 * size * LEADING <= height
        } else {
            text_width(text, size) <= width
        };
        if fits || size <= low {
            return (size, lines);
        }
        size -= 0.5;
    }
}

/// Escapes WinAnsi bytes for a PDF literal string in a content stream.
fn escape_pdf_string(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for &b in text {
        if matches!(b, b'\\' | b'(' | b')') {
            out.push(b'\\');
        }
        out.push(b);
    }
    out
}

/// A PDF text string for a field's `/V`. ASCII goes out as a literal; anything else uses
/// UTF-16BE with a byte-order mark, which is the only way a PDF text string carries
/// characters outside PDFDocEncoding.
fn pdf_text_string(text: &str) -> Object {
    if text.is_ascii() {
        return Object::String(text.as_bytes().to_vec(), StringFormat::Literal);
    }
    let mut bytes = vec![0xFE, 0xFF];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    Object::String(bytes, StringFormat::Literal)
}

/// Builds the `/AP /N` form XObject that draws `lines` inside a cell of `width` by `height`.
fn appearance_stream(
    width: f64,
    height: f64,
    lines: &[Vec<u8>],
    size: f64,
    align: Align,
    multiline: bool,
    font: ObjectId,
) -> Stream {
    let leading = size * LEADING;
    let inner = width - 2.0 * PAD;
    let mut ops: Vec<u8> = Vec::new();
    /// Appends one content-stream operator line.
    fn push(ops: &mut Vec<u8>, op: &str) {
        ops.extend_from_slice(op.as_bytes());
        ops.push(b'\n');
    }
    push(&mut ops, "/Tx BMC");
    push(&mut ops, "q");
    push(
        &mut ops,
        &format!(
            "{:.2} {:.2} {:.2} {:.2} re W n",
            PAD,
            PAD,
            inner,
            height - 2.0 * PAD
        ),
    );
    push(&mut ops, "BT");
    push(&mut ops, &format!("/Helv {} Tf", trim_number(size)));
    push(&mut ops, "0 g");

    let baseline = if multiline {
        // Centre the whole block vertically so descriptions line up with the hours figure
        // beside them, but never start above the top inset.
        let top = ((height + lines.len() as f64 * leading) / 2.0).min(height - PAD);
        top - size * 0.85
    } else {
        // Centre a single line on its cap height, as form viewers do.
        (height - size * 0.717) / 2.0
    };

    // Td is relative to the start of the previous line, so each step carries the horizontal
    // difference this line's alignment needs plus one line of leading.
    let mut previous_x = 0.0;
    for (offset, line) in lines.iter().enumerate() {
        let x = match align {
            Align::Center => PAD + (inner - text_width(line, size)) / 2.0,
            Align::Left => PAD,
        };
        let dy = if offset == 0 { baseline } else { -leading };
        push(&mut ops, &format!("{:.2} {:.2} Td", x - previous_x, dy));
        ops.push(b'(');
        ops.extend_from_slice(&escape_pdf_string(line));
        ops.extend_from_slice(b") Tj\n");
        previous_x = x;
    }
    push(&mut ops, "ET");
    push(&mut ops, "Q");
    push(&mut ops, "EMC");
    ops.pop(); // The operators are newline-separated, not newline-terminated.

    let mut font_resources = Dictionary::new();
    font_resources.set("Helv", Object::Reference(font));
    let mut resources = Dictionary::new();
    resources.set("Font", Object::Dictionary(font_resources));

    let mut dict = Dictionary::new();
    dict.set("Type", Object::Name(b"XObject".to_vec()));
    dict.set("Subtype", Object::Name(b"Form".to_vec()));
    dict.set(
        "BBox",
        Object::Array(vec![
            Object::Real(0.0),
            Object::Real(0.0),
            Object::Real(width as f32),
            Object::Real(height as f32),
        ]),
    );
    dict.set("Resources", Object::Dictionary(resources));
    Stream::new(dict, ops).with_compression(false)
}

/// Formats a font size the way PDF operators want it: `10` rather than `10.0`, `7.5` intact.
fn trim_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        format!("{}", value)
    }
}

/// Widens a coordinate parsed from the file, snapping it back to the decimal value that was
/// written there.
///
/// Real numbers are held as `f32`, so `392.64` reads back as `392.640015`. Left alone, that
/// error reaches the baseline arithmetic and shifts text by a hundredth of a point — invisible,
/// but enough to make otherwise identical output differ. PDF coordinates are written with a
/// handful of decimals, so rounding to four recovers the intended value exactly.
fn decimal(value: f32) -> f64 {
    (value as f64 * 10_000.0).round() / 10_000.0
}

/// Resolves an object that may be given inline or by reference.
fn deref<'a>(doc: &'a Document, object: &'a Object) -> Option<&'a Object> {
    match object {
        Object::Reference(id) => doc.get_object(*id).ok(),
        other => Some(other),
    }
}

/// Maps field name to the object holding its widget annotation, for every named annotation
/// in the document.
fn widget_ids(doc: &Document) -> HashMap<String, ObjectId> {
    let mut found = HashMap::new();
    for (_, page_id) in doc.get_pages() {
        let Ok(page) = doc.get_dictionary(page_id) else {
            continue;
        };
        let Some(annots) = page.get(b"Annots").ok().and_then(|a| deref(doc, a)) else {
            continue;
        };
        let Ok(annots) = annots.as_array() else {
            continue;
        };
        for annot in annots {
            let Object::Reference(id) = annot else {
                continue;
            };
            let Ok(dict) = doc.get_dictionary(*id) else {
                continue;
            };
            if let Ok(Object::String(name, _)) = dict.get(b"T") {
                found.insert(String::from_utf8_lossy(name).into_owned(), *id);
            }
        }
    }
    found
}

/// Where the `/AcroForm` dictionary lives. A generator may put it in its own object or write
/// it straight into the catalog, and both have to be writable.
enum AcroForm {
    Referenced(ObjectId),
    Inline,
}

/// Locates the `/AcroForm` dictionary and the id of its `/DR /Font /Helv` entry.
fn acroform(doc: &Document, template: &Path) -> Result<(AcroForm, ObjectId), String> {
    let catalog = doc
        .catalog()
        .map_err(|e| format!("no PDF catalog: {}", e))?;
    let (location, form) = match catalog.get(b"AcroForm") {
        Ok(Object::Reference(id)) => (
            AcroForm::Referenced(*id),
            doc.get_dictionary(*id)
                .map_err(|e| format!("unreadable /AcroForm: {}", e))?,
        ),
        Ok(Object::Dictionary(dict)) => (AcroForm::Inline, dict),
        _ => {
            return Err(format!(
                "{} has no /AcroForm; it is not a fillable form",
                template.display()
            ))
        }
    };
    let font_id = form
        .get(b"DR")
        .ok()
        .and_then(|dr| deref(doc, dr))
        .and_then(|dr| dr.as_dict().ok())
        .and_then(|dr| dr.get(b"Font").ok())
        .and_then(|fonts| deref(doc, fonts))
        .and_then(|fonts| fonts.as_dict().ok())
        .and_then(|fonts| match fonts.get(b"Helv") {
            Ok(Object::Reference(id)) => Some(*id),
            _ => None,
        })
        .ok_or_else(|| {
            format!(
                "the /AcroForm in {} has no /DR /Font /Helv entry, so there is no font to draw \
                 the filled text with",
                template.display()
            )
        })?;
    Ok((location, font_id))
}

/// Fills `template`'s form fields with `values` (keyed by PDF field name) and returns the
/// finished PDF.
///
/// `multiline` names the fields that wrap; the rest are drawn as a single line. Warnings
/// about text that cannot be made to fit are collected rather than printed, so the caller
/// decides where they go.
pub fn fill(
    template: &Path,
    values: &[(String, String)],
    multiline: &[String],
    left_aligned: &[String],
    min_font_size: f64,
    max_font_size: f64,
    warnings: &mut Vec<String>,
) -> Result<Vec<u8>, String> {
    let mut doc = Document::load(template)
        .map_err(|e| format!("cannot read {}: {}", template.display(), e))?;
    let (form, font_id) = acroform(&doc, template)?;
    let annots = widget_ids(&doc);

    let missing: Vec<&str> = values
        .iter()
        .map(|(field, _)| field.as_str())
        .filter(|field| !annots.contains_key(*field))
        .collect();
    if !missing.is_empty() {
        let mut missing = missing;
        missing.sort_unstable();
        return Err(format!(
            "{} has no field(s) {}; check the \"fields:\" section of the config file",
            template.display(),
            missing.join(", ")
        ));
    }

    // The base-14 /Helv in a template of this kind declares no encoding; the width table and
    // the escaped literals above are both WinAnsi, so say so.
    if let Ok(font) = doc.get_object_mut(font_id).and_then(Object::as_dict_mut) {
        font.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
    }

    for (field, text) in values {
        let annot_id = annots[field];
        let rect: Vec<f64> = doc
            .get_dictionary(annot_id)
            .ok()
            .and_then(|d| d.get(b"Rect").ok())
            .and_then(|r| r.as_array().ok())
            .map(|r| {
                r.iter()
                    .filter_map(|v| v.as_float().ok())
                    .map(decimal)
                    .collect()
            })
            .unwrap_or_default();
        if rect.len() != 4 {
            return Err(format!("field {} has no usable /Rect", field));
        }
        let (width, height) = (rect[2] - rect[0], rect[3] - rect[1]);
        let (inner_w, inner_h) = (width - 2.0 * PAD, height - 2.0 * PAD);
        let is_multiline = multiline.iter().any(|m| m == field);
        let align = if is_multiline || left_aligned.iter().any(|m| m == field) {
            Align::Left
        } else {
            Align::Center
        };

        let encoded = to_winansi(text);
        let (size, lines) = fit(
            &encoded,
            inner_w,
            inner_h,
            min_font_size,
            max_font_size,
            is_multiline,
        );
        if is_multiline && lines.len() as f64 * size * LEADING > inner_h {
            warnings.push(format!(
                "field {} cannot fit its text at {}pt; it will be clipped",
                field,
                trim_number(min_font_size)
            ));
        }

        let stream = appearance_stream(width, height, &lines, size, align, is_multiline, font_id);
        let stream_id = doc.add_object(Object::Stream(stream));

        let existing_flags = doc
            .get_dictionary(annot_id)
            .ok()
            .and_then(|d| d.get(b"Ff").ok())
            .and_then(|f| f.as_i64().ok())
            .unwrap_or(0);
        let annot = doc
            .get_object_mut(annot_id)
            .and_then(Object::as_dict_mut)
            .map_err(|e| format!("field {} is not a dictionary: {}", field, e))?;
        annot.set("V", pdf_text_string(text));
        annot.set(
            "DA",
            Object::String(
                format!("/Helv {} Tf 0 g", trim_number(size)).into_bytes(),
                StringFormat::Literal,
            ),
        );
        annot.set("Q", Object::Integer(align as i64));
        if is_multiline {
            annot.set("Ff", Object::Integer(existing_flags | MULTILINE_FLAG));
        }
        let mut ap = Dictionary::new();
        ap.set("N", Object::Reference(stream_id));
        annot.set("AP", Object::Dictionary(ap));
    }

    // Appearances are supplied, so viewers must not regenerate — and lose — them.
    let form_dict = match form {
        AcroForm::Referenced(id) => doc.get_object_mut(id).and_then(Object::as_dict_mut).ok(),
        AcroForm::Inline => doc
            .catalog_mut()
            .ok()
            .and_then(|catalog| catalog.get_mut(b"AcroForm").ok())
            .and_then(|form| form.as_dict_mut().ok()),
    };
    if let Some(form_dict) = form_dict {
        form_dict.set("NeedAppearances", Object::Boolean(false));
    }

    let mut out = Vec::new();
    doc.save_to(&mut out)
        .map_err(|e| format!("cannot write the filled PDF: {}", e))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_helvetica_against_known_widths() {
        // "A" is 667/1000 em, space is 278/1000.
        assert!((text_width(b"A", 10.0) - 6.67).abs() < 1e-9);
        assert!((text_width(b"A A", 10.0) - (6.67 + 2.78 + 6.67)).abs() < 1e-9);
        assert_eq!(text_width(b"", 10.0), 0.0);
    }

    #[test]
    fn wraps_on_word_boundaries() {
        let lines = wrap(b"alpha beta gamma", 10.0, text_width(b"alpha beta", 10.0));
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], b"alpha beta");
        assert_eq!(lines[1], b"gamma");
    }

    #[test]
    fn breaks_a_word_too_long_for_the_line() {
        let lines = wrap(b"aaaaaaaaaaaaaaaaaaaa", 10.0, text_width(b"aaaa", 10.0));
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(text_width(line, 10.0) <= text_width(b"aaaa", 10.0) + 1e-9);
        }
        let rejoined: Vec<u8> = lines.concat();
        assert_eq!(rejoined, b"aaaaaaaaaaaaaaaaaaaa");
    }

    #[test]
    fn empty_text_still_yields_one_line() {
        assert_eq!(wrap(b"", 10.0, 100.0), vec![Vec::<u8>::new()]);
    }

    #[test]
    fn single_line_fit_shrinks_until_the_text_fits_the_width() {
        let text = b"a rather long single line of text";
        let width = text_width(text, 6.0);
        let (size, lines) = fit(text, width, 20.0, 5.0, 10.0, false);
        assert_eq!(lines.len(), 1);
        assert!((5.0..=6.0).contains(&size));
        assert!(text_width(text, size) <= width + 1e-9);
    }

    #[test]
    fn fit_never_goes_below_the_minimum() {
        let (size, _) = fit(b"impossibly long text", 1.0, 1.0, 5.0, 10.0, true);
        assert_eq!(size, 5.0);
    }

    #[test]
    fn winansi_maps_smart_punctuation_and_replaces_the_rest() {
        assert_eq!(to_winansi("plain"), b"plain".to_vec());
        assert_eq!(to_winansi("\u{2014}"), vec![0x97]); // em dash
        assert_eq!(to_winansi("\u{2019}"), vec![0x92]); // right single quote
        assert_eq!(to_winansi("\u{00E9}"), vec![0xE9]); // e-acute, shared with Latin-1
        assert_eq!(to_winansi("\u{4E2D}"), b"?".to_vec()); // no WinAnsi equivalent
    }

    #[test]
    fn escapes_the_three_literal_string_characters() {
        assert_eq!(escape_pdf_string(b"a(b)c\\d"), b"a\\(b\\)c\\\\d".to_vec());
    }

    #[test]
    fn non_ascii_field_values_go_out_as_utf16() {
        match pdf_text_string("ok") {
            Object::String(bytes, _) => assert_eq!(bytes, b"ok".to_vec()),
            _ => panic!("expected a string"),
        }
        match pdf_text_string("\u{00E9}") {
            Object::String(bytes, _) => assert_eq!(bytes, vec![0xFE, 0xFF, 0x00, 0xE9]),
            _ => panic!("expected a string"),
        }
    }

    #[test]
    fn font_sizes_render_without_a_trailing_zero() {
        assert_eq!(trim_number(10.0), "10");
        assert_eq!(trim_number(7.5), "7.5");
    }
}
