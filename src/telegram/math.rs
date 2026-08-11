//! Normalizes the Markdown/LaTeX dialects commonly emitted by chat models.
//!
//! Model formatting is best-effort, so the input boundary accepts explicit
//! Markdown math, TeX delimiters, labelled math fences, and conservative bare
//! TeX expressions. The output is canonical Markdown math for pulldown-cmark.

const MATH_FENCE_LANGUAGES: &[&str] = &["math", "latex", "tex", "katex"];

const MATH_COMMANDS: &[&str] = &[
    "\\operatorname",
    "\\displaystyle",
    "\\overline",
    "\\underline",
    "\\arctan",
    "\\arcsin",
    "\\arccos",
    "\\partial",
    "\\nabla",
    "\\begin",
    "\\end",
    "\\right",
    "\\left",
    "\\boxed",
    "\\mathrm",
    "\\text",
    "\\dfrac",
    "\\tfrac",
    "\\frac",
    "\\sqrt",
    "\\iiint",
    "\\iint",
    "\\oint",
    "\\int",
    "\\prod",
    "\\sum",
    "\\lim",
    "\\infty",
    "\\lambda",
    "\\theta",
    "\\alpha",
    "\\beta",
    "\\gamma",
    "\\delta",
    "\\Delta",
    "\\vec",
    "\\hat",
    "\\bar",
    "\\exp",
    "\\log",
    "\\ln",
    "\\sin",
    "\\cos",
    "\\tan",
    "\\cot",
    "\\sec",
    "\\csc",
];

pub(super) fn normalize_model_math(markdown: &str) -> String {
    let normalized = normalize_fenced_and_explicit_math(markdown);
    normalize_bare_math(&normalized)
}

pub(super) fn sanitize_for_telegram(latex: &str) -> String {
    let mut sanitized = latex.replace("\\boxed{", "{");
    for command in ["\\dfrac", "\\tfrac"] {
        sanitized = replace_tex_command(&sanitized, command, "\\frac");
    }

    for command in [
        "\\displaystyle",
        "\\textstyle",
        "\\Biggl",
        "\\Biggr",
        "\\biggl",
        "\\biggr",
        "\\Bigl",
        "\\Bigr",
        "\\bigl",
        "\\bigr",
        "\\Bigg",
        "\\bigg",
        "\\Big",
        "\\big",
        "\\middle",
        "\\left",
        "\\right",
        "\\!",
    ] {
        sanitized = replace_tex_command(&sanitized, command, "");
    }
    for command in [
        "\\qquad",
        "\\quad",
        "\\thinspace",
        "\\medspace",
        "\\thickspace",
        "\\enspace",
        "\\,",
        "\\:",
        "\\;",
    ] {
        sanitized = replace_tex_command(&sanitized, command, " ");
    }

    sanitized
}

fn replace_tex_command(latex: &str, command: &str, replacement: &str) -> String {
    let command_is_word = command
        .as_bytes()
        .last()
        .is_some_and(u8::is_ascii_alphabetic);
    let mut output = String::with_capacity(latex.len());
    let mut index = 0;

    while index < latex.len() {
        let rest = &latex[index..];
        let matches_command = rest.starts_with(command);
        let has_command_boundary = matches_command
            && (!command_is_word
                || rest[command.len()..]
                    .chars()
                    .next()
                    .is_none_or(|next| !next.is_ascii_alphabetic()));
        if has_command_boundary && !is_escaped(latex, index) {
            output.push_str(replacement);
            index += command.len();
        } else {
            push_next_character(latex, &mut output, &mut index);
        }
    }

    output
}

#[derive(Clone, Copy)]
struct Fence<'a> {
    marker: u8,
    width: usize,
    language: &'a str,
}

fn normalize_fenced_and_explicit_math(markdown: &str) -> String {
    let lines = markdown.lines().collect::<Vec<_>>();
    let mut segments = Vec::new();
    let mut text_start = 0;
    let mut index = 0;

    while index < lines.len() {
        let Some(fence) = parse_fence(lines[index]) else {
            index += 1;
            continue;
        };

        if text_start < index {
            segments.push(normalize_explicit_delimiters(
                &lines[text_start..index].join("\n"),
            ));
        }

        let closing =
            ((index + 1)..lines.len()).find(|candidate| is_closing_fence(lines[*candidate], fence));
        let Some(closing) = closing else {
            segments.push(lines[index..].join("\n"));
            text_start = lines.len();
            break;
        };

        if is_math_fence(fence.language) {
            let expression = lines[index + 1..closing].join("\n");
            segments.push(format!("$${}$$", strip_outer_math_delimiters(&expression)));
        } else {
            segments.push(lines[index..=closing].join("\n"));
        }

        index = closing + 1;
        text_start = index;
    }

    if text_start < lines.len() {
        segments.push(normalize_explicit_delimiters(
            &lines[text_start..].join("\n"),
        ));
    }

    segments.join("\n")
}

fn parse_fence(line: &str) -> Option<Fence<'_>> {
    let trimmed = line.trim_start();
    let marker = *trimmed.as_bytes().first()?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let width = trimmed
        .as_bytes()
        .iter()
        .take_while(|character| **character == marker)
        .count();
    if width < 3 {
        return None;
    }
    let language = trimmed[width..]
        .trim()
        .trim_start_matches("{.")
        .trim_end_matches('}');
    Some(Fence {
        marker,
        width,
        language,
    })
}

fn is_closing_fence(line: &str, fence: Fence<'_>) -> bool {
    let trimmed = line.trim();
    let width = trimmed
        .as_bytes()
        .iter()
        .take_while(|character| **character == fence.marker)
        .count();
    width >= fence.width && trimmed[width..].trim().is_empty()
}

fn is_math_fence(language: &str) -> bool {
    MATH_FENCE_LANGUAGES
        .iter()
        .any(|candidate| language.eq_ignore_ascii_case(candidate))
}

fn strip_outer_math_delimiters(expression: &str) -> &str {
    let trimmed = expression.trim();
    for (opening, closing) in [("\\[", "\\]"), ("\\(", "\\)"), ("$$", "$$"), ("$", "$")] {
        if trimmed.len() >= opening.len() + closing.len()
            && trimmed.starts_with(opening)
            && trimmed.ends_with(closing)
        {
            return trimmed[opening.len()..trimmed.len() - closing.len()].trim();
        }
    }
    trimmed
}

#[derive(Clone, Copy)]
enum ScanMode {
    Text,
    Code(usize),
    Dollar(usize),
    Explicit { display: bool, output_start: usize },
}

fn normalize_explicit_delimiters(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut mode = ScanMode::Text;
    let mut index = 0;

    while index < text.len() {
        match mode {
            ScanMode::Text => {
                if text.as_bytes()[index] == b'`' {
                    let width = byte_run(text, index, b'`');
                    output.push_str(&text[index..index + width]);
                    mode = ScanMode::Code(width);
                    index += width;
                } else if text.as_bytes()[index] == b'$' && !is_escaped(text, index) {
                    let width = byte_run(text, index, b'$').min(2);
                    if has_closing_dollars(text, index + width, width) {
                        output.push_str(&text[index..index + width]);
                        mode = ScanMode::Dollar(width);
                        index += width;
                    } else {
                        push_next_character(text, &mut output, &mut index);
                    }
                } else if text[index..].starts_with("\\(") && !is_escaped(text, index) {
                    let output_start = output.len();
                    output.push('$');
                    mode = ScanMode::Explicit {
                        display: false,
                        output_start,
                    };
                    index += 2;
                } else if text[index..].starts_with("\\[") && !is_escaped(text, index) {
                    let output_start = output.len();
                    output.push_str("$$");
                    mode = ScanMode::Explicit {
                        display: true,
                        output_start,
                    };
                    index += 2;
                } else {
                    push_next_character(text, &mut output, &mut index);
                }
            }
            ScanMode::Code(width) => {
                if text.as_bytes()[index] == b'`' && byte_run(text, index, b'`') >= width {
                    output.push_str(&text[index..index + width]);
                    index += width;
                    mode = ScanMode::Text;
                } else {
                    push_next_character(text, &mut output, &mut index);
                }
            }
            ScanMode::Dollar(width) => {
                if text.as_bytes()[index] == b'$'
                    && !is_escaped(text, index)
                    && byte_run(text, index, b'$') >= width
                {
                    output.push_str(&text[index..index + width]);
                    index += width;
                    mode = ScanMode::Text;
                } else {
                    push_next_character(text, &mut output, &mut index);
                }
            }
            ScanMode::Explicit {
                display,
                output_start,
            } => {
                let closing = if display { "\\]" } else { "\\)" };
                let closed = text[index..].starts_with(closing) && !is_escaped(text, index);
                if closed {
                    output.push_str(if display { "$$" } else { "$" });
                    index += 2;
                    mode = ScanMode::Text;
                } else {
                    push_next_character(text, &mut output, &mut index);
                }

                if !closed && index == text.len() {
                    if display {
                        output.replace_range(output_start..output_start + 2, "\\[");
                    } else {
                        output.replace_range(output_start..output_start + 1, "\\(");
                    }
                }
            }
        }
    }

    output
}

fn byte_run(text: &str, start: usize, byte: u8) -> usize {
    text.as_bytes()[start..]
        .iter()
        .take_while(|candidate| **candidate == byte)
        .count()
}

fn is_escaped(text: &str, index: usize) -> bool {
    text.as_bytes()[..index]
        .iter()
        .rev()
        .take_while(|character| **character == b'\\')
        .count()
        % 2
        == 1
}

fn has_closing_dollars(text: &str, mut index: usize, width: usize) -> bool {
    while index < text.len() {
        if text.as_bytes()[index] == b'$'
            && !is_escaped(text, index)
            && byte_run(text, index, b'$') >= width
        {
            return true;
        }
        index += text[index..]
            .chars()
            .next()
            .expect("index must be on a character boundary")
            .len_utf8();
    }
    false
}

fn push_next_character(text: &str, output: &mut String, index: &mut usize) {
    let character = text[*index..]
        .chars()
        .next()
        .expect("index must be on a character boundary");
    output.push(character);
    *index += character.len_utf8();
}

fn normalize_bare_math(markdown: &str) -> String {
    let lines = markdown.lines().collect::<Vec<_>>();
    let mut normalized = Vec::with_capacity(lines.len());
    let mut index = 0;
    let mut in_code_fence = false;
    let mut in_display_math = false;

    while index < lines.len() {
        let line = lines[index];
        let trimmed = trim_invisible_start(line.trim());
        if parse_fence(trimmed).is_some() {
            in_code_fence = !in_code_fence;
            normalized.push(line.to_owned());
            index += 1;
            continue;
        }

        if !in_code_fence && contains_display_delimiter(line) {
            if display_delimiter_count(line) % 2 == 1 {
                in_display_math = !in_display_math;
            }
            normalized.push(line.to_owned());
            index += 1;
            continue;
        }

        if in_code_fence || in_display_math {
            normalized.push(line.to_owned());
            index += 1;
            continue;
        }

        if is_bare_math_line(trimmed) {
            let mut expression = trimmed.to_owned();
            while expression_is_incomplete(&expression) && index + 1 < lines.len() {
                let continuation = trim_invisible_start(lines[index + 1].trim());
                if continuation.is_empty()
                    || continuation.contains('$')
                    || parse_fence(continuation).is_some()
                {
                    break;
                }
                expression.push(' ');
                expression.push_str(continuation);
                index += 1;
            }
            normalized.push(format!("$${expression}$$"));
        } else {
            normalized.push(wrap_embedded_bare_math(line));
        }
        index += 1;
    }

    normalized.join("\n")
}

fn trim_invisible_start(text: &str) -> &str {
    text.trim_start_matches(['\u{200b}', '\u{200c}', '\u{200d}', '\u{2060}', '\u{feff}'])
}

fn is_bare_math_line(text: &str) -> bool {
    if text.contains('$') {
        return false;
    }
    if math_command_at(text, 0) {
        return true;
    }

    let Some(command_start) = find_bare_math_command(text) else {
        return false;
    };
    let prefix = text[..command_start].trim();
    prefix.contains('=')
        && prefix.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || character.is_whitespace()
                || "_'()[]{}^=+-*/.,".contains(character)
        })
}

fn wrap_embedded_bare_math(line: &str) -> String {
    let Some(start) = find_bare_math_command(line) else {
        return line.to_owned();
    };
    if line[..start].trim().is_empty() {
        return line.to_owned();
    }

    let prefix = line[..start].trim_end();
    let expression = line[start..].trim();
    format!("{prefix} ${expression}$")
}

fn find_bare_math_command(text: &str) -> Option<usize> {
    let mut code_width = None;
    let mut math_width = None;
    let mut index = 0;

    while index < text.len() {
        if let Some(width) = code_width {
            if text.as_bytes()[index] == b'`' && byte_run(text, index, b'`') >= width {
                index += width;
                code_width = None;
            } else {
                index += next_character_width(text, index);
            }
        } else if let Some(width) = math_width {
            if text.as_bytes()[index] == b'$'
                && !is_escaped(text, index)
                && byte_run(text, index, b'$') >= width
            {
                index += width;
                math_width = None;
            } else {
                index += next_character_width(text, index);
            }
        } else if text.as_bytes()[index] == b'`' {
            let width = byte_run(text, index, b'`');
            index += width;
            code_width = Some(width);
        } else if text.as_bytes()[index] == b'$' && !is_escaped(text, index) {
            let width = byte_run(text, index, b'$').min(2);
            if has_closing_dollars(text, index + width, width) {
                index += width;
                math_width = Some(width);
            } else {
                index += 1;
            }
        } else if text.as_bytes()[index] == b'\\'
            && !is_escaped(text, index)
            && math_command_at(text, index)
        {
            return Some(index);
        } else {
            index += next_character_width(text, index);
        }
    }
    None
}

fn next_character_width(text: &str, index: usize) -> usize {
    text[index..]
        .chars()
        .next()
        .expect("index must be on a character boundary")
        .len_utf8()
}

fn math_command_at(text: &str, index: usize) -> bool {
    let rest = &text[index..];
    MATH_COMMANDS.iter().any(|command| {
        if !rest.starts_with(command) {
            return false;
        }
        rest[command.len()..]
            .chars()
            .next()
            .is_none_or(|next| !next.is_ascii_alphabetic())
    })
}

fn expression_is_incomplete(text: &str) -> bool {
    grouping_depth(text) > 0
        || text.matches("\\left").count() > text.matches("\\right").count()
        || text.matches("\\begin{").count() > text.matches("\\end{").count()
}

fn grouping_depth(text: &str) -> usize {
    let mut depth = 0usize;
    for (index, character) in text.char_indices() {
        if is_escaped(text, index) {
            continue;
        }
        match character {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    depth
}

fn contains_display_delimiter(text: &str) -> bool {
    display_delimiter_count(text) > 0
}

fn display_delimiter_count(text: &str) -> usize {
    let mut count = 0;
    let mut index = 0;
    while index + 1 < text.len() {
        if text[index..].starts_with("$$") && !is_escaped(text, index) {
            count += 1;
            index += 2;
        } else {
            index += text[index..]
                .chars()
                .next()
                .expect("index must be on a character boundary")
                .len_utf8();
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_all_explicit_model_delimiters() {
        let normalized =
            normalize_model_math("Markdown $x$ and $$y$$; TeX \\(a+b\\) and \\[c+d\\].");

        assert_eq!(normalized, "Markdown $x$ and $$y$$; TeX $a+b$ and $$c+d$$.");
    }

    #[test]
    fn converts_labelled_math_fences_but_preserves_code() {
        let normalized = normalize_model_math(
            r#"```latex
\frac{a}{b}
```
```rust
let value = "\(x\)";
```"#,
        );

        assert!(normalized.contains("$$\\frac{a}{b}$$"));
        assert!(normalized.contains(
            r#"```rust
let value = "\(x\)";
```"#
        ));
    }

    #[test]
    fn removes_redundant_delimiters_inside_math_fences() {
        let normalized = normalize_model_math("```latex\n\\[\n\\frac{a}{b}\n\\]\n```");

        assert_eq!(normalized, "$$\\frac{a}{b}$$");
    }

    #[test]
    fn protects_inline_code_and_unclosed_delimiters() {
        let normalized = normalize_model_math("Use `\\(literal\\)` and leave \\(unclosed alone.");

        assert_eq!(
            normalized,
            "Use `\\(literal\\)` and leave \\(unclosed alone."
        );
    }

    #[test]
    fn protects_multi_tick_code_spans_from_bare_math_detection() {
        let normalized = normalize_model_math("Use ``\\frac{a}{b}`` literally.");

        assert_eq!(normalized, "Use ``\\frac{a}{b}`` literally.");
    }

    #[test]
    fn preserves_bare_math_inside_explicit_display_delimiters() {
        let normalized = normalize_model_math("\\[\n\\frac{a}{b}\n\\]");

        assert_eq!(normalized, "$$\n\\frac{a}{b}\n$$");
    }

    #[test]
    fn canonicalizes_bare_multiline_environment() {
        let normalized = normalize_model_math(
            "Result:\n\\begin{aligned}\nx &= 1 \\\\\ny &= 2\n\\end{aligned}\nDone.",
        );

        assert!(normalized.contains("$$\\begin{aligned} x &= 1 \\\\ y &= 2 \\end{aligned}$$"));
        assert!(normalized.ends_with("Done."));
    }

    #[test]
    fn recognizes_equation_prefix_before_a_bare_command() {
        let normalized = normalize_model_math("f'(x) = \\frac{1}{x}.");

        assert_eq!(normalized, "$$f'(x) = \\frac{1}{x}.$$");
    }

    #[test]
    fn simplifies_only_presentation_latex_for_telegram() {
        let sanitized = sanitize_for_telegram(
            "\\boxed{\\displaystyle \\dfrac{dy}{dx}=f'\\!\\bigl(u\\bigr)\\,u'}",
        );

        assert_eq!(sanitized, "{ \\frac{dy}{dx}=f'(u) u'}");
    }

    #[test]
    fn preserves_semantic_commands_that_share_presentation_prefixes() {
        let latex = "\\bigcup_{i=1}^{n} A_i \\leftrightarrow B";

        assert_eq!(sanitize_for_telegram(latex), latex);
    }
}
