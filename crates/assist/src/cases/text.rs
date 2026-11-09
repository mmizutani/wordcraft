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

/// The tokens of `text`, at most [`MAX_TOKENS`]: a run of letters and digits is one token, a run of
/// CJK characters becomes overlapping bigrams (a one-character run is itself), and a bigram of two
/// hiragana is dropped, since の, する, こと carry little and BM25's idf can't tell on a small pool.
pub fn tokens(text: &str) -> impl Iterator<Item = Token> + '_ {
    let mut chars = text.chars().map(fold).peekable();
    let mut pending: Vec<Token> = Vec::new();
    std::iter::from_fn(move || {
        loop {
            if let Some(t) = pending.pop() {
                return Some(t);
            }
            let c = chars.next()?;
            if is_cjk_word(c) {
                let mut run = vec![c];
                while let Some(&n) = chars.peek() {
                    if !is_cjk_word(n) {
                        break;
                    }
                    // More than MAX_TOKENS + 1 characters make more bigrams than are ever read.
                    if run.len() <= MAX_TOKENS {
                        run.push(n);
                    }
                    chars.next();
                }
                if run.len() == 1 {
                    return Some(Token(c.to_string()));
                }
                pending = run.windows(2).rev().filter(|w| !(is_hiragana(w[0]) && is_hiragana(w[1]))).map(|w| Token(w.iter().collect())).collect();
            } else if c.is_alphanumeric() {
                let mut run = String::new();
                run.push(c);
                while let Some(&n) = chars.peek() {
                    if is_cjk_word(n) || !n.is_alphanumeric() {
                        break;
                    }
                    run.push(n);
                    chars.next();
                }
                return Some(Token(run));
            }
        }
    })
    .take(MAX_TOKENS)
}

/// English and Japanese terms per topic. An ASCII term matches at the start of a word (indemn
/// finds indemnify and indemnity); a Japanese term matches anywhere.
pub const TERMS: &[(Topic, &[&str])] = &[
    (Topic::Liability, &["liabilit", "liable", "cap on damages", "consequential loss", "責任制限", "責任の上限", "損害賠償", "間接損害", "免責"]),
    (Topic::Indemnity, &["indemn", "hold harmless", "補償"]),
    (Topic::Ip, &["intellectual property", "copyright", "patent", "trade mark", "trademark", "知的財産", "著作権", "特許"]),
    (Topic::Confidentiality, &["confidential", "non-disclosure", "nondisclosure", "nda", "秘密保持", "秘密情報", "機密"]),
    (Topic::Termination, &["terminat", "expir", "notice period", "解除", "解約", "終了", "予告期間"]),
    (Topic::DataProtection, &["personal data", "data protection", "privacy", "gdpr", "sub-processor", "個人情報", "プライバシー", "個人データ"]),
    (Topic::GoverningLaw, &["governing law", "jurisdiction", "arbitration", "venue", "準拠法", "管轄", "仲裁"]),
    (Topic::Assignment, &["assign", "change of control", "譲渡", "承継"]),
    (Topic::Warranties, &["warrant", "representation", "保証", "表明"]),
    (Topic::Sla, &["service level", "sla", "uptime", "availability", "credit", "サービスレベル", "稼働率", "クレジット"]),
    (Topic::Payment, &["payment", "invoice", "fee", "price", "net 30", "net 45", "net 60", "支払", "料金", "報酬", "対価", "代金"]),
];

/// The topics `text` names, by [`TERMS`].
pub fn topics_in(text: &str) -> Topics {
    // Folded, with ASCII words separated by single spaces and a leading space, so an ASCII term
    // matches only at a word's start.
    let mut folded = String::with_capacity(text.len() + 1);
    folded.push(' ');
    for c in text.chars().take(MAX_TOKENS * 8).map(fold) {
        if c.is_ascii() && !c.is_ascii_alphanumeric() {
            if !folded.ends_with(' ') {
                folded.push(' ');
            }
        } else {
            folded.push(c);
        }
    }
    TERMS
        .iter()
        .filter(|(_, terms)| terms.iter().any(|t| if t.is_ascii() { folded.contains(&format!(" {t}")) } else { folded.contains(t) }))
        .map(|(topic, _)| *topic)
        .collect()
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
