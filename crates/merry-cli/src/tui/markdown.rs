mod table;

use self::table::{TableState, spans_width, table_lines};
use super::{
    copy_controls::{CopyContent, CopyTarget, code_block_header},
    highlight::highlight_code_to_lines,
    links::{LinkTarget, link_style, push_text_with_bare_urls},
    state::TuiState,
    text_wrap::{
        StyledTextPart, WrappedLine, inline_code_style, inline_code_text, semantic_style,
        wrap_styled_parts_preserving_leading_whitespace, wrap_styled_parts_with_links,
    },
    theme::SemanticColor,
    transcript::{SelectionPolicy, TranscriptRow},
};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use std::sync::Arc;
use unicode_width::UnicodeWidthStr;

/// Display lines, copy controls, and hyperlink hit targets produced by the same Markdown parse.
pub(crate) struct RenderedMarkdown {
    pub(crate) rows: Vec<TranscriptRow>,
    pub(crate) copy_targets: Vec<CopyTarget>,
    pub(crate) link_targets: Vec<LinkTarget>,
}

impl RenderedMarkdown {
    fn new(
        rows: Vec<TranscriptRow>,
        copy_targets: Vec<CopyTarget>,
        link_targets: Vec<LinkTarget>,
    ) -> Self {
        Self {
            rows,
            copy_targets,
            link_targets,
        }
    }
}

pub(crate) fn markdown_lines(
    state: &TuiState,
    markdown: &str,
    region_width: u16,
) -> RenderedMarkdown {
    let parser = Parser::new_ext(
        markdown,
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS,
    );
    let mut renderer = MarkdownRenderer::new(state, region_width);
    for event in parser {
        renderer.push_event(event);
    }
    renderer.finish()
}

struct MarkdownRenderer<'state> {
    state: &'state TuiState,
    region_width: u16,
    prose_width: u16,
    rows: Vec<TranscriptRow>,
    copy_targets: Vec<CopyTarget>,
    link_targets: Vec<LinkTarget>,
    parts: Vec<StyledTextPart>,
    style_stack: Vec<Style>,
    link_stack: Vec<LinkState>,
    quote_depth: usize,
    list_stack: Vec<ListState>,
    pending_list_marker: Option<String>,
    active_list_indent: usize,
    prev_list_item_multiline: bool,
    code_block: Option<CodeBlockState>,
    table: Option<TableState>,
}

#[derive(Debug, Clone)]
struct ListState {
    next_number: Option<u64>,
}

#[derive(Debug, Clone)]
struct CodeBlockState {
    lang: Option<String>,
    text: String,
}

#[derive(Debug, Clone)]
struct LinkState {
    dest_url: Arc<str>,
    has_visible_text: bool,
}

impl<'state> MarkdownRenderer<'state> {
    fn new(state: &'state TuiState, region_width: u16) -> Self {
        let max_reading = state.max_reading_width();
        let prose_width = if max_reading == 0 {
            region_width
        } else {
            region_width.min(max_reading)
        }
        .max(1);
        Self {
            state,
            region_width,
            prose_width,
            rows: Vec::new(),
            copy_targets: Vec::new(),
            link_targets: Vec::new(),
            parts: Vec::new(),
            style_stack: vec![semantic_style(state, SemanticColor::Assistant)],
            link_stack: Vec::new(),
            quote_depth: 0,
            list_stack: Vec::new(),
            pending_list_marker: None,
            active_list_indent: 0,
            prev_list_item_multiline: false,
            code_block: None,
            table: None,
        }
    }

    fn push_block_gap(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let last_is_blank = self
            .rows
            .last()
            .is_some_and(|row| row.display.spans.iter().all(|span| span.content.is_empty()));
        if !last_is_blank {
            self.push_row(TranscriptRow::new(Line::from(""), SelectionPolicy::Keep));
        }
    }

    fn push_event(&mut self, event: Event<'_>) {
        if self.push_code_block_event(&event) {
            return;
        }
        if self.table.is_some() && !matches!(event, Event::Start(Tag::Table(_))) {
            self.push_table_event(&event);
            return;
        }

        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) => self.push_text(text.as_ref()),
            Event::Code(code) => self.push_atomic_text(
                inline_code_text(self.state, code.as_ref()),
                inline_code_style(self.state, self.current_style()),
            ),
            Event::SoftBreak | Event::HardBreak => self.flush_parts(),
            Event::Rule => {
                self.flush_parts();
                self.push_block_gap();
                self.push_row(TranscriptRow::new(
                    Line::from(Span::styled(
                        "─".repeat(usize::from(self.prose_width).max(1)),
                        semantic_style(self.state, SemanticColor::Border),
                    )),
                    SelectionPolicy::Keep,
                ));
            }
            Event::Html(html) | Event::InlineHtml(html) => self.push_text(html.as_ref()),
            Event::FootnoteReference(reference) => self.push_text(reference.as_ref()),
            Event::TaskListMarker(checked) => {
                self.push_text(if checked { "[x] " } else { "[ ] " });
            }
            Event::InlineMath(math) | Event::DisplayMath(math) => self.push_text(math.as_ref()),
        }
    }

    fn push_code_block_event(&mut self, event: &Event<'_>) -> bool {
        let Some(block) = &mut self.code_block else {
            return false;
        };
        match event {
            Event::End(TagEnd::CodeBlock) => {
                let block = self.code_block.take().expect("checked above");
                self.flush_code_block(block);
            }
            Event::Text(text)
            | Event::Code(text)
            | Event::Html(text)
            | Event::InlineHtml(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text) => block.text.push_str(text.as_ref()),
            Event::SoftBreak | Event::HardBreak => block.text.push('\n'),
            _ => {}
        }
        true
    }

    fn start_tag(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                if self.list_stack.is_empty() && self.quote_depth == 0 {
                    self.push_block_gap();
                }
            }
            Tag::Heading { level, .. } => {
                self.flush_parts();
                self.push_block_gap();
                let style = match level {
                    HeadingLevel::H1 | HeadingLevel::H2 => {
                        semantic_style(self.state, SemanticColor::Heading)
                            .add_modifier(Modifier::BOLD)
                    }
                    _ => semantic_style(self.state, SemanticColor::Assistant)
                        .add_modifier(Modifier::BOLD),
                };
                self.push_style(style);
            }
            Tag::BlockQuote(_) => {
                self.flush_parts();
                if self.quote_depth == 0 {
                    self.push_block_gap();
                }
                self.quote_depth = self.quote_depth.saturating_add(1);
            }
            Tag::CodeBlock(kind) => {
                self.flush_parts();
                self.push_block_gap();
                self.code_block = Some(CodeBlockState {
                    lang: code_block_lang(kind),
                    text: String::new(),
                });
            }
            Tag::List(start) => {
                if self.list_stack.is_empty() && self.quote_depth == 0 {
                    self.flush_parts();
                    self.push_block_gap();
                }
                self.list_stack.push(ListState { next_number: start });
            }
            Tag::Item => self.start_list_item(),
            Tag::Emphasis => self.push_style(self.current_style().add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(strong_style(self.state)),
            Tag::Strikethrough => self.push_style(strikethrough_style(self.state)),
            Tag::Superscript | Tag::Subscript => self.push_style(self.current_style()),
            Tag::Link { dest_url, .. } => {
                self.start_link(dest_url.as_ref());
            }
            Tag::Image { dest_url, .. } => {
                self.push_text("[image: ");
                self.push_text(dest_url.as_ref());
                self.push_text("]");
            }
            Tag::HtmlBlock => {}
            Tag::Table(alignments) => {
                self.flush_parts();
                self.push_block_gap();
                self.table = Some(TableState::new(alignments));
            }
            Tag::TableHead
            | Tag::TableRow
            | Tag::TableCell
            | Tag::FootnoteDefinition(_)
            | Tag::MetadataBlock(_) => {}
            Tag::DefinitionList | Tag::DefinitionListTitle | Tag::DefinitionListDefinition => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.flush_parts(),
            TagEnd::BlockQuote(_) => {
                self.flush_parts();
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            TagEnd::Heading(level) => {
                self.flush_parts();
                self.pop_style();
                if level == HeadingLevel::H1 {
                    let rule_len = usize::from(self.prose_width).clamp(1, 36);
                    self.push_row(TranscriptRow::new(
                        Line::from(Span::styled(
                            "━".repeat(rule_len),
                            semantic_style(self.state, SemanticColor::Border),
                        )),
                        SelectionPolicy::Skip,
                    ));
                }
            }
            TagEnd::CodeBlock => {}
            TagEnd::List(_) => {
                self.flush_parts();
                self.list_stack.pop();
                if self.list_stack.is_empty() {
                    self.prev_list_item_multiline = false;
                }
            }
            TagEnd::Item => self.flush_parts(),
            TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript => self.pop_style(),
            TagEnd::Link => self.finish_link(),
            TagEnd::Image => {}
            TagEnd::HtmlBlock => {}
            TagEnd::Table
            | TagEnd::TableHead
            | TagEnd::TableRow
            | TagEnd::TableCell
            | TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::MetadataBlock(_) => {}
        }
    }

    fn start_list_item(&mut self) {
        self.flush_parts();
        let depth = self.list_stack.len().saturating_sub(1);
        if depth == 0 && self.prev_list_item_multiline {
            self.push_block_gap();
        }
        self.prev_list_item_multiline = false;
        let indent = "  ".repeat(depth);
        let marker = match self.list_stack.last_mut() {
            Some(ListState {
                next_number: Some(next_number),
            }) => {
                let marker = format!("{indent}{next_number}. ");
                *next_number = next_number.saturating_add(1);
                marker
            }
            _ => format!("{indent}- "),
        };
        self.pending_list_marker = Some(marker);
    }

    fn push_table_event(&mut self, event: &Event<'_>) {
        match event {
            Event::End(TagEnd::Table) => {
                let table = self.table.take().expect("checked above");
                self.flush_table(table);
            }
            Event::Start(Tag::TableHead) => {
                if let Some(table) = &mut self.table {
                    table.in_head = true;
                    table.start_row();
                }
            }
            Event::End(TagEnd::TableHead) => {
                if let Some(table) = &mut self.table {
                    table.finish_row();
                    table.in_head = false;
                }
            }
            Event::Start(Tag::TableRow) => {
                if let Some(table) = &mut self.table {
                    table.start_row();
                }
            }
            Event::End(TagEnd::TableRow) => {
                if let Some(table) = &mut self.table {
                    table.finish_row();
                }
            }
            Event::Start(Tag::TableCell) => {
                if let Some(table) = &mut self.table {
                    table.start_cell();
                }
            }
            Event::End(TagEnd::TableCell) => {
                if let Some(table) = &mut self.table {
                    table.finish_cell();
                }
            }
            Event::Start(Tag::Strong) => self.push_style(strong_style(self.state)),
            Event::End(TagEnd::Strong) => self.pop_style(),
            Event::Start(Tag::Emphasis) => {
                self.push_style(self.current_style().add_modifier(Modifier::ITALIC));
            }
            Event::End(TagEnd::Emphasis) => self.pop_style(),
            Event::Start(Tag::Strikethrough) => {
                self.push_style(strikethrough_style(self.state));
            }
            Event::End(TagEnd::Strikethrough) => self.pop_style(),
            Event::Start(Tag::Link { dest_url, .. }) => {
                self.start_link(dest_url.as_ref());
            }
            Event::End(TagEnd::Link) => self.finish_link(),
            Event::Text(text)
            | Event::Html(text)
            | Event::InlineHtml(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text) => {
                self.push_table_text(text.as_ref());
            }
            Event::Code(code) => {
                let link = self.current_link_url();
                self.push_table_part(StyledTextPart::with_link(
                    inline_code_text(self.state, code.as_ref()),
                    inline_code_style(self.state, self.current_style()),
                    true,
                    link,
                ));
            }
            Event::SoftBreak | Event::HardBreak => {
                let link = self.current_link_url();
                self.push_table_part(StyledTextPart::with_link(
                    " ".to_owned(),
                    self.current_style(),
                    false,
                    link,
                ));
            }
            Event::TaskListMarker(checked) => {
                let link = self.current_link_url();
                self.push_table_part(StyledTextPart::with_link(
                    if *checked { "[x] " } else { "[ ] " }.to_owned(),
                    self.current_style(),
                    false,
                    link,
                ));
            }
            Event::FootnoteReference(reference) => {
                let link = self.current_link_url();
                self.push_table_part(StyledTextPart::with_link(
                    reference.to_string(),
                    self.current_style(),
                    false,
                    link,
                ));
            }
            Event::Rule | Event::Start(_) | Event::End(_) => {}
        }
    }

    fn push_table_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(link) = self.current_link_url() {
            self.push_table_part(StyledTextPart::with_link(
                text.to_owned(),
                self.current_style(),
                false,
                Some(link),
            ));
            return;
        }
        let base_style = self.current_style();
        let mut bare_parts = Vec::new();
        push_text_with_bare_urls(&mut bare_parts, self.state, text, base_style);
        for part in bare_parts {
            self.push_table_part(part);
        }
    }

    fn push_table_part(&mut self, part: StyledTextPart) {
        self.record_link_label(&part.text);
        if let Some(table) = &mut self.table {
            table.push_part(part);
        }
    }

    fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.push_pending_list_marker();
        if let Some(link) = self.current_link_url() {
            self.record_link_label(text);
            self.parts.push(StyledTextPart::with_link(
                text.to_owned(),
                self.current_style(),
                false,
                Some(link),
            ));
            return;
        }
        let base_style = self.current_style();
        push_text_with_bare_urls(&mut self.parts, self.state, text, base_style);
    }

    fn push_atomic_text(&mut self, text: String, style: Style) {
        if text.is_empty() {
            return;
        }
        self.push_pending_list_marker();
        self.record_link_label(&text);
        let link = self.current_link_url();
        self.parts
            .push(StyledTextPart::with_link(text, style, true, link));
    }

    fn push_pending_list_marker(&mut self) {
        let Some(marker) = self.pending_list_marker.take() else {
            return;
        };
        self.active_list_indent = UnicodeWidthStr::width(marker.as_str());
        self.parts.push(StyledTextPart::new(
            marker,
            semantic_style(self.state, SemanticColor::Muted),
            true,
        ));
    }

    fn flush_parts(&mut self) {
        self.push_pending_list_marker();
        if self.parts.is_empty() {
            self.active_list_indent = 0;
            return;
        }
        let parts = std::mem::take(&mut self.parts);
        let list_indent = std::mem::take(&mut self.active_list_indent);
        if self.quote_depth == 0 {
            if list_indent > 0 && parts.len() > 1 {
                let mut iter = parts.into_iter();
                let marker = iter.next().expect("non-empty parts");
                let body: Vec<StyledTextPart> = iter.collect();
                let body_width = self
                    .prose_width
                    .saturating_sub(u16::try_from(list_indent).unwrap_or(u16::MAX))
                    .max(1);
                let continuation = " ".repeat(list_indent);
                let wrapped: Vec<WrappedLine> = wrap_styled_parts_with_links(body, body_width)
                    .into_iter()
                    .enumerate()
                    .map(|(index, mut wrapped_line)| {
                        if index == 0 {
                            wrapped_line
                                .line
                                .spans
                                .insert(0, Span::styled(marker.text.clone(), marker.style));
                        } else {
                            wrapped_line
                                .line
                                .spans
                                .insert(0, Span::raw(continuation.clone()));
                        }
                        wrapped_line.shift_links(list_indent);
                        wrapped_line
                    })
                    .collect();
                if wrapped.len() > 1 {
                    self.prev_list_item_multiline = true;
                }
                self.push_wrapped_lines(wrapped, SelectionPolicy::Keep);
                return;
            }
            self.push_wrapped_lines(
                wrap_styled_parts_with_links(parts, self.prose_width),
                SelectionPolicy::Keep,
            );
            return;
        }

        let prefix = quote_prefix(self.quote_depth);
        let prefix_width = UnicodeWidthStr::width(prefix.as_str());
        let content_width = self
            .prose_width
            .saturating_sub(u16::try_from(prefix_width).unwrap_or(u16::MAX))
            .max(1);
        let prefix_style =
            semantic_style(self.state, SemanticColor::BorderFocus).add_modifier(Modifier::BOLD);
        self.push_wrapped_lines(
            wrap_styled_parts_with_links(parts, content_width)
                .into_iter()
                .map(|mut wrapped_line| {
                    wrapped_line
                        .line
                        .spans
                        .insert(0, Span::styled(prefix.clone(), prefix_style));
                    wrapped_line.shift_links(prefix_width);
                    wrapped_line
                })
                .collect(),
            SelectionPolicy::Keep,
        );
    }

    fn flush_code_block(&mut self, block: CodeBlockState) {
        let copy_target = if !block.text.is_empty()
            && let Some((header, column, width)) =
                code_block_header(self.state, block.lang.as_deref(), self.region_width)
        {
            let line_index = self.rows.len();
            self.push_row(TranscriptRow::new(header, SelectionPolicy::Skip));
            Some((line_index, column, width))
        } else {
            None
        };
        let text = block.text.strip_suffix('\n').unwrap_or(block.text.as_str());
        if let Some(lang) = block.lang {
            let highlighted = highlight_code_to_lines(text, &lang, self.state.code_theme());
            self.push_lines(
                highlighted
                    .into_iter()
                    .flat_map(|line| code_block_visual_lines(self.state, line, self.region_width))
                    .collect(),
                SelectionPolicy::StripPrefix(2),
            );
        } else {
            for line in text.split('\n') {
                self.push_lines(
                    code_block_lines(self.state, line, self.region_width),
                    SelectionPolicy::StripPrefix(2),
                );
            }
        }
        if let Some((line_index, column, width)) = copy_target {
            self.copy_targets.push(CopyTarget {
                line_index,
                column,
                width,
                content: CopyContent::Text(block.text),
            });
        }
    }

    fn flush_table(&mut self, table: TableState) {
        let wrapped = table_lines(self.state, table.rows, &table.alignments, self.region_width);
        self.push_wrapped_lines(wrapped, SelectionPolicy::Keep);
    }

    fn push_row(&mut self, row: TranscriptRow) {
        self.rows.push(row);
    }

    fn push_lines(&mut self, lines: Vec<Line<'static>>, selection: SelectionPolicy) {
        self.rows.extend(
            lines
                .into_iter()
                .map(|line| TranscriptRow::new(line, selection)),
        );
    }

    fn push_wrapped_lines(&mut self, lines: Vec<WrappedLine>, selection: SelectionPolicy) {
        for wrapped in lines {
            let line_index = self.rows.len();
            for link in wrapped.links {
                self.link_targets.push(LinkTarget::new(
                    line_index,
                    link.column,
                    link.width,
                    link.url,
                ));
            }
            self.rows.push(TranscriptRow::new(wrapped.line, selection));
        }
    }

    fn push_style(&mut self, style: Style) {
        self.style_stack.push(style);
    }

    fn pop_style(&mut self) {
        if self.style_stack.len() > 1 {
            self.style_stack.pop();
        }
    }

    fn current_style(&self) -> Style {
        *self
            .style_stack
            .last()
            .expect("style stack always has a base style")
    }

    fn current_link_url(&self) -> Option<Arc<str>> {
        self.link_stack
            .last()
            .map(|link| Arc::clone(&link.dest_url))
    }

    fn start_link(&mut self, dest_url: &str) {
        self.link_stack.push(LinkState {
            dest_url: Arc::from(dest_url),
            has_visible_text: false,
        });
        self.push_style(link_style(self.state));
    }

    fn finish_link(&mut self) {
        self.pop_style();
        let Some(link) = self.link_stack.pop() else {
            return;
        };
        if link.has_visible_text || link.dest_url.trim().is_empty() {
            return;
        }
        let part = StyledTextPart::with_link(
            link.dest_url.as_ref().to_owned(),
            link_style(self.state),
            false,
            Some(link.dest_url),
        );
        if self.table.is_some() {
            self.push_table_part(part);
        } else {
            self.push_pending_list_marker();
            self.parts.push(part);
        }
    }

    fn record_link_label(&mut self, text: &str) {
        if let Some(link) = self.link_stack.last_mut()
            && !text.trim().is_empty()
        {
            link.has_visible_text = true;
        }
    }

    fn finish(mut self) -> RenderedMarkdown {
        self.flush_parts();
        if self.rows.is_empty() {
            self.push_row(TranscriptRow::new(
                Line::from(String::new()),
                SelectionPolicy::Keep,
            ));
        }
        RenderedMarkdown::new(self.rows, self.copy_targets, self.link_targets)
    }
}

fn code_block_lang(kind: CodeBlockKind<'_>) -> Option<String> {
    match kind {
        CodeBlockKind::Fenced(lang) => {
            let lang = lang.split_whitespace().next().unwrap_or_default();
            (!lang.is_empty()).then(|| lang.to_owned())
        }
        CodeBlockKind::Indented => None,
    }
}

fn code_block_visual_lines(
    state: &TuiState,
    line: Line<'static>,
    region_width: u16,
) -> Vec<Line<'static>> {
    const RAIL: &str = "▎ ";
    let rail_width = u16::try_from(UnicodeWidthStr::width(RAIL)).unwrap_or(u16::MAX);
    let content_width = region_width.saturating_sub(rail_width).max(1);
    let parts = line
        .spans
        .into_iter()
        .map(|span| StyledTextPart::new(span.content.into_owned(), span.style, false))
        .collect();
    let rail_style = semantic_style(state, SemanticColor::Status).add_modifier(Modifier::BOLD);
    let background = state.theme().color(SemanticColor::CodeBackground);
    wrap_styled_parts_preserving_leading_whitespace(parts, content_width)
        .into_iter()
        .map(|mut line| {
            line.spans.insert(0, Span::styled(RAIL, rail_style));
            if let Some(background) = background {
                for span in &mut line.spans {
                    span.style = span.style.bg(background);
                }
                let padding = usize::from(region_width).saturating_sub(spans_width(&line.spans));
                if padding > 0 {
                    line.spans.push(Span::styled(
                        " ".repeat(padding),
                        Style::default().bg(background),
                    ));
                }
            }
            line
        })
        .collect()
}

fn code_block_lines(state: &TuiState, text: &str, region_width: u16) -> Vec<Line<'static>> {
    code_block_visual_lines(
        state,
        Line::from(Span::styled(
            text.to_owned(),
            semantic_style(state, SemanticColor::Assistant),
        )),
        region_width,
    )
}

fn strong_style(state: &TuiState) -> Style {
    semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD)
}

fn strikethrough_style(state: &TuiState) -> Style {
    semantic_style(state, SemanticColor::Muted).add_modifier(Modifier::CROSSED_OUT)
}

fn quote_prefix(depth: usize) -> String {
    format!("{} ", ">".repeat(depth.max(1)))
}
