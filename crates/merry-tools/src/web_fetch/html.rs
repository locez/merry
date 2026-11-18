//! Lightweight HTML cleanup and Markdown extraction for `web_fetch`.

/// Extracts a trimmed HTML `<title>` if present.
#[must_use]
pub(crate) fn extract_html_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start_open = lower.find("<title")?;
    let after_tag = lower[start_open..].find('>')? + start_open + 1;
    let end_close = lower[after_tag..].find("</title>")? + after_tag;
    let decoded = decode_html_entities(&strip_all_tags(&html[after_tag..end_close]));
    let trimmed = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
    (!trimmed.is_empty()).then_some(trimmed)
}

/// Converts HTML into readable Markdown with noisy boilerplate and data URIs removed.
#[must_use]
pub(crate) fn html_to_markdown(html: &str) -> String {
    let cleaned = strip_noisy_sections(html);
    let body = extract_body_slice(&cleaned);
    let converted = convert_html_fragments(body);
    let decoded = decode_html_entities(&converted);
    let sanitized = strip_base64_data_uris(&decoded);
    normalize_whitespace(&sanitized)
}

/// Strips inline `data:...;base64,...` URIs so encoded binary blobs never pollute context.
#[must_use]
pub(crate) fn strip_base64_data_uris(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(idx) = find_ascii_case_insensitive(rest, "data:") {
        out.push_str(&rest[..idx]);
        let candidate = &rest[idx..];
        let Some(base64_pos) = find_ascii_case_insensitive(candidate, ";base64,") else {
            out.push_str("data:");
            rest = &candidate[5..];
            continue;
        };
        let header = &candidate[..base64_pos];
        if header.chars().any(char::is_whitespace) || header.len() > 64 {
            out.push_str("data:");
            rest = &candidate[5..];
            continue;
        }
        let payload_start = base64_pos + ";base64,".len();
        let payload = &candidate[payload_start..];
        let payload_len = payload
            .find(|ch: char| {
                !(ch.is_ascii_alphanumeric()
                    || ch == '+'
                    || ch == '/'
                    || ch == '='
                    || ch == '-'
                    || ch == '_')
            })
            .unwrap_or(payload.len());
        out.push_str("<data-uri-omitted>");
        rest = &candidate[payload_start + payload_len..];
    }
    out.push_str(rest);
    out
}

fn strip_noisy_sections(input: &str) -> String {
    let without_comments = strip_delimited(input, "<!--", "-->");
    let mut current = without_comments;
    for tag in [
        "script", "style", "noscript", "svg", "nav", "header", "footer",
    ] {
        current = strip_tag_block(&current, tag);
    }
    current
}

fn strip_delimited(input: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find(open) {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + open.len()..];
        if let Some(end) = after_open.find(close) {
            rest = &after_open[end + close.len()..];
        } else {
            return out;
        }
    }
    out.push_str(rest);
    out
}

fn strip_tag_block(input: &str, tag: &str) -> String {
    let open_pattern = format!("<{tag}");
    let close_pattern = format!("</{tag}>");
    let mut out = String::with_capacity(input.len());
    let mut rest = input;

    while let Some(start) = find_tag_start(rest, &open_pattern) {
        out.push_str(&rest[..start]);
        let after_open = &rest[start..];
        if let Some(close_idx) = find_ascii_case_insensitive(after_open, &close_pattern) {
            rest = &after_open[close_idx + close_pattern.len()..];
        } else if let Some(self_close) = after_open.find('>') {
            rest = &after_open[self_close + 1..];
        } else {
            return out;
        }
    }
    out.push_str(rest);
    out
}

fn find_tag_start(haystack: &str, open_prefix: &str) -> Option<usize> {
    let mut offset = 0;
    while let Some(rel) = find_ascii_case_insensitive(&haystack[offset..], open_prefix) {
        let abs = offset + rel;
        let next_char = haystack[abs + open_prefix.len()..].chars().next();
        if matches!(next_char, Some('>' | ' ' | '\t' | '\n' | '\r' | '/')) {
            return Some(abs);
        }
        offset = abs + open_prefix.len();
    }
    None
}

fn extract_body_slice(html: &str) -> &str {
    let Some(body_start) = find_tag_start(html, "<body") else {
        return html;
    };
    let Some(content_offset) = html[body_start..].find('>') else {
        return html;
    };
    let content_start = body_start + content_offset + 1;
    let Some(body_end) = find_ascii_case_insensitive(&html[content_start..], "</body>") else {
        return &html[content_start..];
    };
    &html[content_start..content_start + body_end]
}

fn convert_html_fragments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut pos = 0;
    let mut pending_link: Option<(String, usize)> = None;
    let mut in_pre = false;

    while pos < input.len() {
        let Some(rel_lt) = input[pos..].find('<') else {
            out.push_str(&input[pos..]);
            break;
        };
        if rel_lt > 0 {
            let text_chunk = &input[pos..pos + rel_lt];
            if in_pre {
                out.push_str(text_chunk);
            } else {
                push_collapsed_text(&mut out, text_chunk);
            }
        }
        let tag_start = pos + rel_lt;
        let Some(rel_gt) = input[tag_start..].find('>') else {
            out.push_str(&input[tag_start..]);
            break;
        };
        let raw_tag = &input[tag_start + 1..tag_start + rel_gt];
        let trimmed_tag = raw_tag.trim();
        let is_closing = trimmed_tag.starts_with('/');
        let tag_name = trimmed_tag
            .trim_start_matches('/')
            .split(|ch: char| ch.is_whitespace() || ch == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();

        match (is_closing, tag_name.as_str()) {
            (false, "h1") => ensure_heading(&mut out, "# "),
            (false, "h2") => ensure_heading(&mut out, "## "),
            (false, "h3") => ensure_heading(&mut out, "### "),
            (false, "h4") => ensure_heading(&mut out, "#### "),
            (false, "h5") => ensure_heading(&mut out, "##### "),
            (false, "h6") => ensure_heading(&mut out, "###### "),
            (
                true,
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" | "div" | "section" | "article"
                | "blockquote" | "tr",
            ) => {
                ensure_blank_line(&mut out);
            }
            (false, "p" | "div" | "section" | "article" | "blockquote" | "tr") => {
                ensure_newline(&mut out);
            }
            (false, "br") => out.push('\n'),
            (false, "hr") => {
                ensure_blank_line(&mut out);
                out.push_str("---\n\n");
            }
            (false, "li") => {
                ensure_newline(&mut out);
                out.push_str("- ");
            }
            (true, "li" | "ul" | "ol") => ensure_newline(&mut out),
            (false, "pre") => {
                ensure_blank_line(&mut out);
                out.push_str("```\n");
                in_pre = true;
            }
            (true, "pre") => {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str("```\n\n");
                in_pre = false;
            }
            (false, "code") if !in_pre => out.push('`'),
            (true, "code") if !in_pre => out.push('`'),
            (false, "a") => {
                if let Some(href) = extract_attribute(trimmed_tag, "href")
                    && !href.is_empty()
                    && !href.to_ascii_lowercase().starts_with("javascript:")
                {
                    out.push('[');
                    pending_link = Some((href, out.len()));
                }
            }
            (true, "a") => {
                if let Some((href, label_start)) = pending_link.take() {
                    let label_is_empty = out[label_start..].trim().is_empty();
                    if label_is_empty {
                        out.truncate(label_start.saturating_sub(1));
                    } else {
                        out.push_str(&format!("]({href})"));
                    }
                }
            }
            (true, "td" | "th") => out.push(' '),
            _ => {}
        }

        pos = tag_start + rel_gt + 1;
    }

    if let Some((_, label_start)) = pending_link.take()
        && out.len() >= label_start
    {
        out.push(']');
    }
    out
}

fn ensure_newline(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

fn ensure_blank_line(out: &mut String) {
    if out.is_empty() {
        return;
    }
    if out.ends_with("\n\n") {
        return;
    }
    if out.ends_with('\n') {
        out.push('\n');
    } else {
        out.push_str("\n\n");
    }
}

fn ensure_heading(out: &mut String, prefix: &str) {
    ensure_blank_line(out);
    out.push_str(prefix);
}

fn push_collapsed_text(out: &mut String, text: &str) {
    let mut prev_space = out.is_empty() || out.ends_with(char::is_whitespace);
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
}

fn extract_attribute(tag_inner: &str, name: &str) -> Option<String> {
    let lower = tag_inner.to_ascii_lowercase();
    let needle = format!("{name}=");
    let pos = lower.find(&needle)?;
    let rest = tag_inner[pos + needle.len()..].trim_start();
    if let Some(quoted) = rest.strip_prefix('"') {
        let end = quoted.find('"')?;
        return Some(quoted[..end].trim().to_owned());
    }
    if let Some(quoted) = rest.strip_prefix('\'') {
        let end = quoted.find('\'')?;
        return Some(quoted[..end].trim().to_owned());
    }
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some(rest[..end].trim().to_owned())
}

fn strip_all_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut inside_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            _ if !inside_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

fn decode_html_entities(input: &str) -> String {
    input
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

fn normalize_whitespace(input: &str) -> String {
    let mut lines = Vec::new();
    let mut consecutive_blank = 0usize;
    for raw_line in input.lines() {
        let trimmed = raw_line.trim_end();
        if trimmed.is_empty() {
            consecutive_blank += 1;
            if consecutive_blank <= 1 && !lines.is_empty() {
                lines.push("");
            }
        } else {
            consecutive_blank = 0;
            lines.push(trimmed);
        }
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    let haystack_bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();
    if needle_bytes.len() > haystack_bytes.len() {
        return None;
    }
    haystack_bytes
        .windows(needle_bytes.len())
        .position(|window| window.eq_ignore_ascii_case(needle_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_html_to_clean_markdown_and_strips_scripts_nav_and_data_uris() {
        let html = r#"
        <html>
          <head><title>Merry &amp; Docs</title><style>.x { color: red; }</style></head>
          <body>
            <nav><a href="/home">Home</a></nav>
            <h1>Getting Started</h1>
            <p>Read the <a href="https://example.com/guide">Guide</a> carefully.</p>
            <img src="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAE=" />
            <ul><li>First item</li><li>Second item</li></ul>
            <pre><code>cargo test</code></pre>
            <script>alert('xss');</script>
            <footer>Copyright</footer>
          </body>
        </html>
        "#;

        assert_eq!(extract_html_title(html).as_deref(), Some("Merry & Docs"));
        let md = html_to_markdown(html);
        assert!(md.contains("# Getting Started"));
        assert!(md.contains("Read the [Guide](https://example.com/guide) carefully."));
        assert!(md.contains("- First item"));
        assert!(md.contains("- Second item"));
        assert!(md.contains("```"));
        assert!(md.contains("cargo test"));
        assert!(!md.contains("alert('xss')"));
        assert!(!md.contains("color: red"));
        assert!(!md.contains("Home"));
        assert!(!md.contains("Copyright"));
        assert!(!md.contains("iVBORw0KGgo"));
    }
}
