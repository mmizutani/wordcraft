//! Answers read as Markdown: what each block and span says, hostile text, and text that is still
//! arriving.

use std::time::{Duration, Instant};

use super::{Block, Formatted, MAX_BLOCKS, MAX_PARSED, MAX_SPANS, Span, format, opens};

fn words(f: &Formatted) -> String {
    let span = |s: &[Span]| s.iter().map(|s| s.text.as_str()).collect::<String>();
    let mut out: Vec<String> = f
        .blocks
        .iter()
        .map(|b| match b {
            Block::Para(s) | Block::Quote(s) | Block::Heading { spans: s, .. } | Block::Item { spans: s, .. } => span(s),
            Block::Code(t) => t.clone(),
            Block::Rule => String::new(),
        })
        .collect();
    out.push(f.rest.clone());
    out.join("\n")
}

#[test]
fn headings_lists_emphasis_code_and_quotes_read_as_themselves() {
    let f = format(
        "# Title\n\nSome **bold**, *italic*, `code` and ~~gone~~.\n\n- one\n  - deep\n- two\n\n3. three\n4. four\n\n> said\n\n```\nlet a;\nlet b;\n```\n\n---\n",
        false,
    );
    let [title, para, one, deep, two, three, four, quote, code, rule] = f.blocks.as_slice() else { panic!("{:#?}", f.blocks) };
    assert!(matches!(title, Block::Heading { level: 1, spans } if spans[0].text == "Title"), "{title:?}");
    let Block::Para(spans) = para else { panic!("{para:?}") };
    assert!(spans.iter().any(|s| s.bold && s.text == "bold"), "{spans:?}");
    assert!(spans.iter().any(|s| s.italic && s.text == "italic"), "{spans:?}");
    assert!(spans.iter().any(|s| s.code && s.text == "code"), "{spans:?}");
    assert!(spans.iter().any(|s| s.strike && s.text == "gone"), "{spans:?}");
    assert!(matches!(one, Block::Item { marker, level: 0, .. } if marker == "•"), "{one:?}");
    assert!(matches!(deep, Block::Item { level: 1, .. }), "{deep:?}");
    assert!(matches!(two, Block::Item { level: 0, .. }), "{two:?}");
    assert!(matches!(three, Block::Item { marker, .. } if marker == "3."), "{three:?}");
    assert!(matches!(four, Block::Item { marker, .. } if marker == "4."), "{four:?}");
    assert!(matches!(quote, Block::Quote(s) if s[0].text == "said"), "{quote:?}");
    assert_eq!(code, &Block::Code("let a;\nlet b;".into()));
    assert_eq!(rule, &Block::Rule);
}

#[test]
fn a_link_keeps_its_address_and_only_web_addresses_open() {
    let f = format("[here](https://example.com/a) and [run](file:///bin/sh)", false);
    let Block::Para(spans) = &f.blocks[0] else { panic!("{f:?}") };
    let linked: Vec<_> = spans.iter().filter_map(|s| s.link.as_deref().map(|l| (s.text.as_str(), l))).collect();
    assert_eq!(linked, [("here", "https://example.com/a"), ("run", "file:///bin/sh")]);
    assert!(opens("https://example.com/a") && opens("HTTP://example.com") && opens("http://é.com"));
    for address in ["file:///bin/sh", "javascript:alert(1)", "mailto:a@b.c", "wordcraft://x", "", "http:"] {
        assert!(!opens(address), "{address}");
    }
}

#[test]
fn a_picture_shows_its_description_and_nothing_is_loaded() {
    // A 1×1 PNG in the text: the parser makes it a picture, which an answer doesn't draw.
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
    let f = format(&format!("![a chart](data:image/png;base64,{png}) ![far](https://example.com/x.png)"), false);
    let shown = words(&f);
    assert!(shown.contains("a chart"), "{shown}");
    assert!(!shown.contains("iVBOR"), "{shown}");
}

#[test]
fn entities_cant_bring_back_what_hides_or_reorders_text() {
    let f = format("wollA&#x202E; &#xE0041;x", false);
    let shown = words(&f);
    assert!(!shown.contains('\u{202E}') && !shown.contains('\u{E0041}'), "{shown:?}");
    let f = format("[a](https://x.example/&#x202E;moc)", false);
    let Block::Para(spans) = &f.blocks[0] else { panic!("{f:?}") };
    assert!(!spans[0].link.as_deref().unwrap_or_default().contains('\u{202E}'), "{spans:?}");
}

/// Every piece of Markdown that has to be read at once to be hard. The budget is a debug build's
/// on a busy machine: a release build reads each in a small part of it (a frame is about 16 ms).
#[test]
fn junk_reads_within_a_frame_budget_and_holds_to_the_limits() {
    const SIZE: usize = 256 << 10;
    let repeat = |s: &str| s.repeat(SIZE / s.len());
    let cases = [
        repeat("*"),
        repeat("* "),
        repeat("**a"),
        repeat("_a "),
        repeat("`"),
        repeat("`a"),
        repeat("["),
        repeat("[a]("),
        repeat("!["),
        repeat(">"),
        repeat("> "),
        repeat("- "),
        repeat("  - a\n"),
        repeat("- a\n"),
        repeat("1. a\n"),
        repeat("# a\n"),
        repeat("|a"),
        repeat("|a|\n|-|\n"),
        repeat("```\n"),
        format!("```\n{}", repeat("a\n")),
        repeat("&#x202E;"),
        repeat("\\"),
        repeat("é"),
    ];
    for case in &cases {
        for streaming in [false, true] {
            let at = Instant::now();
            let f = format(case, streaming);
            let took = at.elapsed();
            let name: String = case.chars().take(12).collect();
            assert!(took < Duration::from_millis(1500), "{name:?} (streaming: {streaming}) took {took:?}");
            assert!(f.blocks.len() <= MAX_BLOCKS, "{name:?}: {} blocks", f.blocks.len());
        }
    }
}

#[test]
fn an_answer_past_the_limits_is_all_still_there_as_text() {
    let many: String = (0..2000).map(|n| format!("- item {n}\n")).collect();
    let f = format(&many, false);
    assert_eq!(f.blocks.len(), MAX_BLOCKS);
    assert!(f.rest.contains("item 1999"), "{}", f.rest.len());
    let long = format!("{}end of it", "word ".repeat(30_000));
    let f = format(&long, false);
    assert!(f.rest.ends_with("end of it"), "{}", f.rest.len());
}

#[test]
fn text_still_arriving_shows_no_marker_and_only_ever_grows() {
    let answer = "Some **bold words**, *italic ones*, `some code`, ~~gone~~ and [a link](https://example.com/page) end.";
    let mut before = String::new();
    for n in 0..=answer.len() {
        let shown = words(&format(&answer[..n], true));
        let shown = shown.trim_end();
        for marker in ['*', '`', '~', '[', ']'] {
            assert!(!shown.contains(marker), "{marker} in {shown:?} at {:?}", &answer[..n]);
        }
        assert!(shown.starts_with(before.trim_end()), "{shown:?} after {before:?}");
        before = shown.to_string();
    }
    assert_eq!(before, "Some bold words, italic ones, some code, gone and a link end.");
}

#[test]
fn a_finished_answer_is_read_as_it_is() {
    // Nothing is closed or held back once the turn has moved on.
    let f = format("a **b [c", false);
    assert_eq!(words(&f).trim_end(), "a **b [c");
}

#[test]
fn an_open_code_fence_stays_code_as_it_arrives() {
    let f = format("```\nlet **a", true);
    assert_eq!(f.blocks, [Block::Code("let **a".into())]);
}

#[test]
fn one_paragraph_holds_to_the_span_limit_too() {
    let f = format(&"*a* ".repeat(16_000), false);
    let spans: usize = f.blocks.iter().map(|b| if let Block::Para(s) = b { s.len() } else { 1 }).sum();
    assert!(spans <= MAX_SPANS, "{spans} spans");
    assert!(f.rest.contains('a'), "the words are still there");
}

#[test]
fn past_the_limit_nothing_comes_before_what_it_followed() {
    let items = "- item\n".repeat(MAX_BLOCKS - 1);
    let f = format(&format!("{items}\n```\nfirst\n```\n\nmiddle\n\n```\nlast\n```\n"), false);
    let at = |word: &str| words(&f).find(word).unwrap_or_else(|| panic!("{word} is gone"));
    assert!(at("first") < at("middle") && at("middle") < at("last"), "{:?}", f.rest);
    // A small block after one too big to draw goes after it too.
    let f = format(&format!("{}\n\nafter", "*a* ".repeat(16_000)), false);
    assert!(f.blocks.is_empty(), "{:?}", f.blocks.first());
    assert!(f.rest.trim_end().ends_with("after"));
}

#[test]
fn an_address_too_long_to_show_whole_is_not_a_link() {
    let f = format(&format!("[download](https://example.com/{})", "a".repeat(600)), false);
    let Block::Para(spans) = &f.blocks[0] else { panic!("{f:?}") };
    assert_eq!(spans[0].text, "download");
    assert_eq!(spans[0].link, None, "cut, it could go somewhere else");
}

#[test]
fn triple_emphasis_closes_as_it_arrives() {
    assert_eq!(words(&format("***x***", true)).trim_end(), "x");
    assert_eq!(words(&format("___x___", true)).trim_end(), "x");
}

#[test]
fn only_a_real_closing_fence_ends_a_code_block_as_it_arrives() {
    let f = format("```\n```not-a-close\nx *a", true);
    assert_eq!(f.blocks, [Block::Code("```not-a-close\nx *a".into())]);
}

#[test]
fn a_link_with_parentheses_waits_for_its_whole_address() {
    assert_eq!(words(&format("See [x](https://e.test/a(b)", true)).trim_end(), "See");
    let f = format("See [x](https://e.test/a(b)) now", true);
    assert_eq!(words(&f).trim_end(), "See x now");
}

#[test]
fn a_character_across_the_read_limit_leaves_the_rest_formatted() {
    let f = format(&format!("# H\n{}é", "a".repeat(MAX_PARSED - 5)), false);
    assert!(matches!(&f.blocks[0], Block::Heading { .. }), "{:?}", f.blocks.first());
}

#[test]
fn a_list_may_start_at_zero() {
    let f = format("0. zero\n1. one\n2. two", false);
    let markers: Vec<&str> = f.blocks.iter().filter_map(|b| if let Block::Item { marker, .. } = b { Some(marker.as_str()) } else { None }).collect();
    assert_eq!(markers, ["0.", "1.", "2."]);
}

#[test]
fn a_table_past_a_document_s_limits_keeps_all_its_words() {
    let wide = format!("| {} |\n", (0..100).map(|n| format!("c{n}")).collect::<Vec<_>>().join(" | "));
    let f = format(&format!("{wide}|{}\n", "-|".repeat(100)), false);
    assert!(words(&f).contains("c99"), "{:?}", f.blocks);
    // Nor do words past the columns its header has go.
    let f = format("| a | b |\n|---|---|\n| c | d | hidden |\n", false);
    assert!(words(&f).contains("hidden"), "{f:?}");
    // One that fits still reads as a table.
    let f = format("| a | b |\n|---|---|\n| c | d |\n", false);
    assert!(!f.blocks.is_empty() && f.rest.is_empty(), "{f:?}");
}

#[test]
fn prose_after_a_closed_code_block_is_read_on_its_own_as_it_arrives() {
    let f = format("```\na\n\nb\n```\nNext", true);
    assert_eq!(f.blocks, [Block::Code("a\n\nb".into()), Block::Para(vec![Span { text: "Next".into(), ..Span::default() }])]);
}

#[test]
fn a_bullet_list_ends_the_ordered_list_before_it() {
    let f = format("1. first\n\n- bullet\n\n1. again", false);
    let markers: Vec<&str> = f.blocks.iter().filter_map(|b| if let Block::Item { marker, .. } = b { Some(marker.as_str()) } else { None }).collect();
    assert_eq!(markers, ["1.", "•", "1."]);
}

#[test]
fn two_code_blocks_stay_two() {
    let f = format("```python\nprint(1)\n```\n\n```sh\necho done\n```\n", false);
    assert_eq!(f.blocks, [Block::Code("print(1)".into()), Block::Code("echo done".into())]);
    // With nothing between them, or arriving.
    let f = format("```\na\n```\n```\nb", true);
    assert_eq!(f.blocks, [Block::Code("a".into()), Block::Code("b".into())]);
    // One block's own blank line and lines stay one.
    let f = format("```\na\n\nb\n```\n", false);
    assert_eq!(f.blocks, [Block::Code("a\n\nb".into())]);
}
