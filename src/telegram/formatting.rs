use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

pub fn markdown_to_telegram_rich_html(markdown: &str) -> String {
    let parser = Parser::new_ext(markdown, Options::all());
    let mut renderer = Renderer::default();

    for event in parser {
        renderer.render(event);
    }
    renderer.finish()
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
                push_escaped_text(&mut self.output, &math);
                self.output.push_str("</tg-math>");
            }
            Event::DisplayMath(math) => {
                self.output.push_str("<tg-math-block>");
                push_escaped_text(&mut self.output, &math);
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
}
