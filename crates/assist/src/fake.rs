//! A scripted backend for tests and for trying the pane without a model: each turn plays the
//! steps it was given ([`Step`]), or, unscripted, says what it was asked.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;

use crate::backend::{
    Answer, Backend, BackendInfo, Event, PermissionKind, PermissionOption, PermissionRequest, PlanEntry, Seat, StopReason, Turn, TurnCtx,
};

/// The fake backend's id.
pub const FAKE_ID: &str = "fake";

/// Longest a [`Step::Hold`] waits to be stopped.
pub const HOLD: Duration = Duration::from_secs(30);

/// One step of a scripted turn.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Say this (a piece of the answer).
    Say(String),
    /// Think this.
    Think(String),
    /// Call this tool with these arguments.
    Tool { name: String, args: Value },
    /// Show this plan.
    Plan(Vec<PlanEntry>),
    /// Ask permission for this, offering allow once and reject once; the answer is kept
    /// ([`Fake::answers`]).
    Ask(String),
    /// Ask permission for `title`, offering `options` as they are (to test what the pane does
    /// with an agent's odd lists); the answer is kept.
    AskWith { title: String, options: Vec<PermissionOption> },
    /// Wait until the turn is stopped (at most [`HOLD`]).
    Hold,
    /// Fail the turn with this.
    Fail(String),
    /// End the turn with this stop reason.
    Stop(StopReason),
    /// Send this event as it is (to test what the conversation does with what a backend sends).
    Emit(Event),
}

impl Step {
    /// A tool call step.
    pub fn tool(name: &str, args: Value) -> Step {
        Step::Tool { name: name.to_string(), args }
    }
    /// A step that says `text`.
    pub fn say(text: &str) -> Step {
        Step::Say(text.to_string())
    }
}

/// What a fake backend saw: the turns it was asked, the seat it began with, the permission
/// answers it got, and whether it ended. Shared with the test that made it.
#[derive(Clone, Debug, Default)]
pub struct Seen {
    pub turns: Vec<Turn>,
    pub seat: Option<(String, wordcraft_engine::room::Role)>,
    pub answers: Vec<Answer>,
    pub ended: bool,
}

/// The scripted backend.
pub struct Fake {
    info: BackendInfo,
    script: VecDeque<Vec<Step>>,
    seen: Arc<Mutex<Seen>>,
}

impl Default for Fake {
    fn default() -> Self {
        Fake::new()
    }
}

impl Fake {
    /// A fake backend that joins as "Test Assistant".
    pub fn new() -> Fake {
        Fake::named("Test Assistant")
    }

    /// A fake backend that joins as `author`.
    pub fn named(author: &str) -> Fake {
        Fake {
            info: BackendInfo { id: FAKE_ID.into(), label: "Scripted assistant (for testing)".into(), author: author.into(), in_process_tools: true },
            script: VecDeque::new(),
            seen: Arc::default(),
        }
    }

    /// Play `steps` for the next turn not scripted yet.
    pub fn then(mut self, steps: Vec<Step>) -> Fake {
        self.script.push_back(steps);
        self
    }

    /// What it saw, as it sees it.
    pub fn seen(&self) -> Arc<Mutex<Seen>> {
        self.seen.clone()
    }

    fn see(&self, f: impl FnOnce(&mut Seen)) {
        f(&mut self.seen.lock().unwrap_or_else(PoisonError::into_inner));
    }
}

impl Backend for Fake {
    fn info(&self) -> BackendInfo {
        self.info.clone()
    }

    fn begin(&mut self, seat: &Seat) -> Result<(), String> {
        self.see(|s| s.seat = Some((seat.name.clone(), seat.role)));
        Ok(())
    }

    fn turn(&mut self, turn: &Turn, ctx: &mut TurnCtx<'_>) -> Result<StopReason, String> {
        self.see(|s| s.turns.push(turn.clone()));
        let steps = self.script.pop_front().unwrap_or_else(|| {
            let asked: String = turn.prompt.chars().take(200).collect();
            vec![Step::Say(format!("I'm the scripted test assistant: I don't write anything. You asked: {asked}"))]
        });
        ctx.emit(Event::Model { name: "scripted".into() });
        for (n, step) in steps.into_iter().enumerate() {
            if ctx.is_cancelled() {
                return Ok(StopReason::Cancelled);
            }
            match step {
                Step::Say(t) => ctx.say(t),
                Step::Think(t) => ctx.emit(Event::Thought { text: t }),
                Step::Tool { name, args } => {
                    ctx.call_tool(&format!("fake-{n}"), &name, &args);
                }
                Step::Plan(entries) => ctx.emit(Event::Plan { entries }),
                Step::Ask(title) => {
                    let options = [("allow", "Allow once", PermissionKind::AllowOnce), ("reject", "Reject", PermissionKind::RejectOnce)]
                        .map(|(id, name, kind)| PermissionOption { id: id.into(), name: name.into(), kind });
                    let answer = ctx.ask(PermissionRequest::bounded(&title, options));
                    self.see(|s| s.answers.push(answer));
                }
                Step::AskWith { title, options } => {
                    let answer = ctx.ask(PermissionRequest { id: 0, title, options });
                    self.see(|s| s.answers.push(answer));
                }
                Step::Hold => {
                    ctx.cancel().wait(HOLD);
                }
                Step::Fail(why) => return Err(why),
                Step::Stop(stop) => return Ok(stop),
                Step::Emit(event) => ctx.emit(event),
            }
        }
        Ok(if ctx.is_cancelled() { StopReason::Cancelled } else { StopReason::EndTurn })
    }

    fn end(&mut self) {
        self.see(|s| s.ended = true);
    }
}
