//! How English and Japanese text is searched: a topic table that crosses languages, word tokens
//! and CJK character pairs, and a small BM25 built per query. No dictionary and no index.

use std::collections::HashMap;

use wordcraft_doc::is_cjk_word;

use super::record::{Topic, Topics};

/// Most tokens read from one text, whatever its length. Hostile input is bounded here.
pub const MAX_TOKENS: usize = 4096;

/// One searchable unit: a lowercased letter-and-digit run, or a CJK bigram.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Token(String);

impl Token {
    #[cfg(test)]
    pub fn for_test(&self) -> String {
        self.0.clone()
    }
}

/// Full-width ASCII (U+FF01..=U+FF5E) folded to ASCII, lowercased. Japanese contracts write 第１条.
/// ponytail: half-width katakana (U+FF66..=U+FF9D) stays unfolded, so ｻｰﾋﾞｽ and サービス don't
/// match; the upgrade is a 63-entry table to full-width, not `unicode-normalization`, whose
/// licence (Unicode-3.0) isn't on the allowlist.
fn fold(c: char) -> char {
    match c {
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c).to_ascii_lowercase(),
        _ => c.to_lowercase().next().unwrap_or(c),
    }
}

fn is_hiragana(c: char) -> bool {
    matches!(c, '\u{3040}'..='\u{309F}')
}

/// Most characters kept of one word: a longer run is still one token, cut short.
const MAX_WORD: usize = 64;

/// Most characters of a text [`topics_in`] reads.
const MAX_TOPIC_TEXT: usize = MAX_TOKENS * 8;

/// The tokens of `text`, at most [`MAX_TOKENS`], in one pass that holds at most one word: a run of
/// letters and digits is one token of at most [`MAX_WORD`] characters, a run of CJK characters
/// becomes overlapping bigrams (a one-character run is itself), and hiragana alone, as a pair or a
/// single character, is dropped, since の, する, こと carry little and BM25's idf can't tell on a
/// small pool.
pub fn tokens(text: &str) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::new();
    let mut word = String::new();
    let mut word_len = 0usize;
    // The last CJK character of the run being read, and how long the run is so far.
    let mut prev: Option<char> = None;
    let mut run = 0usize;
    for c in text.chars().map(fold) {
        if out.len() >= MAX_TOKENS {
            break;
        }
        if is_cjk_word(c) {
            end_word(&mut out, &mut word, &mut word_len);
            if let Some(p) = prev.filter(|p| !(is_hiragana(*p) && is_hiragana(c))) {
                out.push(Token([p, c].iter().collect()));
            }
            prev = Some(c);
            run = run.saturating_add(1);
            continue;
        }
        end_run(&mut out, &mut prev, &mut run);
        if c.is_alphanumeric() {
            if word_len < MAX_WORD {
                word.push(c);
                word_len += 1;
            }
        } else {
            end_word(&mut out, &mut word, &mut word_len);
        }
    }
    end_word(&mut out, &mut word, &mut word_len);
    end_run(&mut out, &mut prev, &mut run);
    out.truncate(MAX_TOKENS);
    out
}

fn end_word(out: &mut Vec<Token>, word: &mut String, len: &mut usize) {
    if !word.is_empty() {
        out.push(Token(std::mem::take(word)));
    }
    *len = 0;
}

fn end_run(out: &mut Vec<Token>, prev: &mut Option<char>, run: &mut usize) {
    if let Some(p) = prev.take().filter(|p| *run == 1 && !is_hiragana(*p)) {
        out.push(Token(p.to_string()));
    }
    *run = 0;
}

/// English and Japanese terms per topic. An ASCII term ending in `*` is a stem and matches at the
/// start of a word (`indemn*` finds indemnify and indemnity); any other ASCII term matches whole
/// words only, so `fee` doesn't find "feedback". A Japanese term matches anywhere. Hyphens read as
/// spaces, so the table writes "non disclosure" for "non-disclosure".
pub const TERMS: &[(Topic, &[&str])] = &[
    (Topic::Liability, &["liabilit*", "liable", "cap on damages", "consequential loss*", "責任制限", "責任の上限", "損害賠償", "間接損害", "免責"]),
    (Topic::Indemnity, &["indemn*", "hold harmless", "補償"]),
    (Topic::Ip, &["intellectual property", "copyright*", "patent*", "trade mark*", "trademark*", "知的財産", "著作権", "特許"]),
    (Topic::Confidentiality, &["confidential*", "non disclosure", "nondisclosure", "nda", "ndas", "秘密保持", "秘密情報", "機密"]),
    (Topic::Termination, &["terminat*", "expir*", "notice period*", "解除", "解約", "終了", "予告期間"]),
    (Topic::DataProtection, &["personal data", "data protection", "privacy", "gdpr", "sub processor*", "個人情報", "プライバシー", "個人データ"]),
    (Topic::GoverningLaw, &["governing law", "jurisdiction*", "arbitrat*", "venue", "準拠法", "管轄", "仲裁"]),
    (Topic::Assignment, &["assign*", "change of control", "譲渡", "承継"]),
    (Topic::Warranties, &["warrant*", "representation*", "表明保証", "品質保証", "契約不適合", "瑕疵担保"]),
    (Topic::Sla, &["service level*", "sla", "slas", "uptime", "availability", "service credit*", "サービスレベル", "稼働率", "クレジット"]),
    (
        Topic::Payment,
        &["payment*", "invoice*", "fee", "fees", "price", "prices", "pricing", "net 30", "net 45", "net 60", "支払", "料金", "報酬", "対価", "代金"],
    ),
];

/// The topics `text` names, by [`TERMS`].
pub fn topics_in(text: &str) -> Topics {
    // ASCII words separated by single spaces, with a space at each end and wherever ASCII meets
    // other text, so 本ＮＤＡの reads as " 本 nda の " and a term is found at a word's edges.
    let mut folded = String::with_capacity(text.len().min(MAX_TOPIC_TEXT * 4).saturating_add(2));
    folded.push(' ');
    let mut last_ascii = false;
    for c in text.chars().take(MAX_TOPIC_TEXT).map(fold) {
        let ascii = c.is_ascii_alphanumeric();
        if c.is_ascii() && !ascii {
            if !folded.ends_with(' ') {
                folded.push(' ');
            }
            continue;
        }
        if ascii != last_ascii && !folded.ends_with(' ') {
            folded.push(' ');
        }
        folded.push(c);
        last_ascii = ascii;
    }
    folded.push(' ');
    TERMS.iter().filter(|(_, terms)| terms.iter().any(|t| names(&folded, t))).map(|(topic, _)| *topic).collect()
}

fn names(folded: &str, term: &str) -> bool {
    if !term.is_ascii() {
        return folded.contains(term);
    }
    match term.strip_suffix('*') {
        Some(stem) => folded.contains(&format!(" {stem}")),
        None => folded.contains(&format!(" {term} ")),
    }
}

/// Word overlap scored over one pool, built per query, so nothing is stored and a delete has
/// nothing to update.
pub struct Bm25 {
    df: HashMap<Token, u32>,
    docs: u32,
    avg_len: f32,
}

const K1: f32 = 1.2;
const B: f32 = 0.75;

impl Bm25 {
    pub fn over<'a>(docs: impl Iterator<Item = &'a [Token]>) -> Bm25 {
        let mut df: HashMap<Token, u32> = HashMap::new();
        let (mut n, mut total_len) = (0u32, 0usize);
        for doc in docs {
            n = n.saturating_add(1);
            total_len = total_len.saturating_add(doc.len());
            let mut seen: Vec<&Token> = doc.iter().collect();
            seen.sort_by(|a, b| a.0.cmp(&b.0));
            seen.dedup();
            for t in seen {
                *df.entry(t.clone()).or_default() += 1;
            }
        }
        Bm25 { df, docs: n, avg_len: if n == 0 { 1.0 } else { total_len as f32 / n as f32 } }
    }

    /// The BM25 score of `doc` for `query`; zero when they share no token.
    pub fn score(&self, query: &[Token], doc: &[Token]) -> f32 {
        if query.is_empty() || doc.is_empty() {
            return 0.0;
        }
        let len_norm = 1.0 - B + B * doc.len() as f32 / self.avg_len.max(1.0);
        let mut asked: Vec<&Token> = query.iter().collect();
        asked.sort_by(|a, b| a.0.cmp(&b.0));
        asked.dedup();
        asked
            .into_iter()
            .map(|t| {
                let tf = doc.iter().filter(|d| *d == t).count() as f32;
                if tf == 0.0 {
                    return 0.0;
                }
                let df = self.df.get(t).copied().unwrap_or(0) as f32;
                let idf = (1.0 + (self.docs as f32 - df + 0.5) / (df + 0.5)).ln();
                idf * tf * (K1 + 1.0) / (tf + K1 * len_norm)
            })
            .sum()
    }
}
