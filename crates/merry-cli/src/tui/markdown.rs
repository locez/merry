use super::{
    highlight::highlight_code_to_lines,
    render::{StyledTextPart, wrap_styled_parts, wrap_styled_parts_preserving_leading_whitespace},
    state::TuiState,
    theme::SemanticColor,
};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) fn markdown_lines(
    state: &TuiState,
    markdown: &str,
    region_width: u16,
) -> Vec<Line<'static>> {
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
    lines: Vec<Line<'static>>,
    parts: Vec<StyledTextPart>,
    style_stack: Vec<Style>,
    link_stack: Vec<LinkState>,
    quote_depth: usize,
    list_stack: Vec<ListState>,
    pending_list_marker: Option<String>,
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
    dest_url: String,
    label: String,
}

#[derive(Debug, Default, Clone)]
struct TableState {
    rows: Vec<TableRow>,
    current_row: Option<TableRow>,
    current_cell: Option<Vec<StyledTextPart>>,
    in_head: bool,
}

#[derive(Debug, Clone)]
struct TableRow {
    cells: Vec<Vec<StyledTextPart>>,
    header: bool,
}

impl<'state> MarkdownRenderer<'state> {
    fn new(state: &'state TuiState, region_width: u16) -> Self {
        Self {
            state,
            region_width,
            lines: Vec::new(),
            parts: Vec::new(),
            style_stack: vec![semantic_style(state, SemanticColor::Assistant)],
            link_stack: Vec::new(),
            quote_depth: 0,
            list_stack: Vec::new(),
            pending_list_marker: None,
            code_block: None,
            table: None,
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
                format!(" {} ", code.as_ref()),
                inline_code_style(self.state, self.current_style()),
            ),
            Event::SoftBreak | Event::HardBreak => self.flush_parts(),
            Event::Rule => {
                self.flush_parts();
                self.lines.push(Line::from(Span::styled(
                    "-".repeat(usize::from(self.region_width).max(1)),
                    semantic_style(self.state, SemanticColor::Muted),
                )));
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
            Tag::Paragraph => {}
            Tag::Heading { .. } => {
                self.flush_parts();
                self.push_style(
                    semantic_style(self.state, SemanticColor::Focus).add_modifier(Modifier::BOLD),
                );
            }
            Tag::BlockQuote(_) => {
                self.flush_parts();
                self.quote_depth = self.quote_depth.saturating_add(1);
            }
            Tag::CodeBlock(kind) => {
                self.flush_parts();
                self.code_block = Some(CodeBlockState {
                    lang: code_block_lang(kind),
                    text: String::new(),
                });
            }
            Tag::List(start) => self.list_stack.push(ListState { next_number: start }),
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
            Tag::Table(_) => {
                self.flush_parts();
                self.table = Some(TableState::default());
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
            TagEnd::Heading(_) => {
                self.flush_parts();
                self.pop_style();
            }
            TagEnd::CodeBlock => {}
            TagEnd::List(_) => {
                self.flush_parts();
                self.list_stack.pop();
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
                self.push_table_part(StyledTextPart {
                    text: text.to_string(),
                    style: self.current_style(),
                    atomic: false,
                });
            }
            Event::Code(code) => {
                self.push_table_part(StyledTextPart {
                    text: format!(" {} ", code.as_ref()),
                    style: inline_code_style(self.state, self.current_style()),
                    atomic: true,
                });
            }
            Event::SoftBreak | Event::HardBreak => {
                self.push_table_part(StyledTextPart {
                    text: " ".to_owned(),
                    style: self.current_style(),
                    atomic: false,
                });
            }
            Event::TaskListMarker(checked) => {
                self.push_table_part(StyledTextPart {
                    text: if *checked { "[x] " } else { "[ ] " }.to_owned(),
                    style: self.current_style(),
                    atomic: false,
                });
            }
            Event::FootnoteReference(reference) => {
                self.push_table_part(StyledTextPart {
                    text: reference.to_string(),
                    style: self.current_style(),
                    atomic: false,
                });
            }
            Event::Rule | Event::Start(_) | Event::End(_) => {}
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
        self.record_link_label(text);
        self.parts.push(StyledTextPart {
            text: text.to_owned(),
            style: self.current_style(),
            atomic: false,
        });
    }

    fn push_atomic_text(&mut self, text: String, style: Style) {
        if text.is_empty() {
            return;
        }
        self.push_pending_list_marker();
        self.record_link_label(&text);
        self.parts.push(StyledTextPart {
            text,
            style,
            atomic: true,
        });
    }

    fn push_pending_list_marker(&mut self) {
        let Some(marker) = self.pending_list_marker.take() else {
            return;
        };
        self.parts.push(StyledTextPart {
            text: marker,
            style: semantic_style(self.state, SemanticColor::Muted),
            atomic: true,
        });
    }

    fn flush_parts(&mut self) {
        self.push_pending_list_marker();
        if self.parts.is_empty() {
            return;
        }
        let parts = std::mem::take(&mut self.parts);
        if self.quote_depth == 0 {
            self.lines
                .extend(wrap_styled_parts(parts, self.region_width));
            return;
        }

        let prefix = quote_prefix(self.quote_depth);
        let prefix_width = UnicodeWidthStr::width(prefix.as_str());
        let content_width = self
            .region_width
            .saturating_sub(u16::try_from(prefix_width).unwrap_or(u16::MAX))
            .max(1);
        let prefix_style =
            semantic_style(self.state, SemanticColor::Focus).add_modifier(Modifier::BOLD);
        self.lines.extend(
            wrap_styled_parts(parts, content_width)
                .into_iter()
                .map(|mut line| {
                    line.spans
                        .insert(0, Span::styled(prefix.clone(), prefix_style));
                    line
                }),
        );
    }

    fn flush_code_block(&mut self, block: CodeBlockState) {
        let text = block
            .text
            .strip_suffix('\n')
            .unwrap_or(block.text.as_str())
            .to_owned();
        if let Some(lang) = block.lang {
            let highlighted = highlight_code_to_lines(&text, &lang, self.state.code_theme());
            self.lines.extend(
                highlighted
                    .into_iter()
                    .flat_map(|line| code_block_visual_lines(self.state, line, self.region_width)),
            );
            return;
        }
        for line in text.split('\n') {
            self.lines
                .extend(code_block_lines(self.state, line, self.region_width));
        }
    }

    fn flush_table(&mut self, table: TableState) {
        self.lines
            .extend(table_lines(self.state, table.rows, self.region_width));
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

    fn start_link(&mut self, dest_url: &str) {
        self.link_stack.push(LinkState {
            dest_url: dest_url.to_owned(),
            label: String::new(),
        });
        self.push_style(link_style(self.state, dest_url));
    }

    fn finish_link(&mut self) {
        self.pop_style();
        let Some(link) = self.link_stack.pop() else {
            return;
        };
        if link_url_is_visible_in_label(&link) {
            return;
        }
        let text = format!(" {}", link.dest_url);
        let part = StyledTextPart {
            text,
            style: link_style(self.state, &link.dest_url),
            atomic: true,
        };
        if self.table.is_some() {
            self.push_table_part(part);
        } else {
            self.push_pending_list_marker();
            self.parts.push(part);
        }
    }

    fn record_link_label(&mut self, text: &str) {
        if let Some(link) = self.link_stack.last_mut() {
            link.label.push_str(text);
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush_parts();
        if self.lines.is_empty() {
            self.lines.push(Line::from(String::new()));
        }
        self.lines
    }
}

impl TableState {
    fn start_row(&mut self) {
        self.current_row = Some(TableRow {
            cells: Vec::new(),
            header: self.in_head,
        });
    }

    fn finish_row(&mut self) {
        self.finish_cell();
        if let Some(row) = self.current_row.take()
            && !row.cells.is_empty()
        {
            self.rows.push(row);
        }
    }

    fn start_cell(&mut self) {
        self.current_cell = Some(Vec::new());
    }

    fn finish_cell(&mut self) {
        let Some(cell) = self.current_cell.take() else {
            return;
        };
        if let Some(row) = &mut self.current_row {
            row.cells.push(cell);
        }
    }

    fn push_part(&mut self, part: StyledTextPart) {
        if self.current_cell.is_none() {
            self.start_cell();
        }
        if let Some(cell) = &mut self.current_cell {
            cell.push(part);
        }
    }
}

fn table_lines(state: &TuiState, rows: Vec<TableRow>, region_width: u16) -> Vec<Line<'static>> {
    if rows.is_empty() {
        return Vec::new();
    }

    let column_count = rows.iter().map(|row| row.cells.len()).max().unwrap_or(0);
    if column_count == 0 {
        return Vec::new();
    }

    let mut widths = vec![1; column_count];
    for row in &rows {
        for (index, cell) in row.cells.iter().enumerate() {
            widths[index] = widths[index].max(styled_parts_width(cell));
        }
    }
    fit_table_widths(&mut widths, usize::from(region_width).max(1));

    let mut lines = Vec::new();
    for row in rows {
        lines.push(table_row_line(state, &row, &widths));
        if row.header {
            lines.push(table_separator_line(state, &widths));
        }
    }
    lines
}

fn table_row_line(state: &TuiState, row: &TableRow, widths: &[usize]) -> Line<'static> {
    let mut spans = Vec::new();
    spans.push(Span::styled(
        " ".to_owned(),
        semantic_style(state, SemanticColor::Muted),
    ));
    for (index, width) in widths.iter().copied().enumerate() {
        if index > 0 {
            spans.push(Span::styled(
                "  ".to_owned(),
                semantic_style(state, SemanticColor::Muted),
            ));
        }
        let cell = row.cells.get(index).map(Vec::as_slice).unwrap_or(&[]);
        let cell_style = if row.header {
            semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD)
        } else {
            semantic_style(state, SemanticColor::Assistant)
        };
        let mut cell_spans = if row.header {
            truncate_styled_parts_with_override(cell, width, cell_style)
        } else {
            truncate_styled_parts(cell, width, cell_style)
        };
        let cell_width = spans_width(&cell_spans);
        spans.append(&mut cell_spans);
        if cell_width < width {
            spans.push(Span::styled(
                " ".repeat(width - cell_width),
                semantic_style(state, SemanticColor::Muted),
            ));
        }
    }
    Line::from(spans)
}

fn table_separator_line(state: &TuiState, widths: &[usize]) -> Line<'static> {
    let mut text = String::from(" ");
    for (index, width) in widths.iter().copied().enumerate() {
        if index > 0 {
            text.push_str("  ");
        }
        text.push_str(&"-".repeat(width.max(3)));
    }
    Line::from(Span::styled(
        text,
        semantic_style(state, SemanticColor::Muted),
    ))
}

fn fit_table_widths(widths: &mut [usize], max_width: usize) {
    if widths.is_empty() {
        return;
    }
    let separators = widths.len().saturating_sub(1) * 2 + 1;
    let available = max_width.saturating_sub(separators).max(widths.len());
    let mut total: usize = widths.iter().sum();
    while total > available {
        let Some((index, width)) = widths
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, width)| *width > 3)
            .max_by_key(|(_, width)| *width)
        else {
            break;
        };
        widths[index] = width.saturating_sub(1);
        total = total.saturating_sub(1);
    }
}

fn styled_parts_width(parts: &[StyledTextPart]) -> usize {
    parts
        .iter()
        .map(|part| UnicodeWidthStr::width(part.text.as_str()))
        .sum()
}

fn spans_width(spans: &[Span<'static>]) -> usize {
    spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

fn truncate_styled_parts(
    parts: &[StyledTextPart],
    max_width: usize,
    fallback_style: Style,
) -> Vec<Span<'static>> {
    truncate_styled_parts_impl(parts, max_width, fallback_style, None)
}

fn truncate_styled_parts_with_override(
    parts: &[StyledTextPart],
    max_width: usize,
    style: Style,
) -> Vec<Span<'static>> {
    truncate_styled_parts_impl(parts, max_width, style, Some(style))
}

fn truncate_styled_parts_impl(
    parts: &[StyledTextPart],
    max_width: usize,
    fallback_style: Style,
    style_override: Option<Style>,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut used = 0;
    for part in parts {
        if used >= max_width {
            break;
        }
        let remaining = max_width - used;
        let text = truncate_to_width(&part.text, remaining);
        if text.is_empty() {
            continue;
        }
        used += UnicodeWidthStr::width(text.as_str());
        spans.push(Span::styled(text, style_override.unwrap_or(part.style)));
    }
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), fallback_style));
    }
    spans
}

fn truncate_to_width(text: &str, max_width: usize) -> String {
    let mut width = 0;
    let mut result = String::new();
    for character in text.chars() {
        let char_width = character.width().unwrap_or(0);
        if width + char_width > max_width {
            break;
        }
        width += char_width;
        result.push(character);
    }
    result
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
        .map(|span| StyledTextPart {
            text: span.content.into_owned(),
            style: span.style,
            atomic: false,
        })
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

fn link_style(state: &TuiState, _url: &str) -> Style {
    semantic_style(state, SemanticColor::Command).add_modifier(Modifier::UNDERLINED)
}

fn inline_code_style(state: &TuiState, base: Style) -> Style {
    base.fg.unwrap_or_default();
    semantic_style(state, SemanticColor::Focus).add_modifier(Modifier::BOLD)
}

fn strikethrough_style(state: &TuiState) -> Style {
    semantic_style(state, SemanticColor::Muted).add_modifier(Modifier::CROSSED_OUT)
}

fn semantic_style(state: &TuiState, color: SemanticColor) -> Style {
    state
        .theme()
        .color(color)
        .map_or_else(Style::default, |color| Style::default().fg(color))
}

fn quote_prefix(depth: usize) -> String {
    format!("{} ", ">".repeat(depth.max(1)))
}

fn link_url_is_visible_in_label(link: &LinkState) -> bool {
    link.label.trim() == link.dest_url.trim()
}
