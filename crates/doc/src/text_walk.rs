use super::block::{Block, Blocks};
use super::parts::PartKind;
use super::position::Path;
use super::{InlineObject, Numbering, numbering, para, section, styles};

/// Words in `s`, as Word counts them: tokens separated by whitespace that contain at least one
/// alphanumeric char, plus each Han ideograph and kana as a word of its own (Chinese and Japanese
/// are written without spaces, #4547; Korean is spaced, so Hangul counts by tokens).
pub fn count_words(s: &str) -> usize {
    count_word_chars(s.chars())
}

fn count_word_chars(chars: impl Iterator<Item = char>) -> usize {
    let (mut total, mut in_word) = (0usize, false);
    for c in chars {
        if is_cjk_word(c) {
            total = total.saturating_add(1);
            in_word = false;
        } else if c.is_whitespace() || c == para::OBJ {
            in_word = false;
        } else if !in_word && c.is_alphanumeric() && !matches!(c, '\u{FF9E}' | '\u{FF9F}') {
            // Half-width voiced sound marks belong to the preceding kana.
            total = total.saturating_add(1);
            in_word = true;
        }
    }
    total
}

/// Words in a generated list label: whitespace-separated tokens holding a letter or digit, so a
/// CJK label such as 第十二章 stays one word (how Word counts those is unverified).
fn count_label_words(s: &str) -> usize {
    s.split(char::is_whitespace).filter(|w| w.chars().any(char::is_alphanumeric)).count()
}

/// A Han ideograph or kana letter (not CJK punctuation), which Word counts as one word.
pub fn is_cjk_word(c: char) -> bool {
    matches!(c,
        '\u{3005}'..='\u{3007}' // 々 〆 〇
        | '\u{3031}'..='\u{3035}' | '\u{303B}' // vertical kana repeat marks, 〻
        | '\u{3040}'..='\u{30FF}' // hiragana, katakana
        | '\u{31F0}'..='\u{31FF}' // katakana phonetic extensions
        | '\u{3400}'..='\u{4DBF}' // extension A
        | '\u{4E00}'..='\u{9FFF}' // unified ideographs
        | '\u{F900}'..='\u{FAFF}' // compatibility ideographs
        | '\u{FF66}'..='\u{FF9D}' // half-width katakana
        | '\u{1B000}'..='\u{1B16F}' // kana supplement and extended
        | '\u{20000}'..='\u{3FFFF}' // extensions B and later
    ) && c.is_alphanumeric()
}

/// Characters in `s`, as the Word Count dialog counts them ("Characters (with spaces)"):
/// paragraph breaks aren't characters (#2797).
pub fn count_chars(s: &str) -> usize {
    s.chars().filter(|c| *c != '\n' && *c != '\r').count()
}

/// Characters in `s` ignoring every whitespace character, as the Word Count dialog counts them
/// ("Characters (no spaces)"; #2797).
pub fn count_chars_no_spaces(s: &str) -> usize {
    s.chars().filter(|c| !c.is_whitespace()).count()
}

/// How deep in nested tables [`Document::para_paths`] goes: cells below this are skipped.
pub(crate) const PARA_WALK_DEPTH: usize = 16;

/// The note and text box stories an object shows, for `Document::reachable`.
pub(crate) fn notes_and_boxes(o: &InlineObject) -> Vec<(u32, &'static [PartKind])> {
    match o {
        InlineObject::NoteRef { id, .. } => vec![(*id, &[PartKind::Footnote, PartKind::Endnote][..])],
        o => o.text_boxes().into_iter().map(|id| (id, &[PartKind::TextBox][..])).collect(),
    }
}

pub(crate) fn walk(bl: &Blocks, prefix: &mut Vec<u32>, out: &mut Vec<Path>, depth: usize) {
    if depth > PARA_WALK_DEPTH {
        return;
    }
    for (i, b) in bl.iter().enumerate() {
        crate::work::steps(1);
        prefix.push(i as u32);
        match &**b {
            Block::Para(_) => out.push(Path(prefix.clone())),
            Block::Table(t) => {
                for (r, row) in t.rows.iter().enumerate() {
                    for (c, cell) in row.cells.iter().enumerate() {
                        prefix.push(r as u32);
                        prefix.push(c as u32);
                        walk(&cell.blocks, prefix, out, depth + 1);
                        prefix.pop();
                        prefix.pop();
                    }
                }
            }
        }
        prefix.pop();
    }
}

/// [`walk`] for the paragraphs from path `a` to `b` inclusive, skipping the blocks outside.
pub(crate) fn walk_between(bl: &Blocks, prefix: &mut Vec<u32>, out: &mut Vec<Path>, depth: usize, (a, b): (&[u32], &[u32])) {
    use std::cmp::Ordering::{Greater, Less};
    if depth > PARA_WALK_DEPTH {
        return;
    }
    // Where the paths starting with `q` lie against `x`: all before, all after, or either.
    let side = |q: &[u32], x: &[u32]| {
        let m = q.len().min(x.len());
        q[..m].cmp(&x[..m])
    };
    for (i, blk) in bl.iter().enumerate() {
        prefix.push(i as u32);
        if side(prefix, b) == Greater {
            prefix.pop();
            break;
        }
        if side(prefix, a) != Less {
            match &**blk {
                Block::Para(_) => {
                    if prefix.as_slice() >= a && prefix.as_slice() <= b {
                        out.push(Path(prefix.clone()));
                    }
                }
                Block::Table(t) => {
                    for (r, row) in t.rows.iter().enumerate() {
                        for (c, cell) in row.cells.iter().enumerate() {
                            prefix.push(r as u32);
                            prefix.push(c as u32);
                            if side(prefix, a) != Less && side(prefix, b) != Greater {
                                walk_between(&cell.blocks, prefix, out, depth + 1, (a, b));
                            }
                            prefix.pop();
                            prefix.pop();
                        }
                    }
                }
            }
        }
        prefix.pop();
    }
}

/// Which text of a paragraph [`text_of`] collects.
#[derive(Clone, Copy)]
pub(crate) enum TextMode {
    /// Every run, tracked deletions included ([`Paragraph::plain_text`]).
    Plain,
    /// As shown on screen ([`Paragraph::shown_text`]).
    Shown,
    /// Without tracked deletions ([`Paragraph::final_text`]).
    Final,
}

pub(crate) fn text_of(bl: &Blocks, out: &mut String, depth: usize, mode: TextMode) {
    if depth > 16 {
        return;
    }
    for b in bl {
        match &**b {
            Block::Para(p) => {
                out.push_str(&match mode {
                    TextMode::Plain => p.plain_text(),
                    TextMode::Shown => p.shown_text(),
                    TextMode::Final => p.final_text(),
                });
                out.push('\n');
            }
            Block::Table(t) => {
                for row in &t.rows {
                    let mut cells = Vec::new();
                    for c in &row.cells {
                        let mut s = String::new();
                        text_of(&c.blocks, &mut s, depth + 1, mode);
                        cells.push(s.trim_end_matches('\n').replace('\n', " "));
                    }
                    out.push_str(&cells.join("\t"));
                    out.push('\n');
                }
            }
        }
    }
}

pub(crate) fn story_word_count_of(
    bl: &Blocks,
    numbering: &Numbering,
    styles: &styles::StyleSheet,
    counters: &mut numbering::Counters,
    depth: usize,
) -> usize {
    if depth > 16 {
        return 0;
    }
    let mut total = 0usize;
    for b in bl {
        match &**b {
            Block::Para(p) => {
                let n = p.props.numbering.or_else(|| styles.resolve_para(&p.props).numbering).filter(|n| n.num != 0);
                if let Some(n) = n
                    && let Some((label, def)) = counters.next_label(numbering, n.num, n.level)
                    && !matches!(def.format, section::NumFormat::Bullet | section::NumFormat::None)
                {
                    total = total.saturating_add(count_label_words(&label));
                }
                // Keep token state across inline boundaries without concatenating their text.
                let chars = p.text.split(para::OBJ).enumerate().flat_map(|(i, text)| {
                    let object = i.checked_sub(1).and_then(|k| p.objects.get(k)).map(InlineObject::plain_text).unwrap_or_default();
                    object.chars().chain(text.chars())
                });
                total = total.saturating_add(count_word_chars(chars));
            }
            Block::Table(t) => {
                for row in &t.rows {
                    for c in &row.cells {
                        total = total.saturating_add(story_word_count_of(&c.blocks, numbering, styles, counters, depth + 1));
                    }
                }
            }
        }
    }
    total
}

#[cfg(test)]
mod streaming_word_tests {
    use super::*;
    use crate::{CharProps, Document, ListKind, Paragraph, Run, StoryRef, para_block, props::NumRef};
    use std::sync::Arc;

    fn reference_words(text: &str) -> usize {
        let tokens = text
            .split(|c: char| c.is_whitespace() || c == para::OBJ || is_cjk_word(c))
            .filter(|word| word.chars().any(|c| c.is_alphanumeric() && !matches!(c, '\u{FF9E}' | '\u{FF9F}')))
            .count();
        tokens + text.chars().filter(|c| is_cjk_word(*c)).count()
    }

    fn field(result: &str) -> InlineObject {
        InlineObject::Field { instr: String::new(), result: result.into(), locked: false, dirty: false, code: false, form: String::new() }
    }

    fn paragraph(text: &str, objects: Vec<InlineObject>) -> Paragraph {
        let mut p = Paragraph::new();
        p.text = text.into();
        p.runs = vec![Run { len: text.len(), props: CharProps::default() }];
        p.objects = objects;
        p
    }

    #[test]
    fn streaming_words_match_reference_for_unicode_token_boundaries() {
        let chars = ['a', '1', '-', ' ', '\t', '\u{a0}', para::OBJ, '日', 'ｶ', '\u{FF9E}', '\u{FF9F}', '안', '\u{301}', 'é', '\u{20000}'];
        for a in chars {
            for b in chars {
                for c in chars {
                    let text = format!("{a}{b}{c}");
                    assert_eq!(count_words(&text), reference_words(&text), "{text:?}");
                }
            }
        }
        assert_eq!(count_word_chars("pre".chars().chain("fix".chars()).chain("tail".chars())), 1);
        assert_eq!(count_words("ｶﾞｷﾟ ﾞ ﾟ"), 2);
    }

    #[test]
    fn story_words_keep_tokens_across_inline_fields_and_include_tracked_deletions() {
        let p = paragraph("pre\u{fffc}tail", vec![field("fix")]);
        let mut doc = Document::new();
        doc.body = vec![para_block(p)];
        assert_eq!(doc.word_count(), 1, "a field boundary does not split a word");
        let p = doc.para_mut(StoryRef::Body, &Path::top(0)).unwrap();
        p.runs[0].props.del = Some(1);
        assert_eq!(p.final_text(), "");
        assert_eq!(doc.word_count(), 1, "the existing status count includes tracked deletions");

        for result in ["", "-", "\t", "日ｶﾞ", "a\u{fffc}b", " two ", "é\u{301}"] {
            let p = paragraph("a\u{fffc}z \u{fffc} suffix", vec![field(result), field("inner")]);
            let expected = reference_words(&p.plain_text());
            doc.body = vec![para_block(p)];
            assert_eq!(doc.word_count(), expected, "field result {result:?}");
        }
        let p = paragraph("left\u{fffc}right", Vec::new());
        doc.body = vec![para_block(p)];
        assert_eq!(doc.word_count(), 1, "a missing object contributes no text");
    }

    #[test]
    fn story_words_keep_numbered_labels_separate_from_streamed_text() {
        let mut doc = Document::new();
        let numbered = Arc::make_mut(&mut doc.numbering).add_list(ListKind::Numbered);
        let bullets = Arc::make_mut(&mut doc.numbering).add_list(ListKind::Bullet);
        let mut table = crate::Table::new(1, 2, 400.0);
        let mut expected = 0usize;
        for (i, cell) in table.rows[0].cells.iter_mut().enumerate() {
            let mut p = paragraph("pre\u{fffc}tail 日本", vec![field("fix")]);
            p.props.numbering = Some(NumRef { num: if i == 0 { numbered } else { bullets }, level: 0 });
            expected += reference_words(&p.plain_text()) + usize::from(i == 0);
            cell.blocks = vec![para_block(p)];
        }
        doc.body = vec![Arc::new(Block::Table(table))];
        assert_eq!(doc.word_count(), expected);
    }
}
