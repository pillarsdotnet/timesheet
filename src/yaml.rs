// Copyright (c) 2025 Robert August Vincent II <pillarsdotnet@gmail.com>
// Co-author: Claude-AI.

//! A small block-YAML subset, enough for `~/.config/timesheet.yml`.
//!
//! Supported: nested mappings by indentation, block sequences (`- item`), flow sequences
//! (`[a, b]`), `#` comments outside quotes, and optionally quoted scalars. Not supported:
//! anchors, tags, multiple documents, folded/literal block scalars, and complex keys.
//! Anything unrecognized is skipped rather than reported, matching the tolerant handling
//! the rest of the config code already uses.

/// A parsed YAML node. Mappings keep source order so that error messages and any future
/// round-tripping follow the file rather than a hash ordering.
#[derive(Clone, Debug, PartialEq)]
pub enum Yaml {
    Scalar(String),
    List(Vec<Yaml>),
    Map(Vec<(String, Yaml)>),
}

impl Yaml {
    /// The scalar text, or `None` for a list or mapping.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Yaml::Scalar(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Looks up `key` in a mapping, comparing case-insensitively so that config keys may be
    /// written in any case. Returns `None` for a non-mapping.
    pub fn get(&self, key: &str) -> Option<&Yaml> {
        match self {
            Yaml::Map(entries) => entries
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v),
            _ => None,
        }
    }

    /// The scalar value of `key`, trimmed. An empty scalar reads as absent, so that a key
    /// left blank in the config falls back to the default instead of blanking the setting.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key)
            .and_then(Yaml::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// The value of `key` as a list of strings. A scalar counts as a one-element list, which
    /// is what lets `to:` accept either a single address or a sequence of them. Empty
    /// elements are dropped.
    pub fn get_list(&self, key: &str) -> Option<Vec<String>> {
        let value = self.get(key)?;
        let items: Vec<String> = match value {
            Yaml::Scalar(s) => s
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            Yaml::List(items) => items
                .iter()
                .filter_map(Yaml::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            Yaml::Map(_) => return None,
        };
        if items.is_empty() {
            None
        } else {
            Some(items)
        }
    }

    /// The keys of a mapping, in source order; empty for anything else.
    pub fn keys(&self) -> Vec<&str> {
        match self {
            Yaml::Map(entries) => entries.iter().map(|(k, _)| k.as_str()).collect(),
            _ => Vec::new(),
        }
    }
}

/// Strips an unquoted `#` comment from a config line.
pub fn strip_yaml_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut quote: Option<u8> = None;
    for (i, &b) in bytes.iter().enumerate() {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'#' => return &line[..i],
            None => {}
        }
    }
    line
}

/// Removes matching surrounding single or double quotes.
pub fn unquote_yaml_scalar(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' || first == b'\'') && first == last {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// One significant source line: its indentation and its comment-stripped text.
struct Line {
    indent: usize,
    text: String,
}

/// Drops blank lines, comment-only lines and document markers, recording each survivor's
/// indentation. Tabs are not indentation in YAML, so a leading tab is left in the text and
/// the line simply reads as more deeply indented than its parent.
fn significant_lines(text: &str) -> Vec<Line> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = strip_yaml_comment(raw);
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() || trimmed.trim() == "---" || trimmed.trim() == "..." {
            continue;
        }
        let indent = trimmed.len() - trimmed.trim_start().len();
        out.push(Line {
            indent,
            text: trimmed.trim_start().to_string(),
        });
    }
    out
}

/// Splits `key: value` at the first colon that is not inside quotes. Returns `None` when the
/// line has no key, which is how a stray line gets skipped.
fn split_key(text: &str) -> Option<(String, String)> {
    let bytes = text.as_bytes();
    let mut quote: Option<u8> = None;
    for (i, &b) in bytes.iter().enumerate() {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b':' => {
                let key = unquote_yaml_scalar(text[..i].trim()).trim().to_string();
                if key.is_empty() {
                    return None;
                }
                return Some((key, text[i + 1..].trim().to_string()));
            }
            None => {}
        }
    }
    None
}

/// Parses `[a, b, "c, d"]` into its elements, respecting quotes so that a separator inside a
/// quoted element does not split it.
fn parse_flow_sequence(text: &str) -> Vec<Yaml> {
    let inner = text[1..text.len().saturating_sub(1)].trim();
    if inner.is_empty() {
        return Vec::new();
    }
    let mut items = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for c in inner.chars() {
        match quote {
            Some(q) if c == q => {
                quote = None;
                current.push(c);
            }
            Some(_) => current.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                current.push(c);
            }
            None if c == ',' => {
                items.push(Yaml::Scalar(scalar(&current)));
                current.clear();
            }
            None => current.push(c),
        }
    }
    items.push(Yaml::Scalar(scalar(&current)));
    items
}

/// Normalizes a scalar: trim, then remove one layer of surrounding quotes. Quoting is how a
/// value whose surrounding spaces matter — `separator: "; "` above all — survives the trim.
fn scalar(text: &str) -> String {
    unquote_yaml_scalar(text.trim()).to_string()
}

/// Parses a scalar, or a flow sequence when the text is bracketed.
fn parse_inline(text: &str) -> Yaml {
    let trimmed = text.trim();
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        return Yaml::List(parse_flow_sequence(trimmed));
    }
    Yaml::Scalar(scalar(trimmed))
}

/// Parses the block starting at `pos` whose lines are indented at least `indent`, advancing
/// `pos` past it. A block is a sequence when its first line begins with `- `, otherwise a
/// mapping.
fn parse_block(lines: &[Line], pos: &mut usize, indent: usize) -> Yaml {
    if lines
        .get(*pos)
        .is_some_and(|l| l.text == "-" || l.text.starts_with("- "))
    {
        return parse_sequence(lines, pos, indent);
    }
    parse_mapping(lines, pos, indent)
}

fn parse_sequence(lines: &[Line], pos: &mut usize, indent: usize) -> Yaml {
    let mut items = Vec::new();
    while let Some(line) = lines.get(*pos) {
        if line.indent < indent || !(line.text == "-" || line.text.starts_with("- ")) {
            break;
        }
        let rest = line.text[1..].trim().to_string();
        let item_indent = line.indent + 2;
        *pos += 1;
        if rest.is_empty() {
            // `-` alone introduces a nested block on the following lines.
            match lines.get(*pos) {
                Some(next) if next.indent > line.indent => {
                    let child = next.indent;
                    items.push(parse_block(lines, pos, child));
                }
                _ => items.push(Yaml::Scalar(String::new())),
            }
        } else if split_key(&rest).is_some() {
            // `- key: value` starts a mapping whose remaining keys align under the dash.
            let mut sub = vec![Line {
                indent: item_indent,
                text: rest,
            }];
            while let Some(next) = lines.get(*pos) {
                if next.indent < item_indent {
                    break;
                }
                sub.push(Line {
                    indent: next.indent,
                    text: next.text.clone(),
                });
                *pos += 1;
            }
            let mut sub_pos = 0;
            items.push(parse_mapping(&sub, &mut sub_pos, item_indent));
        } else {
            items.push(parse_inline(&rest));
        }
    }
    Yaml::List(items)
}

fn parse_mapping(lines: &[Line], pos: &mut usize, indent: usize) -> Yaml {
    let mut entries: Vec<(String, Yaml)> = Vec::new();
    while let Some(line) = lines.get(*pos) {
        if line.indent < indent {
            break;
        }
        let Some((key, rest)) = split_key(&line.text) else {
            // Not a mapping entry (a stray line, or a sequence dash at this level).
            *pos += 1;
            continue;
        };
        *pos += 1;
        let value = if !rest.is_empty() {
            parse_inline(&rest)
        } else {
            match lines.get(*pos) {
                // A block sequence may sit at the parent's own indentation, which is why a
                // dash line at exactly `line.indent` still counts as this key's value.
                Some(next)
                    if next.indent > line.indent
                        || (next.indent == line.indent
                            && (next.text == "-" || next.text.starts_with("- "))) =>
                {
                    parse_block(lines, pos, next.indent)
                }
                _ => Yaml::Scalar(String::new()),
            }
        };
        entries.push((key, value));
    }
    Yaml::Map(entries)
}

/// Parses a whole document. The result is always a mapping at the top level; a file that is
/// empty or entirely unrecognized yields an empty one.
pub fn parse(text: &str) -> Yaml {
    let lines = significant_lines(text);
    let indent = lines.first().map(|l| l.indent).unwrap_or(0);
    let mut pos = 0;
    let parsed = parse_block(&lines, &mut pos, indent);
    match parsed {
        Yaml::Map(_) => parsed,
        _ => Yaml::Map(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_mappings() {
        let doc = parse("rotate:\n  day: monday\n  time: \"00:00\"\ntop: value\n");
        assert_eq!(doc.get("top").and_then(Yaml::as_str), Some("value"));
        let rotate = doc.get("rotate").unwrap();
        assert_eq!(rotate.get_str("day"), Some("monday"));
        assert_eq!(rotate.get_str("time"), Some("00:00"));
    }

    #[test]
    fn parses_three_levels() {
        let doc = parse("prefixes:\n  ST:\n    fields:\n      total_hours: cell_1_14\n");
        let fields = doc
            .get("prefixes")
            .and_then(|p| p.get("ST"))
            .and_then(|p| p.get("fields"))
            .unwrap();
        assert_eq!(fields.get_str("total_hours"), Some("cell_1_14"));
    }

    #[test]
    fn scalar_or_sequence_reads_as_a_list() {
        let doc = parse("to: one@example.com\ncc:\n  - a@example.com\n  - b@example.com\n");
        assert_eq!(
            doc.get_list("to"),
            Some(vec!["one@example.com".to_string()])
        );
        assert_eq!(
            doc.get_list("cc"),
            Some(vec![
                "a@example.com".to_string(),
                "b@example.com".to_string()
            ])
        );
    }

    #[test]
    fn block_sequence_may_align_with_its_key() {
        let doc = parse("cc:\n- a@example.com\n- b@example.com\nfrom: me@example.com\n");
        assert_eq!(
            doc.get_list("cc"),
            Some(vec![
                "a@example.com".to_string(),
                "b@example.com".to_string()
            ])
        );
        assert_eq!(doc.get_str("from"), Some("me@example.com"));
    }

    #[test]
    fn flow_sequence_respects_quotes() {
        let doc = parse("to: [\"a, b\", c]\n");
        assert_eq!(
            doc.get_list("to"),
            Some(vec!["a, b".to_string(), "c".to_string()])
        );
    }

    #[test]
    fn comma_separated_scalar_splits_into_a_list() {
        let doc = parse("cc: \"a@example.com, b@example.com\"\n");
        assert_eq!(
            doc.get_list("cc"),
            Some(vec![
                "a@example.com".to_string(),
                "b@example.com".to_string()
            ])
        );
    }

    #[test]
    fn quotes_preserve_surrounding_space() {
        let doc = parse("separator: \"; \"\nzero: \"\"\n");
        assert_eq!(doc.get("separator").and_then(Yaml::as_str), Some("; "));
        // An empty string is a deliberate value, but `get_str` treats it as unset so the
        // caller's default applies; the raw scalar is still there.
        assert_eq!(doc.get("zero").and_then(Yaml::as_str), Some(""));
        assert_eq!(doc.get_str("zero"), None);
    }

    #[test]
    fn comments_are_stripped_outside_quotes() {
        let doc = parse("day: Monday   # start of the week\nnote: \"a # b\"\n");
        assert_eq!(doc.get_str("day"), Some("Monday"));
        assert_eq!(doc.get_str("note"), Some("a # b"));
    }

    #[test]
    fn keys_match_case_insensitively_and_keep_source_order() {
        let doc = parse("Alpha: 1\nbeta: 2\n");
        assert_eq!(doc.get_str("alpha"), Some("1"));
        assert_eq!(doc.keys(), vec!["Alpha", "beta"]);
    }

    #[test]
    fn empty_document_is_an_empty_mapping() {
        assert_eq!(parse(""), Yaml::Map(Vec::new()));
        assert_eq!(parse("# only a comment\n"), Yaml::Map(Vec::new()));
    }
}
