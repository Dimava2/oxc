#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "napi"), allow(dead_code))]
pub(super) struct VueScriptBlock {
    pub content_start: usize,
    pub content_end: usize,
    pub lang: Option<String>,
}

#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "napi"), allow(dead_code))]
pub(super) struct VueTemplateBlock {
    pub content_start: usize,
    pub content_end: usize,
}

/// Parse all `<script ...>...</script>` blocks from a Vue SFC source.
///
/// This lightweight parser is intended for staged internal Vue formatting:
/// it extracts script block ranges and `lang` attribute for script-only formatting.
pub(super) fn parse_script_blocks(source_text: &str) -> Vec<VueScriptBlock> {
    parse_blocks_by_tag(source_text, "script")
        .into_iter()
        .map(|raw| VueScriptBlock {
            content_start: raw.content_start,
            content_end: raw.content_end,
            lang: extract_lang_attribute(raw.open_tag_content).map(ToString::to_string),
        })
        .collect()
}

/// Parse all `<template ...>...</template>` blocks from a Vue SFC source.
pub(super) fn parse_template_blocks(source_text: &str) -> Vec<VueTemplateBlock> {
    parse_blocks_by_tag(source_text, "template")
        .into_iter()
        .map(|raw| VueTemplateBlock {
            content_start: raw.content_start,
            content_end: raw.content_end,
        })
        .collect()
}

#[derive(Debug, Clone)]
struct RawBlock<'a> {
    content_start: usize,
    content_end: usize,
    open_tag_content: &'a str,
}

fn parse_blocks_by_tag<'a>(source_text: &'a str, tag_name: &str) -> Vec<RawBlock<'a>> {
    let mut blocks = Vec::new();
    let mut pointer = 0usize;
    let close_tag = format!("</{tag_name}>");
    let open_prefix = format!("<{tag_name}");

    while let Some(open_start) = find_open_tag(source_text, pointer, &open_prefix) {
        let Some(tag_end_offset) = find_tag_end(source_text, open_start) else {
            break;
        };
        let open_end = open_start + tag_end_offset + 1;
        let open_tag_content = &source_text[open_start..open_start + tag_end_offset];

        let Some(close_rel) = source_text[open_end..].find(&close_tag) else {
            break;
        };
        let content_end = open_end + close_rel;
        blocks.push(RawBlock { content_start: open_end, content_end, open_tag_content });

        pointer = content_end + close_tag.len();
    }

    blocks
}

fn find_open_tag(source_text: &str, from: usize, open_prefix: &str) -> Option<usize> {
    let mut pointer = from;
    while let Some(rel_idx) = source_text[pointer..].find(open_prefix) {
        let idx = pointer + rel_idx;

        // Skip tags inside HTML comments.
        if is_inside_html_comment(source_text, idx) {
            pointer = idx + open_prefix.len();
            continue;
        }

        let boundary = source_text[idx + open_prefix.len()..].chars().next()?;
        if matches!(boundary, '>' | ' ' | '\t' | '\n' | '\r') {
            return Some(idx);
        }

        // Handles `<script-view ...>` etc.
        pointer = idx + open_prefix.len();
    }
    None
}

fn is_inside_html_comment(source_text: &str, index: usize) -> bool {
    let before = &source_text[..index];
    let start = before.rfind("<!--");
    let end = before.rfind("-->");
    matches!(start, Some(start_idx) if end.is_none_or(|end_idx| end_idx < start_idx))
}

/// Find `>` for an opening tag while respecting quoted attribute values.
fn find_tag_end(source_text: &str, start: usize) -> Option<usize> {
    let mut in_quote: Option<char> = None;
    for (offset, ch) in source_text[start..].char_indices() {
        match ch {
            '"' | '\'' => {
                if in_quote == Some(ch) {
                    in_quote = None;
                } else if in_quote.is_none() {
                    in_quote = Some(ch);
                }
            }
            '>' if in_quote.is_none() => return Some(offset),
            _ => {}
        }
    }
    None
}

fn extract_lang_attribute(open_tag_content: &str) -> Option<&str> {
    let lang_index = open_tag_content.find("lang")?;
    let mut rest = open_tag_content[lang_index + "lang".len()..].trim_start();
    if !rest.starts_with('=') {
        return None;
    }
    rest = rest[1..].trim_start();

    let first_char = rest.chars().next()?;
    match first_char {
        '"' | '\'' => {
            let quote = first_char;
            rest = &rest[1..];
            let end = rest.find(quote)?;
            Some(&rest[..end])
        }
        _ => {
            let end = rest.find(|c: char| c.is_whitespace() || c == '>').unwrap_or(rest.len());
            Some(&rest[..end])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_script_blocks, parse_template_blocks};

    #[test]
    fn parses_multiple_script_blocks() {
        let source = r#"
<template><script-view /></template>
<script lang="ts">const a=1</script>
<script setup>const b=2</script>
"#;
        let blocks = parse_script_blocks(source);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].lang.as_deref(), Some("ts"));
        assert_eq!(blocks[1].lang, None);
    }

    #[test]
    fn ignores_script_in_html_comment() {
        let source = r#"
<!-- <script>nope</script> -->
<script>const ok = true</script>
"#;
        let blocks = parse_script_blocks(source);
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn parses_template_blocks() {
        let source = r#"
<template> <div> {{a}} </div> </template>
<script setup>const a=1</script>
"#;
        let blocks = parse_template_blocks(source);
        assert_eq!(blocks.len(), 1);
        let content = &source[blocks[0].content_start..blocks[0].content_end];
        assert_eq!(content, " <div> {{a}} </div> ");
    }
}
