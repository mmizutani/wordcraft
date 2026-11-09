//! An answer in the Assistant pane, as formatted text: [`wordcraft_assist::answer`] reads the
//! model's Markdown, once for each text it wrote, and this draws what it read. A link is a widget
//! of its own in the flow of the text, reachable by keyboard and named by its words, and says
//! where it goes: words can say anything. A citation is a numbered chip that selects the words it
//! cites, and shows only when those words are really in the document or an attached source, so a
//! made-up one can't look real. A preference's chip says which of the person's preferences shaped
//! what it follows ([`wordcraft_assist::memory`]), and goes to the list in Details.

use std::sync::Arc;

use egui::cache::{ComputerMut, FrameCache};
use egui::{RichText, Stroke, Ui};
use wordcraft_assist::answer::{Block, Formatted, Span, UsedChip, format, opens};

use super::{Tokens, WordApp, semibold};
use crate::assist::{Cited, cited};
use crate::i18n::fmt;

/// Most characters of a link's address shown beside its words; all of it on hover.
const ADDRESS_CHARS: usize = 80;

#[derive(Default)]
struct Reader;

impl ComputerMut<(&str, bool), Arc<Formatted>> for Reader {
    fn compute(&mut self, (text, streaming): (&str, bool)) -> Arc<Formatted> {
        Arc::new(format(text, streaming))
    }
}

/// What the answers shown read as: the frames between two pieces of an answer read nothing.
type Read = FrameCache<Arc<Formatted>, Reader>;

/// Draw the answer `text`, the conversation's entry at `entry`; `streaming`: more of it is still
/// coming.
pub(super) fn answer(app: &mut WordApp, ui: &mut Ui, t: &Tokens, entry: usize, text: &str, streaming: bool) {
    let read = ui.ctx().memory_mut(|m| m.caches.cache::<Read>().get((text, streaming)).clone());
    let size = egui::TextStyle::Body.resolve(ui.style()).size;
    let mut chips = Chips { entry, words: &read.citations, shown: 0 };
    for (i, block) in read.blocks.iter().enumerate() {
        if i > 0 {
            ui.add_space(4.0);
        }
        match block {
            Block::Para(spans) => inline(app, ui, t, spans, size, &mut chips),
            Block::Heading { level, spans } => {
                let bigger = match level {
                    1 => 3.0,
                    2 => 2.0,
                    _ => 1.0,
                };
                let bold: Vec<Span> = spans.iter().cloned().map(|s| Span { bold: true, ..s }).collect();
                inline(app, ui, t, &bold, size + bigger, &mut chips);
            }
            Block::Item { marker, level, spans } => {
                ui.horizontal_top(|ui| {
                    ui.add_space(f32::from(*level) * 12.0);
                    ui.label(RichText::new(marker).color(t.text_dim));
                    ui.vertical(|ui| inline(app, ui, t, spans, size, &mut chips));
                });
            }
            Block::Quote(spans) => {
                let area = egui::Frame::NONE
                    .inner_margin(egui::Margin { left: 10, ..Default::default() })
                    .show(ui, |ui| inline(app, ui, t, spans, size, &mut chips));
                let rect = area.response.rect;
                ui.painter().vline(rect.left() + 2.0, rect.y_range(), Stroke::new(2.0, t.border_strong));
            }
            Block::Code(text) => {
                egui::Frame::NONE.fill(t.checked).corner_radius(4).inner_margin(6).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(egui::Label::new(RichText::new(text).monospace().color(t.text)).wrap());
                });
            }
            Block::Rule => {
                ui.separator();
            }
        }
    }
    if !read.rest.is_empty() {
        ui.add(egui::Label::new(RichText::new(&read.rest).color(t.text)).wrap());
    }
}

/// The spans of one block, flowing and wrapping as one text.
fn inline(app: &mut WordApp, ui: &mut Ui, t: &Tokens, spans: &[Span], size: f32, chips: &mut Chips<'_>) {
    if let [only] = spans
        && only.link.is_none()
        && only.cite.is_none()
        && only.used.is_none()
    {
        ui.add(egui::Label::new(rich(only, t, size)).wrap());
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (i, span) in spans.iter().enumerate() {
            if let Some(cite) = span.cite {
                chips.draw(app, ui, cite);
                continue;
            }
            if let Some(used) = &span.used {
                used_chip(app, ui, t, chips.entry, used);
                continue;
            }
            match &span.link {
                None => {
                    ui.add(egui::Label::new(rich(span, t, size)).wrap());
                }
                Some(address) => {
                    let last_of_link = spans.get(i.saturating_add(1)).is_none_or(|next| next.link != span.link);
                    link(app, ui, t, span, address, size, last_of_link);
                }
            }
        }
    });
}

/// A link: it opens on the person's click only, and only for a web address; its address shows
/// beside its words, unless they are its address.
fn link(app: &mut WordApp, ui: &mut Ui, t: &Tokens, span: &Span, address: &str, size: f32, last_of_link: bool) {
    if opens(address) {
        let words = rich(span, t, size).color(ui.visuals().hyperlink_color);
        web_link(app, ui, words, address);
    } else {
        ui.add(egui::Label::new(rich(span, t, size)).wrap());
    }
    if last_of_link && span.text.trim() != address {
        ui.add(egui::Label::new(RichText::new(beside(address)).small().color(t.text_dim)).wrap()).on_hover_text(address);
    }
}

/// An answer's citations, and how many have shown so far: the numbers count only those.
struct Chips<'a> {
    /// The answer's place in the conversation: a preference's chip reads its turn's words.
    entry: usize,
    words: &'a [String],
    shown: usize,
}

impl Chips<'_> {
    /// The chip of citation `cite`, when its words are there to select. A click selects them; a
    /// source has nothing to select, so it says which source has them.
    fn draw(&mut self, app: &mut WordApp, ui: &mut Ui, cite: usize) {
        let Some(words) = self.words.get(cite) else { return };
        let found = cited(app, words);
        if found == Cited::Nowhere {
            return;
        }
        self.shown = self.shown.saturating_add(1);
        let number = self.shown.to_string();
        let chip = ui.small_button(RichText::new(number).small()).on_hover_text(format!("“{words}”"));
        if chip.clicked() {
            match found {
                Cited::Source(name) => app.assist.notice = Some(fmt(tl!("That is from {name}."), &[("name", &name)])),
                _ => select_cited(app, words),
            }
        }
    }
}

/// A preference's chip: its words as its turn sent them on hover, and a click shows it in Details ›
/// Memory. Only for what that turn sent: the transcript checks each marker, but text can come to
/// read as one once it is drawn.
fn used_chip(app: &mut WordApp, ui: &mut Ui, t: &Tokens, entry: usize, used: &UsedChip) {
    let Some(said) = app.conversation().and_then(|a| a.transcript().sent_preference(entry, &used.id).map(str::to_string)) else { return };
    let words = RichText::new(crate::assist::memory::chip_label(used.number)).small().color(t.accent);
    if ui.small_button(words).on_hover_text(said).clicked() {
        app.assist.view = crate::assist::PaneView::Details;
        app.assist.memory.show_list = true;
    }
}

/// Select the words a citation cites.
pub(super) fn select_cited(app: &mut WordApp, words: &str) {
    if !crate::assist::select_cited(app, words) {
        app.assist.notice = Some(tl!("Those words aren't in the document any more.").to_string());
    }
}

/// A link to web `address` from what a model wrote: it opens on the person's click only.
pub(super) fn web_link(app: &mut WordApp, ui: &mut Ui, words: RichText, address: &str) {
    let r = ui.add(egui::Link::new(words)).on_hover_text(address);
    if r.clicked() {
        // A click the control channel made doesn't open what a model wrote.
        if app.input_from_control(ui.ctx()) {
            app.assist.notice = Some(tl!("Only you can open a link in an answer: click it yourself.").to_string());
        } else {
            app.canvas.open_url = Some(address.to_string());
        }
    }
}

/// A link's address as it shows beside its words.
fn beside(address: &str) -> String {
    if address.chars().count() > ADDRESS_CHARS {
        format!(" ({}…)", address.chars().take(ADDRESS_CHARS).collect::<String>())
    } else {
        format!(" ({address})")
    }
}

fn rich(span: &Span, t: &Tokens, size: f32) -> RichText {
    let mut text = RichText::new(&span.text).color(t.text);
    text = if span.code {
        text.monospace().background_color(t.checked)
    } else if span.bold {
        text.font(semibold(size))
    } else {
        text.size(size)
    };
    if span.italic {
        text = text.italics();
    }
    if span.strike {
        text = text.strikethrough();
    }
    text
}

/// The words an answer shows as the model wrote them, which the interface audit leaves
/// untranslated.
#[cfg(test)]
pub(crate) fn shown_texts(text: &str) -> Vec<String> {
    let read = format(text, false);
    let mut out = vec![text.to_string(), read.rest.clone()];
    for block in &read.blocks {
        let spans = match block {
            Block::Para(s) | Block::Quote(s) | Block::Heading { spans: s, .. } => s.as_slice(),
            Block::Item { marker, spans, .. } => {
                out.push(marker.clone());
                spans.as_slice()
            }
            Block::Code(code) => {
                out.push(code.clone());
                &[]
            }
            Block::Rule => &[],
        };
        for s in spans {
            out.push(s.text.clone());
            out.extend(s.link.iter().flat_map(|l| [l.clone(), beside(l)]));
        }
    }
    out
}
