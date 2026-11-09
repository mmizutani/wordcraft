//! The ranker on a synthetic corpus: twelve cases, sixty notes, English and Japanese, no real
//! client text. It pins what typed facts and the power balance change, not the weights.

use super::recall::{Candidate, Place, Query, Ranked, rank, rank_by_text};
use super::record::{
    Applies, Balance, Body, Case, ContractKind, CounterpartyKind, Day, Facts, Kind, Law, Note, OurSide, Paper, Quoted, Side, Stance, Standing, Topic,
    Topics,
};
use super::text::{TERMS, tokens, topics_in};

fn day(s: &str) -> Day {
    Day::parse(s).unwrap()
}

const TODAY: &str = "2026-11-10";

fn q(text: &str) -> Quoted {
    Quoted::parse(text)
}

fn confirmed(on: &str) -> Standing {
    Standing::Confirmed { on: day(on) }
}

fn note(on: &str, body: Body) -> Note {
    Note { at: day(on), body, how: None, standing: confirmed(on), stated_topics: Topics::default() }
}

fn pitfall(on: &str, trap: &str) -> Note {
    note(on, Body::Pitfall { trap: q(trap), guard: None })
}

fn pattern(on: &str, rule: &str) -> Note {
    note(on, Body::Pattern { rule: q(rule), traps: None })
}

fn lesson(on: &str, rule: &str) -> Note {
    note(on, Body::Lesson { rule: q(rule), traps: None })
}

fn leverage(on: &str, what: &str) -> Note {
    note(on, Body::Leverage { what: q(what), favors: Side::Us, until: None })
}

fn position(on: &str, topic: Topic, terms: &str) -> Note {
    note(on, Body::Position { topic, moved: Side::Them, terms: q(terms), traded_for: None, landed: true })
}

fn law(s: &str) -> Option<Law> {
    Some(Law::parse(s).unwrap())
}

fn facts(kind: ContractKind, side: OurSide, paper: Paper, theirs: CounterpartyKind, law_: &str) -> Facts {
    Facts { kind: Some(kind), side: Some(side), paper: Some(paper), theirs: Some(theirs), law: law(law_) }
}

fn case(title: &str, facts: Facts, balance: &[(&str, Stance)], notes: Vec<Note>) -> Case {
    Case {
        title: q(title),
        facts,
        balance: balance.iter().map(|(on, stance)| Balance { on: day(on), stance: *stance, standing: confirmed(on) }).collect(),
        notes,
    }
}

use ContractKind::*;
use CounterpartyKind::*;
use OurSide::*;
use Paper::*;
use Stance::*;

/// Twelve cases in one wall. Case 0 is the one the queries are asked from.
fn corpus() -> Vec<Case> {
    vec![
        case(
            "Current: SaaS subscription, customer side",
            facts(Saas, Customer, Theirs, Enterprise, "JP"),
            &[("2026-01-10", Even), ("2026-10-01", Weaker)],
            vec![
                pitfall("2026-02-01", "Their order form overrides the MSA by a precedence clause in the footer."),
                pattern("2026-03-01", "They open with a 12-month auto-renewal and a 90-day notice window."),
                position("2026-04-01", Topic::Payment, "Payment terms moved from net 30 to net 45."),
                lesson("2026-05-01", "Ask for the data export format before the price."),
                leverage("2026-09-01", "Their fiscal year ends in December and the sales team needs the signature."),
            ],
        ),
        case(
            "Old SaaS renewal, same vendor type",
            facts(Saas, Customer, Theirs, Enterprise, "JP"),
            &[("2024-01-10", Stronger), ("2024-06-01", Weaker)],
            vec![
                position(
                    "2024-02-10",
                    Topic::Liability,
                    "Liability cap raised to twenty-four months of fees, with the cap excluding data breach, confidentiality and indemnity claims, and credits counted outside the cap.",
                ),
                position("2024-07-10", Topic::Liability, "Liability cap raised to twelve months of fees."),
                pitfall("2024-03-01", "A silent edit in the schedules broadened Affiliate to any future subsidiary."),
                pattern("2024-04-01", "Enterprise vendors concede the SLA credit cap only after the liability cap is settled."),
                lesson("2024-08-01", "Settle the liability cap before the SLA credits, or the credits count against it."),
            ],
        ),
        case(
            "英文SaaS契約、ベンダー側",
            facts(Saas, Vendor, Ours, Sme, "JP"),
            &[("2025-03-01", Stronger)],
            vec![
                pitfall("2025-03-10", "相手方の修正履歴に表示されない定義語の変更があった。"),
                pattern("2025-04-01", "中小企業の顧客は支払条件より解約条項を先に争う。"),
                position("2025-05-01", Topic::Termination, "解約予告期間を30日から60日に延長した。"),
                lesson("2025-06-01", "責任制限の上限は年間料金の額に固定し、間接損害を除外する。"),
                leverage("2025-02-15", "顧客は四半期末までの導入を急いでいた。"),
            ],
        ),
        case(
            "秘密保持契約、受領側",
            facts(Nda, Customer, Theirs, Startup, "JP"),
            &[("2025-09-01", Even)],
            vec![
                pitfall("2025-09-05", "秘密情報の定義に口頭開示が含まれ、記録義務がなかった。"),
                pattern("2025-09-10", "スタートアップは残存条項の期間を5年で提示する。"),
                position("2025-09-20", Topic::Confidentiality, "秘密保持期間を5年から3年に短縮した。"),
                lesson("2025-10-01", "準拠法を東京地方裁判所の専属管轄と合わせて確認する。"),
                leverage("2025-09-02", "相手方は資金調達の前に契約を必要としていた。"),
            ],
        ),
        case(
            "Master services agreement, vendor side, US",
            facts(Msa, Vendor, Theirs, Enterprise, "US-NY"),
            &[("2023-05-01", Weaker)],
            vec![
                pitfall("2023-05-10", "Indemnity clause covered third-party claims and first-party losses alike."),
                pattern("2023-06-01", "New York enterprises want uncapped indemnity for IP infringement."),
                position("2023-07-01", Topic::Indemnity, "Indemnity narrowed to third-party IP claims only."),
                lesson("2023-08-01", "Hold harmless language without a cap is uncapped liability under another name."),
                leverage("2023-05-05", "We were the only vendor certified for their platform."),
            ],
        ),
        case(
            "Software licence, licensor side",
            facts(Licence, Licensor, Ours, Sme, "GB"),
            &[("2024-10-01", Stronger)],
            vec![
                pitfall("2024-10-05", "Licence scope said enterprise-wide while the fee schedule counted seats."),
                pattern("2024-10-10", "SMEs ask for a source code escrow in the second round."),
                position("2024-11-01", Topic::Ip, "Intellectual property in customisations stays with the licensor."),
                lesson("2024-12-01", "Warranty disclaimers need the governing law's conspicuous-text rule checked."),
                leverage("2024-10-02", "There was no comparable product in their price band."),
            ],
        ),
        case(
            "Supply agreement, buyer side",
            facts(Supply, Buyer, Theirs, Enterprise, "DE"),
            &[("2025-01-15", Weaker)],
            vec![
                pitfall("2025-01-20", "Force majeure listed supplier strikes, which shifted the delivery risk to us."),
                pattern("2025-02-01", "German suppliers tie payment to delivery milestones rather than acceptance."),
                position("2025-03-01", Topic::Payment, "Payment moved from on delivery to on acceptance."),
                lesson("2025-04-01", "Check the Incoterm before the price, since it moves the insurance cost."),
                leverage("2025-01-16", "They needed a reference customer in our market."),
            ],
        ),
        case(
            "業務委託契約、委託者側",
            facts(Services, Customer, Ours, Sme, "JP"),
            &[("2024-04-01", Stronger)],
            vec![
                pitfall("2024-04-05", "成果物の著作権が受託者に残る条項が別紙にあった。"),
                pattern("2024-04-10", "受託者は再委託の承諾を包括的に求める。"),
                position("2024-05-01", Topic::Ip, "著作権は検収と同時に委託者へ移転することになった。"),
                lesson("2024-06-01", "個人情報の取扱いは別途覚書で定め、本契約では参照にとどめる。"),
                leverage("2024-04-02", "受託者は当社との継続取引を重視していた。"),
            ],
        ),
        case(
            "Data processing addendum, controller side",
            facts(Saas, Customer, Theirs, Enterprise, "GB"),
            &[("2025-06-01", Even)],
            vec![
                pitfall("2025-06-05", "The processor's sub-processor list was a URL that could change without notice."),
                pattern("2025-06-10", "Enterprise processors offer a 30-day objection window for new sub-processors."),
                position("2025-07-01", Topic::DataProtection, "Personal data breach notice shortened from 72 to 48 hours."),
                lesson("2025-08-01", "Data protection terms sit in the addendum and the MSA cap must say whether they count."),
                leverage("2025-06-02", "Their largest competitor had just signed on our terms."),
            ],
        ),
        case(
            "Distribution agreement, distributor side",
            facts(Distribution, Buyer, Theirs, Enterprise, "US-CA"),
            &[("2022-02-01", Weaker)],
            vec![
                pitfall("2022-02-05", "Minimum purchase commitment was measured in list price, not net."),
                pattern("2022-03-01", "Suppliers tie exclusivity to minimum volumes and review it yearly."),
                position("2022-04-01", Topic::Termination, "Termination for convenience needs 180 days' notice."),
                lesson("2022-05-01", "Assignment on change of control needs carving out for a group reorganisation."),
                leverage("2022-02-02", "We held the only logistics network in the region."),
            ],
        ),
        case(
            "賃貸借契約、賃借人側",
            facts(Lease, Tenant, Theirs, Enterprise, "JP"),
            &[("2023-09-01", Even)],
            vec![
                pitfall("2023-09-05", "原状回復の範囲が通常損耗を含む書き方になっていた。"),
                pattern("2023-09-10", "大手の貸主は中途解約の違約金を残期間の賃料で提示する。"),
                position("2023-10-01", Topic::Termination, "中途解約の予告期間を6か月とした。"),
                lesson("2023-11-01", "保証金の返還時期を明記しないと退去後の交渉になる。"),
                leverage("2023-09-02", "空室が多く、貸主は早期の入居を望んでいた。"),
            ],
        ),
        case(
            "Loan agreement, borrower side",
            facts(Loan, Borrower, Theirs, Enterprise, "GB"),
            &[("2021-03-01", Weaker)],
            vec![
                pitfall("2021-03-05", "Cross-default clause reached our group companies' leases."),
                pattern("2021-04-01", "Lenders accept a materiality threshold on representations in round two."),
                position("2021-05-01", Topic::Payment, "Prepayment fee waived after the second year."),
                lesson("2021-06-01", "Warranties repeat on each drawdown unless the clause says otherwise."),
                leverage("2021-03-02", "The lender's quarter closed in two weeks."),
            ],
        ),
    ]
}

fn pool(cases: &[Case]) -> Vec<Candidate<'_>> {
    cases
        .iter()
        .enumerate()
        .flat_map(|(i, c)| {
            let place = if i == 0 { Place::ThisCase } else { Place::SameClient };
            c.notes.iter().map(move |n| Candidate { note: n, case: c, place })
        })
        .collect()
}

fn query(cases: &[Case], about: &str) -> Query {
    Query::new(about, cases[0].applies(day(TODAY)), day(TODAY))
}

fn text_of(r: &Ranked<'_>) -> String {
    r.candidate.note.body.texts().map(Quoted::for_person).collect::<Vec<_>>().join(" ")
}

fn rank_of(ranked: &[Ranked<'_>], needle: &str) -> usize {
    ranked.iter().position(|r| text_of(r).contains(needle)).unwrap_or_else(|| panic!("{needle:?} not recalled"))
}

#[test]
fn a_japanese_query_finds_an_english_note_through_the_topic_and_the_reverse() {
    let cases = corpus();
    let pool = pool(&cases);

    let ja = rank(&query(&cases, "責任制限の上限について"), &pool);
    assert!(ja.iter().any(|r| text_of(r).contains("Liability cap raised to twelve months")));

    let en = rank(&query(&cases, "how did we handle the confidentiality term"), &pool);
    assert!(en.iter().any(|r| text_of(r).contains("秘密保持期間を5年から3年に短縮した")));
}

#[test]
fn weaker_now_a_concession_won_while_weaker_outranks_a_wordier_one_won_while_stronger() {
    let cases = corpus();
    let pool = pool(&cases);
    let q = query(&cases, "liability cap in months of fees, and the exclusions for data breach and indemnity claims");
    assert_eq!(cases[0].stance_now(), Some((Weaker, confirmed("2026-10-01"))));

    let typed = rank(&q, &pool);
    let won_weaker = rank_of(&typed, "twelve months of fees");
    let won_stronger = rank_of(&typed, "twenty-four months of fees");
    assert!(won_weaker < won_stronger, "typed: weaker {won_weaker}, stronger {won_stronger}");

    let text_only = rank_by_text(&q, &pool);
    let won_weaker = rank_of(&text_only, "twelve months of fees");
    let won_stronger = rank_of(&text_only, "twenty-four months of fees");
    assert!(won_stronger < won_weaker, "text only: weaker {won_weaker}, stronger {won_stronger}");
}

#[test]
fn a_pitfall_ranks_the_same_whatever_the_stance() {
    let mut cases = corpus();
    let scores: Vec<f32> = [Stronger, Even, Weaker]
        .into_iter()
        .map(|now| {
            cases[0].balance = vec![Balance { on: day("2026-10-01"), stance: now, standing: confirmed("2026-10-01") }];
            let pool = pool(&cases);
            let ranked = rank(&query(&cases, "a silent edit broadened Affiliate in the schedules"), &pool);
            ranked[rank_of(&ranked, "broadened Affiliate")].score
        })
        .collect();
    assert!(scores[0] > 0.0);
    assert_eq!(scores[0], scores[1]);
    assert_eq!(scores[1], scores[2]);
}

#[test]
fn a_stale_item_ranks_below_an_equal_fresh_one() {
    let mut cases = corpus();
    let text = "Check the Incoterm before the price, since it moves the insurance cost.";
    cases[6].notes.push(lesson("2026-10-01", text));
    let pool = pool(&cases);
    let ranked = rank(&query(&cases, "incoterm and insurance cost"), &pool);
    let both: Vec<&Ranked<'_>> = ranked.iter().filter(|r| text_of(r) == text).collect();
    assert_eq!(both.len(), 2);
    assert_eq!(both[0].candidate.note.at, day("2026-10-01"));
    assert!(both[0].score > both[1].score);
}

#[test]
fn a_query_of_only_hiragana_matches_nothing() {
    let cases = corpus();
    let pool = pool(&cases);
    assert!(rank(&query(&cases, "これはそれでしょう"), &pool).is_empty());
}

#[test]
fn similar_facts_with_no_topic_or_word_in_common_recall_nothing() {
    let cases = corpus();
    let twin = Case { notes: vec![pattern("2026-06-01", "Quarterly business reviews happen every third Tuesday.")], ..cases[0].clone() };
    let pool = vec![Candidate { note: &twin.notes[0], case: &twin, place: Place::SameClient }];
    let q = Query::new("governing law and jurisdiction", cases[0].applies(day(TODAY)), day(TODAY));
    assert!(rank(&q, &pool).is_empty());
}

#[test]
fn every_topic_has_an_english_and_a_japanese_term() {
    for topic in Topic::ALL {
        let terms = TERMS.iter().find(|(t, _)| *t == topic).map(|(_, terms)| *terms).unwrap_or(&[]);
        assert!(terms.iter().any(|t| t.is_ascii()), "{topic:?} has no English term");
        assert!(terms.iter().any(|t| !t.is_ascii()), "{topic:?} has no Japanese term");
        assert!(topics_in(terms[0]).contains(topic));
    }
}

#[test]
fn hostile_text_tokenizes_within_a_bound() {
    for c in ['契', '['] {
        let text: String = std::iter::repeat_n(c, 1 << 20).collect();
        assert!(tokens(&text).len() <= super::text::MAX_TOKENS);
    }
}

#[test]
fn full_width_and_mixed_text_tokenize_as_the_design_says() {
    let got = token_texts("ＳaaＳ契約 第１条");
    assert_eq!(got, ["saas", "契約", "第", "1", "条"]);
    assert!(token_texts("この").is_empty());
}

fn token_texts(text: &str) -> Vec<String> {
    tokens(text).into_iter().map(|t| t.for_test()).collect()
}

#[test]
fn a_long_word_becomes_one_short_token() {
    let got = token_texts(&"a".repeat(1 << 20));
    assert_eq!(got.len(), 1);
    assert!(got.iter().all(|t| t.len() <= 64));
}

#[test]
fn a_long_hiragana_run_keeps_the_words_after_it() {
    let got = token_texts(&format!("{}契約", "あ".repeat(5000)));
    assert!(got.iter().any(|t| t == "契約"));
}

#[test]
fn one_hiragana_alone_is_no_word() {
    assert!(token_texts("の").is_empty());
}

#[test]
fn every_term_finds_its_own_topic() {
    for (topic, terms) in TERMS {
        for term in *terms {
            let term = term.trim_end_matches('*');
            assert!(topics_in(term).contains(*topic), "{term} doesn't find {topic:?}");
        }
    }
}

#[test]
fn a_short_term_matches_whole_words_only() {
    assert!(!topics_in("Please review this draft and give feedback").contains(Topic::Payment));
    assert!(!topics_in("a slash in the heading").contains(Topic::Sla));
    assert!(topics_in("the fees are due").contains(Topic::Payment));
    assert!(topics_in("indemnification").contains(Topic::Indemnity));
}

#[test]
fn an_english_term_inside_japanese_text_is_found() {
    assert!(topics_in("本ＮＤＡの条件").contains(Topic::Confidentiality));
    assert!(topics_in("相手のSLA条項").contains(Topic::Sla));
}

#[test]
fn a_lease_deposit_is_not_a_warranty() {
    assert!(!topics_in("保証金は３か月分").contains(Topic::Warranties));
}

#[test]
fn applies_holds_no_text() {
    fn is_copy<T: Copy>() {}
    is_copy::<Applies>();
    assert_eq!(Kind::ALL.len(), 5);
}
