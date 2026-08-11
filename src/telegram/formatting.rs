use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

pub fn markdown_to_telegram_rich_html(markdown: &str) -> String {
    let normalized = normalize_model_markdown(markdown);
    let mut options = Options::all();
    options.remove(Options::ENABLE_TABLES);
    let parser = Parser::new_ext(&normalized, options);
    let mut renderer = Renderer::default();

    for event in parser {
        renderer.render(event);
    }
    renderer.finish()
}

fn normalize_model_markdown(markdown: &str) -> String {
    let normalized = markdown
        .replace("<br />", "\n")
        .replace("<br/>", "\n")
        .replace("<br>", "\n")
        .replace("\\[", "$$")
        .replace("\\]", "$$")
        .replace("\\(", "$")
        .replace("\\)", "$");

    let normalized = normalize_markdown_tables(&normalized);
    normalize_bare_latex_lines(&normalized)
}

fn normalize_markdown_tables(markdown: &str) -> String {
    let lines = markdown.lines().collect::<Vec<_>>();
    let mut normalized = Vec::with_capacity(lines.len());
    let mut index = 0;

    while index < lines.len() {
        if index + 1 < lines.len() {
            let header = split_table_cells(lines[index]);
            let separator = split_table_cells(lines[index + 1]);
            if header.len() >= 2
                && header.len() == separator.len()
                && separator.iter().all(|cell| is_table_separator(cell))
            {
                normalized.push(format!("**{}**", header.join(" → ")));
                index += 2;
                while index < lines.len() {
                    let cells = split_table_cells(lines[index]);
                    if cells.len() != header.len() {
                        break;
                    }
                    normalized.push(format!("- {}", cells.join(" → ")));
                    index += 1;
                }
                normalized.push(String::new());
                continue;
            }
        }

        normalized.push(lines[index].to_owned());
        index += 1;
    }

    normalized.join("\n")
}

fn split_table_cells(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return Vec::new();
    }

    let mut cells = Vec::new();
    let mut current = String::new();
    let mut characters = trimmed.chars().peekable();
    let mut in_math = false;
    while let Some(character) = characters.next() {
        if character == '$' {
            current.push(character);
            while characters.peek() == Some(&'$') {
                current.push(characters.next().expect("peeked dollar must exist"));
            }
            in_math = !in_math;
        } else if character == '|' && !in_math {
            let cell = current.trim();
            if !cell.is_empty() {
                cells.push(cell.to_owned());
            }
            current.clear();
        } else {
            current.push(character);
        }
    }
    let cell = current.trim();
    if !cell.is_empty() {
        cells.push(cell.to_owned());
    }
    cells
}

fn is_table_separator(cell: &str) -> bool {
    cell.chars().filter(|character| *character == '-').count() >= 3
        && cell.chars().all(|character| matches!(character, '-' | ':'))
}

fn normalize_bare_latex_lines(markdown: &str) -> String {
    let lines = markdown.lines().collect::<Vec<_>>();
    let mut normalized = Vec::with_capacity(lines.len());
    let mut index = 0;
    let mut in_code_fence = false;

    while index < lines.len() {
        let line = lines[index];
        let trimmed = trim_invisible_start(line.trim());
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_fence = !in_code_fence;
            normalized.push(line.to_owned());
            index += 1;
            continue;
        }

        if !in_code_fence && is_bare_latex_start(trimmed) {
            let mut expression = trimmed.to_owned();
            while latex_needs_continuation(&expression) && index + 1 < lines.len() {
                let continuation = trim_invisible_start(lines[index + 1].trim());
                if continuation.is_empty() || continuation.contains('$') {
                    break;
                }
                expression.push(' ');
                expression.push_str(continuation);
                index += 1;
            }
            normalized.push(format!("$${expression}$$"));
        } else if !in_code_fence {
            normalized.push(wrap_embedded_bare_latex(line));
        } else {
            normalized.push(line.to_owned());
        }
        index += 1;
    }

    normalized.join("\n")
}

fn trim_invisible_start(text: &str) -> &str {
    text.trim_start_matches(['\u{200b}', '\u{200c}', '\u{200d}', '\u{2060}', '\u{feff}'])
}

fn is_bare_latex_start(text: &str) -> bool {
    text.starts_with('\\') && !text.contains('$') && contains_latex_command(text)
}

fn wrap_embedded_bare_latex(line: &str) -> String {
    let Some(start) = find_bare_latex_command(line) else {
        return line.to_owned();
    };
    if line[..start].trim().is_empty() {
        return line.to_owned();
    }

    let prefix = line[..start].trim_end();
    let expression = line[start..].trim();
    format!("{prefix} ${expression}$")
}

fn find_bare_latex_command(text: &str) -> Option<usize> {
    let mut in_math = false;
    let mut in_code = false;
    let mut iterator = text.char_indices().peekable();

    while let Some((index, character)) = iterator.next() {
        match character {
            '`' if !in_math => in_code = !in_code,
            '$' if !in_code => {
                while iterator.peek().is_some_and(|(_, next)| *next == '$') {
                    iterator.next();
                }
                in_math = !in_math;
            }
            '\\' if !in_math && !in_code && contains_latex_command(&text[index..]) => {
                return Some(index);
            }
            _ => {}
        }
    }
    None
}

fn latex_needs_continuation(text: &str) -> bool {
    text.matches("\\left").count() > text.matches("\\right").count()
}

fn contains_latex_command(text: &str) -> bool {
    [
        "\\int",
        "\\frac",
        "\\sum",
        "\\prod",
        "\\sqrt",
        "\\lim",
        "\\partial",
        "\\begin",
        "\\left",
        "\\right",
        "\\mathrm",
        "\\text",
        "\\boxed",
        "\\displaystyle",
    ]
    .iter()
    .any(|command| text.contains(command))
}

#[derive(Default)]
struct Renderer {
    output: String,
    lists: Vec<ListState>,
    links: Vec<bool>,
    blockquote_depth: usize,
}

struct ListState {
    next_number: Option<u64>,
}

impl Renderer {
    fn render(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) => push_escaped_text(&mut self.output, &text),
            Event::Code(code) => {
                self.output.push_str("<code>");
                push_escaped_text(&mut self.output, &code);
                self.output.push_str("</code>");
            }
            Event::InlineMath(math) => {
                self.output.push_str("<tg-math>");
                push_escaped_text(&mut self.output, &sanitize_telegram_latex(&math));
                self.output.push_str("</tg-math>");
            }
            Event::DisplayMath(math) => {
                self.output.push_str("<tg-math-block>");
                push_escaped_text(&mut self.output, &sanitize_telegram_latex(&math));
                self.output.push_str("</tg-math-block>");
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                push_escaped_text(&mut self.output, &html);
            }
            Event::FootnoteReference(reference) => {
                self.output.push('[');
                push_escaped_text(&mut self.output, &reference);
                self.output.push(']');
            }
            Event::SoftBreak | Event::HardBreak => self.output.push('\n'),
            Event::Rule => self.output.push_str("\n—————\n"),
            Event::TaskListMarker(checked) => {
                self.output.push_str(if checked { "☑ " } else { "☐ " });
            }
        }
    }

    fn start_tag(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { .. } => self.output.push_str("<b>"),
            Tag::BlockQuote(_) => {
                if self.blockquote_depth == 0 {
                    self.output.push_str("<blockquote>");
                }
                self.blockquote_depth += 1;
            }
            Tag::CodeBlock(_) => self.output.push_str("<pre><code>"),
            Tag::HtmlBlock => {}
            Tag::List(first) => {
                self.lists.push(ListState { next_number: first });
            }
            Tag::DefinitionList => {}
            Tag::DefinitionListTitle => self.output.push_str("<b>"),
            Tag::DefinitionListDefinition => self.output.push_str("\n  "),
            Tag::Item => {
                let indent = "  ".repeat(self.lists.len().saturating_sub(1));
                self.output.push_str(&indent);
                if let Some(list) = self.lists.last_mut() {
                    if let Some(number) = list.next_number.as_mut() {
                        self.output.push_str(&format!("{number}. "));
                        *number += 1;
                    } else {
                        self.output.push_str("• ");
                    }
                }
            }
            Tag::FootnoteDefinition(_) => {}
            Tag::Table(_) | Tag::TableHead | Tag::TableRow | Tag::TableCell => {}
            Tag::Emphasis => self.output.push_str("<i>"),
            Tag::Strong => self.output.push_str("<b>"),
            Tag::Strikethrough => self.output.push_str("<s>"),
            Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                let safe = is_safe_link(&dest_url);
                self.links.push(safe);
                if safe {
                    self.output.push_str("<a href=\"");
                    push_escaped_attribute(&mut self.output, &dest_url);
                    self.output.push_str("\">");
                }
            }
            Tag::MetadataBlock(_) => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.output.push_str("\n\n"),
            TagEnd::Heading(_) => self.output.push_str("</b>\n"),
            TagEnd::BlockQuote(_) => {
                self.blockquote_depth = self.blockquote_depth.saturating_sub(1);
                if self.blockquote_depth == 0 {
                    self.output.push_str("</blockquote>\n");
                }
            }
            TagEnd::CodeBlock => self.output.push_str("</code></pre>\n"),
            TagEnd::HtmlBlock => self.output.push('\n'),
            TagEnd::List(_) => {
                self.lists.pop();
                self.output.push('\n');
            }
            TagEnd::DefinitionList => self.output.push('\n'),
            TagEnd::DefinitionListTitle => self.output.push_str("</b>"),
            TagEnd::DefinitionListDefinition => self.output.push('\n'),
            TagEnd::Item => self.output.push('\n'),
            TagEnd::FootnoteDefinition => self.output.push('\n'),
            TagEnd::Table => self.output.push('\n'),
            TagEnd::TableHead | TagEnd::TableRow => self.output.push('\n'),
            TagEnd::TableCell => self.output.push_str(" | "),
            TagEnd::Emphasis => self.output.push_str("</i>"),
            TagEnd::Strong => self.output.push_str("</b>"),
            TagEnd::Strikethrough => self.output.push_str("</s>"),
            TagEnd::Link | TagEnd::Image => {
                if self.links.pop().unwrap_or(false) {
                    self.output.push_str("</a>");
                }
            }
            TagEnd::MetadataBlock(_) => {}
        }
    }

    fn finish(mut self) -> String {
        while self.output.contains("\n\n\n") {
            self.output = self.output.replace("\n\n\n", "\n\n");
        }
        self.output.trim().to_owned()
    }
}

fn sanitize_telegram_latex(latex: &str) -> String {
    let mut sanitized = latex.replace("\\boxed{", "{");
    for command in ["\\displaystyle", "\\textstyle"] {
        sanitized = sanitized.replace(command, "");
    }
    sanitized
}

fn push_escaped_text(output: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            _ => output.push(character),
        }
    }
}

fn push_escaped_attribute(output: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '"' => output.push_str("&quot;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            _ => output.push(character),
        }
    }
}

fn is_safe_link(link: &str) -> bool {
    let lowercase = link.to_ascii_lowercase();
    ["https://", "http://", "tg://", "mailto:", "tel:"]
        .iter()
        .any(|scheme| lowercase.starts_with(scheme))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_common_markdown_to_telegram_html() {
        let markdown = "# Title\n\n**bold** and *italic* with `code`.\n\n- one\n- two";
        let html = markdown_to_telegram_rich_html(markdown);
        assert!(html.contains("<b>Title</b>"));
        assert!(html.contains("<b>bold</b>"));
        assert!(html.contains("<i>italic</i>"));
        assert!(html.contains("<code>code</code>"));
        assert!(html.contains("• one"));
    }

    #[test]
    fn escapes_model_html_and_code() {
        let html = markdown_to_telegram_rich_html("Use `<tag>` & never <script>alert(1)</script>.");
        assert!(html.contains("<code>&lt;tag&gt;</code>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn keeps_only_safe_link_schemes() {
        let html = markdown_to_telegram_rich_html(
            "[safe](https://example.com) [unsafe](javascript:alert(1))",
        );
        assert!(html.contains("<a href=\"https://example.com\">safe</a>"));
        assert!(!html.contains("javascript:"));
        assert!(html.contains("unsafe"));
    }

    #[test]
    fn converts_latex_to_native_telegram_math() {
        let html = markdown_to_telegram_rich_html("Inline $x^2 + y^2$ and block:\n\n$$E = mc^2$$");

        assert!(html.contains("<tg-math>x^2 + y^2</tg-math>"));
        assert!(html.contains("<tg-math-block>E = mc^2</tg-math-block>"));
    }

    #[test]
    fn converts_backslash_latex_delimiters_to_native_telegram_math() {
        let html = markdown_to_telegram_rich_html(
            "Inline \\(\\int \\sin x\\,dx = -\\cos x + C\\) and block:\n\n\\[E = mc^2\\]",
        );

        assert!(html.contains("<tg-math>\\int \\sin x\\,dx = -\\cos x + C</tg-math>"));
        assert!(html.contains("<tg-math-block>E = mc^2</tg-math-block>"));
        assert!(!html.contains("\\("));
        assert!(!html.contains("\\["));
    }

    #[test]
    fn converts_bare_latex_line_and_html_breaks() {
        let html = markdown_to_telegram_rich_html(
            "Substitute first.<br>\\int f(kx+c)\\,dx = \\frac{1}{k}\\int f(u)\\,du.",
        );

        assert!(html.contains("Substitute first.\n"));
        assert!(html.contains(
            "<tg-math-block>\\int f(kx+c)\\,dx = \\frac{1}{k}\\int f(u)\\,du.</tg-math-block>"
        ));
        assert!(!html.contains("&lt;br&gt;"));
    }

    #[test]
    fn preserves_absolute_values_in_markdown_table_text() {
        let html = markdown_to_telegram_rich_html(
            "Integral | Result\n--- | ---\n$\\int \\tan x\\,dx$ | $-\\ln|\\cos x|+C$",
        );

        assert!(html.contains("<tg-math>\\int \\tan x\\,dx</tg-math>"));
        assert!(html.contains("<tg-math>-\\ln|\\cos x|+C</tg-math>"));
    }

    #[test]
    fn converts_multiline_bare_latex_from_model_output() {
        let html = markdown_to_telegram_rich_html(
            "b) $${}$$ Split into fractions:\n\u{200b}\\frac{1}{x^{2}-a^{2}}=\\frac{1}{2a}\\left(\\frac{1}{x-a}-\\frac{1}{x+a}\n\\right).\n\ntherefore\n\n\\int\\frac{dx}{x^{2}-a^{2}}",
        );

        assert!(html.contains("<tg-math-block>\\frac{1}{x^{2}-a^{2}}=\\frac{1}{2a}\\left(\\frac{1}{x-a}-\\frac{1}{x+a} \\right).</tg-math-block>"));
        assert!(html.contains("<tg-math-block>\\int\\frac{dx}{x^{2}-a^{2}}</tg-math-block>"));
    }

    #[test]
    fn converts_bare_latex_after_russian_prose() {
        let html = markdown_to_telegram_rich_html(
            "2. Произведение (правило Лейбница) \\frac{d}{dx}\\bigl(u(x)v(x)\\bigr)=u'(x)v(x)+u(x)v'(x)",
        );

        assert!(html.contains("2. Произведение (правило Лейбница)"));
        assert!(html.contains(
            "<tg-math>\\frac{d}{dx}\\bigl(u(x)v(x)\\bigr)=u'(x)v(x)+u(x)v'(x)</tg-math>"
        ));
    }

    #[test]
    fn converts_markdown_math_table_to_bullet_rows() {
        let html = markdown_to_telegram_rich_html(
            "4. Производные\n\n| Функция | Производная |\n| --- | --- |\n| $c$ | $0$ |\n| $x^n$ | $n x^{n-1}$ |",
        );

        assert!(html.contains("<b>Функция → Производная</b>"));
        assert!(html.contains("• <tg-math>c</tg-math> → <tg-math>0</tg-math>"));
        assert!(html.contains("• <tg-math>x^n</tg-math> → <tg-math>n x^{n-1}</tg-math>"));
        assert!(!html.contains(" | "));
    }

    #[test]
    fn simplifies_decorative_latex_from_russian_chain_rule() {
        let html = markdown_to_telegram_rich_html(
            "то её производная находится так:\n\\boxed{\\displaystyle \\frac{dy}{dx}=f'\\!\\bigl(u(x)\\bigr)\\,u'(x)}.",
        );

        assert!(html.contains(
            "<tg-math-block>{ \\frac{dy}{dx}=f'\\!\\bigl(u(x)\\bigr)\\,u'(x)}.</tg-math-block>"
        ));
        assert!(!html.contains("\\boxed"));
        assert!(!html.contains("\\displaystyle"));
        assert!(html.contains("\\bigl"));
    }
}
