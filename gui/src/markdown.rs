//! Markdown to a small, safe tree. The UI turns this tree into Yew nodes, so model text is
//! never inserted as HTML: raw HTML in the input is shown as plain text, images become their
//! alt text, and only http, https and mailto links keep their address. Pure and tested natively.
use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag};

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Text(String),
    Code(String),
    Em(Vec<Node>),
    Strong(Vec<Node>),
    Strike(Vec<Node>),
    /// `url` is `None` when the scheme is not allowed; then only the text is shown.
    Link { url: Option<String>, children: Vec<Node> },
    Break,
    Rule,
    Paragraph(Vec<Node>),
    Heading { level: u8, children: Vec<Node> },
    CodeBlock { lang: Option<String>, text: String },
    List { start: Option<u64>, items: Vec<Vec<Node>> },
    Task(bool),
    Quote(Vec<Node>),
    Table { aligns: Vec<Align>, head: Vec<Vec<Node>>, rows: Vec<Vec<Vec<Node>>> },
    // Internal scaffolding while building tables; never present in the result.
    Cell(Vec<Node>),
    Row(Vec<Node>),
    HeadRow(Vec<Node>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

enum Frame {
    Root,
    Em,
    Strong,
    Strike,
    Link(Option<String>),
    Image,
    Paragraph,
    Heading(u8),
    CodeBlock(Option<String>),
    List(Option<u64>),
    Item,
    Quote,
    Table(Vec<Align>),
    TableHead,
    TableRow,
    TableCell,
    /// Unknown containers: keep their content, drop the wrapper.
    Transparent,
}

pub fn is_safe_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
}

fn level(l: HeadingLevel) -> u8 {
    match l {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn align(a: &Alignment) -> Align {
    match a {
        Alignment::None => Align::None,
        Alignment::Left => Align::Left,
        Alignment::Center => Align::Center,
        Alignment::Right => Align::Right,
    }
}

/// Plain text of a node list (used for image alt text).
fn plain(nodes: &[Node]) -> String {
    nodes
        .iter()
        .map(|n| match n {
            Node::Text(t) | Node::Code(t) => t.clone(),
            Node::Em(c) | Node::Strong(c) | Node::Strike(c) | Node::Link { children: c, .. } => plain(c),
            Node::Break => " ".into(),
            _ => String::new(),
        })
        .collect()
}

pub fn parse(source: &str) -> Vec<Node> {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut stack: Vec<(Frame, Vec<Node>)> = vec![(Frame::Root, Vec::new())];

    fn push(stack: &mut [(Frame, Vec<Node>)], node: Node) {
        if let Some((_, children)) = stack.last_mut() {
            children.push(node);
        }
    }

    for event in Parser::new_ext(source, options) {
        match event {
            Event::Start(tag) => {
                let frame = match tag {
                    Tag::Emphasis => Frame::Em,
                    Tag::Strong => Frame::Strong,
                    Tag::Strikethrough => Frame::Strike,
                    Tag::Link { dest_url, .. } => Frame::Link(is_safe_url(&dest_url).then(|| dest_url.to_string())),
                    Tag::Image { .. } => Frame::Image,
                    Tag::Paragraph => Frame::Paragraph,
                    Tag::Heading { level: l, .. } => Frame::Heading(level(l)),
                    Tag::CodeBlock(CodeBlockKind::Fenced(lang)) => {
                        Frame::CodeBlock(Some(lang.split_whitespace().next().unwrap_or("").to_string()).filter(|l| !l.is_empty()))
                    }
                    Tag::CodeBlock(CodeBlockKind::Indented) => Frame::CodeBlock(None),
                    Tag::List(start) => Frame::List(start),
                    Tag::Item => Frame::Item,
                    Tag::BlockQuote(_) => Frame::Quote,
                    Tag::Table(aligns) => Frame::Table(aligns.iter().map(align).collect()),
                    Tag::TableHead => Frame::TableHead,
                    Tag::TableRow => Frame::TableRow,
                    Tag::TableCell => Frame::TableCell,
                    _ => Frame::Transparent,
                };
                stack.push((frame, Vec::new()));
            }
            Event::End(_) => {
                if stack.len() <= 1 {
                    continue;
                }
                let Some((frame, mut children)) = stack.pop() else { continue };
                let node = match frame {
                    Frame::Root => continue,
                    Frame::Em => Node::Em(children),
                    Frame::Strong => Node::Strong(children),
                    Frame::Strike => Node::Strike(children),
                    Frame::Link(url) => Node::Link { url, children },
                    Frame::Image => Node::Text(format!("[image: {}]", plain(&children))),
                    Frame::Paragraph => Node::Paragraph(children),
                    Frame::Heading(l) => Node::Heading { level: l, children },
                    Frame::CodeBlock(lang) => {
                        let mut text = plain(&children);
                        if text.ends_with('\n') {
                            text.pop();
                        }
                        Node::CodeBlock { lang, text }
                    }
                    Frame::List(start) => Node::List {
                        start,
                        items: children
                            .into_iter()
                            .map(|n| match n {
                                Node::Row(items) => items,
                                other => vec![other],
                            })
                            .collect(),
                    },
                    Frame::Item => Node::Row(children),
                    Frame::Quote => Node::Quote(children),
                    Frame::TableCell => Node::Cell(children),
                    Frame::TableRow => Node::Row(children),
                    Frame::TableHead => Node::HeadRow(children),
                    Frame::Table(aligns) => {
                        let mut head = Vec::new();
                        let mut rows = Vec::new();
                        for child in children.drain(..) {
                            match child {
                                Node::HeadRow(cells) => head = cells.into_iter().map(cell_children).collect(),
                                Node::Row(cells) => rows.push(cells.into_iter().map(cell_children).collect()),
                                _ => {}
                            }
                        }
                        Node::Table { aligns, head, rows }
                    }
                    Frame::Transparent => {
                        for c in children {
                            push(&mut stack, c);
                        }
                        continue;
                    }
                };
                push(&mut stack, node);
            }
            Event::Text(t) => push(&mut stack, Node::Text(t.to_string())),
            Event::Code(t) => push(&mut stack, Node::Code(t.to_string())),
            Event::Html(t) | Event::InlineHtml(t) => push(&mut stack, Node::Text(t.to_string())),
            Event::SoftBreak | Event::HardBreak => push(&mut stack, Node::Break),
            Event::Rule => push(&mut stack, Node::Rule),
            Event::TaskListMarker(done) => push(&mut stack, Node::Task(done)),
            _ => {}
        }
    }
    stack.pop().map(|(_, c)| c).unwrap_or_default()
}

fn cell_children(node: Node) -> Vec<Node> {
    match node {
        Node::Cell(children) => children,
        other => vec![other],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Node {
        Node::Text(s.into())
    }

    #[test]
    fn paragraphs_and_inline_styles() {
        let n = parse("Hello **bold** and *italic* and `code` and ~~gone~~.");
        assert_eq!(
            n,
            vec![Node::Paragraph(vec![
                text("Hello "),
                Node::Strong(vec![text("bold")]),
                text(" and "),
                Node::Em(vec![text("italic")]),
                text(" and "),
                Node::Code("code".into()),
                text(" and "),
                Node::Strike(vec![text("gone")]),
                text("."),
            ])]
        );
    }

    #[test]
    fn headings_rules_and_quotes() {
        let n = parse("# Title\n\n### Sub\n\n---\n\n> quoted");
        assert_eq!(n[0], Node::Heading { level: 1, children: vec![text("Title")] });
        assert_eq!(n[1], Node::Heading { level: 3, children: vec![text("Sub")] });
        assert_eq!(n[2], Node::Rule);
        assert_eq!(n[3], Node::Quote(vec![Node::Paragraph(vec![text("quoted")])]));
    }

    #[test]
    fn fenced_code_keeps_language_and_whitespace_and_drops_the_trailing_newline() {
        let n = parse("```rust\nfn main() {\n    println!(\"<b>\");\n}\n```");
        assert_eq!(n, vec![Node::CodeBlock { lang: Some("rust".into()), text: "fn main() {\n    println!(\"<b>\");\n}".into() }]);
        let plain_fence = parse("```\nx\n```");
        assert_eq!(plain_fence, vec![Node::CodeBlock { lang: None, text: "x".into() }]);
    }

    #[test]
    fn lists_ordered_unordered_nested_and_tasks() {
        let n = parse("- one\n- two\n  - inner\n\n3. a\n4. b");
        let Node::List { start: None, items } = &n[0] else { panic!("{n:?}") };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], vec![text("one")]);
        assert!(items[1].iter().any(|c| matches!(c, Node::List { items, .. } if items.len() == 1)));
        let Node::List { start: Some(3), items } = &n[1] else { panic!("{n:?}") };
        assert_eq!(items.len(), 2);

        let tasks = parse("- [x] done\n- [ ] todo");
        let Node::List { items, .. } = &tasks[0] else { panic!() };
        assert_eq!(items[0][0], Node::Task(true));
        assert_eq!(items[1][0], Node::Task(false));
    }

    #[test]
    fn tables_have_head_rows_and_alignment() {
        let n = parse("| a | b |\n|:--|--:|\n| 1 | 2 |\n| 3 | 4 |");
        let Node::Table { aligns, head, rows } = &n[0] else { panic!("{n:?}") };
        assert_eq!(aligns, &vec![Align::Left, Align::Right]);
        assert_eq!(head, &vec![vec![text("a")], vec![text("b")]]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1], vec![vec![text("3")], vec![text("4")]]);
    }

    #[test]
    fn only_safe_link_schemes_keep_their_address() {
        let n = parse("[ok](https://example.com) [mail](mailto:a@b.c) [bad](javascript:alert(1)) [file](file:///etc/passwd)");
        let Node::Paragraph(items) = &n[0] else { panic!() };
        let urls: Vec<Option<String>> = items
            .iter()
            .filter_map(|i| if let Node::Link { url, .. } = i { Some(url.clone()) } else { None })
            .collect();
        assert_eq!(urls, vec![Some("https://example.com".into()), Some("mailto:a@b.c".into()), None, None]);
        assert!(is_safe_url("  HTTPS://x.y"));
        assert!(!is_safe_url("data:text/html,<script>"));
    }

    #[test]
    fn raw_html_and_images_are_neutralised() {
        let n = parse("<script>alert(1)</script>\n\n<b>hi</b> ![a cat](http://x/cat.png)");
        let dump = format!("{n:?}");
        // The HTML survives only as text nodes (which the UI escapes), never as markup.
        assert!(dump.contains("Text(\"<script>"));
        assert!(!dump.contains("Image"));
        assert!(dump.contains("[image: a cat]"));
    }

    #[test]
    fn line_breaks_are_kept_and_plain_text_passes_through() {
        let n = parse("line one\nline two");
        assert_eq!(n, vec![Node::Paragraph(vec![text("line one"), Node::Break, text("line two")])]);
        assert_eq!(parse(""), Vec::<Node>::new());
        assert_eq!(parse("just words"), vec![Node::Paragraph(vec![text("just words")])]);
    }

    #[test]
    fn unbalanced_or_odd_input_does_not_panic() {
        for src in ["**unclosed", "```\nnever closed", "| a |\n|--|", "[](", "- ", "> > > deep", "\u{0}\u{1}", "# "] {
            let _ = parse(src);
        }
    }

    #[test]
    fn the_shared_sample_covers_every_block_type_and_stays_safe() {
        let nodes = parse(lightning_protocol::fixtures::MARKDOWN_SAMPLE);
        let has = |f: &dyn Fn(&Node) -> bool| nodes.iter().any(f);
        assert!(has(&|n| matches!(n, Node::Heading { level: 1, .. })));
        assert!(has(&|n| matches!(n, Node::Heading { level: 2, .. })));
        assert!(has(&|n| matches!(n, Node::List { start: None, .. })));
        assert!(has(&|n| matches!(n, Node::List { start: Some(1), .. })));
        assert!(has(&|n| matches!(n, Node::CodeBlock { lang: Some(l), .. } if l == "rust")));
        assert!(has(&|n| matches!(n, Node::Table { .. })));
        assert!(has(&|n| matches!(n, Node::Quote(_))));
        assert!(has(&|n| matches!(n, Node::Rule)));
        let dump = format!("{nodes:?}");
        assert!(!dump.contains("javascript:"), "unsafe link addresses must be dropped");
    }
}
