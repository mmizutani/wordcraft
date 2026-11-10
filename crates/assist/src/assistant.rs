//! A conversation with an assistant ([`Assistant`]): a backend, the participant it joined the
//! shared document as, and the thread its turns run on.
//!
//! Starting one takes an invite from the room under the backend's author name and the role the
//! person chose ([`role_for`]), so whatever the backend does reaches the document as that
//! participant: held to its role, a suggester's edits tracked in its name. A backend that runs
//! the model loop in WordCraft gets a copy of the document of its own, joined with the invite's
//! key, which its tool calls work on; an ACP agent joins with the key itself, through WordCraft's
//! MCP server. Ending the conversation takes the invite back, which ends every copy that joined
//! with it.
//!
//! The turns run on a thread of the conversation's own: the window sends a prompt
//! ([`Assistant::ask`]), reads what came back each frame ([`Assistant::poll`]), and never waits.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel, sync_channel};
use std::time::{Duration, Instant};

use wordcraft_doc::change::Anchor;
use wordcraft_engine::AssistantPrefs;
use wordcraft_engine::room::{Role, Room, RoomError};

use crate::backend::{
    Answer, Backend, BackendInfo, Cancel, Event, Events, MAX_QUEUED_EVENTS, NoTools, Offered, PermissionPolicy, PermissionRequest, Permissions, Seat,
    StopReason, ToolHost, Turn, TurnCtx, Waker,
};
use crate::cases::recall::{RecallAsk, Told};
use crate::context::Sent;
use crate::memory::Instructions;
use crate::tools::CopyTools;
use crate::transcript::{Details, Entry, Properties, Transcript};

/// Most events [`Assistant::poll`] takes at once, so a chatty backend can't hold up a frame.
pub const MAX_EVENTS_PER_POLL: usize = 512;

/// Most bytes of a prompt; more is refused.
pub const MAX_PROMPT: usize = 64 << 10;

/// Why a conversation didn't start or take a prompt.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AssistError {
    /// The room didn't let the assistant in.
    #[error("{0}")]
    Room(#[from] RoomError),
    /// The role can't be an assistant's.
    #[error("{0}")]
    Role(String),
    /// A turn is running.
    #[error("the assistant is still answering: stop it first")]
    Busy,
    /// The conversation ended.
    #[error("this conversation has ended: start a new one")]
    Ended,
    /// The prompt is empty or too long.
    #[error("{0}")]
    Prompt(String),
    /// The conversation's thread couldn't start (the web build has none).
    #[error("the assistant can't run here: {0}")]
    NoThread(String),
}

/// The role an assistant joins with: `choice`, this conversation's own, else `setting`, the
/// person's `assistant.defaultRole` (suggester unless they changed it). Owner is never an
/// assistant's: a choice of owner is refused, and a setting of owner reads as suggester.
pub fn role_for(setting: Role, choice: Option<Role>) -> Result<Role, AssistError> {
    match choice {
        Some(Role::Owner) => {
            Err(AssistError::Role("an assistant can't be an owner: owners change the application's settings and share the document".into()))
        }
        Some(role) => Ok(role),
        None => Ok(AssistantPrefs { default_role: setting, ..AssistantPrefs::default() }.default_role()),
    }
}

/// How a conversation starts.
#[derive(Clone, Default)]
pub struct Options {
    /// The person's setting for the role assistants join with (`assistant.defaultRole`).
    pub setting: Option<Role>,
    /// This conversation's role, in place of the setting.
    pub role: Option<Role>,
    /// The shared document's title, for the note on the first turn.
    pub document: String,
    /// How the agent's permission requests are answered: [`PermissionPolicy::Ask`] in the
    /// window; rejected, unless a setting allows, when nobody is there to ask. A turn may be
    /// answered its own way ([`Assistant::ask_with_policy`]).
    pub permissions: PermissionPolicy,
    /// Called when events arrive: the window asks to be drawn again.
    pub wake: Option<Waker>,
}

/// What the conversation's thread is asked to do.
enum Job {
    /// A turn, how its permission requests are answered when not as the conversation's
    /// ([`Assistant::ask_with_policy`]), and what it recalls from the person's cases
    /// ([`Assistant::set_recall`]), which rides beside the turn because a proof is used once.
    Turn(Box<Turn>, Option<PermissionPolicy>, Option<Box<RecallAsk>>),
    End,
}

/// One conversation with an assistant, which joined the shared document as a participant.
pub struct Assistant {
    info: BackendInfo,
    seat: Seat,
    room: Arc<Room>,
    jobs: Sender<Job>,
    events: Option<Receiver<Event>>,
    cancel: Cancel,
    permissions: Permissions,
    transcript: Transcript,
    /// Whether a turn is running: from [`Assistant::ask`] until its last event is polled.
    busy: bool,
    /// Whether the next turn is the conversation's first, which carries the note.
    first: bool,
    /// What each turn from now on carries of what the person kept ([`Assistant::set_memory`]).
    memory: Option<Instructions>,
    /// What the next turn recalls from the person's cases ([`Assistant::set_recall`]): one turn's.
    recall: Option<RecallAsk>,
    /// What the next turns carry of the sources the person attached.
    knowledge: Vec<crate::knowledge::TurnSource>,
    /// The model and reasoning effort each turn carries ([`Assistant::choose`]).
    choice: (Option<String>, Option<String>),
    /// What the backend last said it offers, once it has.
    offered: Option<Offered>,
    /// Whether any turn was sent with something to recall, or recalled anything: from then on
    /// the model may hold case text, whoever can read the document later.
    recalled: bool,
    ended: bool,
}

impl Assistant {
    /// Start a conversation with `backend` in `room`: take an invite under the backend's author
    /// name (or that name with a number, when the room has it already) and the role
    /// [`role_for`] gives, and start the conversation's thread, which joins with the invite.
    pub fn start(room: &Arc<Room>, backend: Box<dyn Backend>, options: Options) -> Result<Assistant, AssistError> {
        let info = backend.info();
        let role = role_for(options.setting.unwrap_or(Role::Suggester), options.role)?;
        let (name, key) = invite(room, &info.author, role)?;
        let seat = Seat { name, role, key, document: options.document.clone() };
        let (jobs_tx, jobs_rx) = channel();
        let (events_tx, events_rx) = sync_channel(MAX_QUEUED_EVENTS);
        let events = Events::new(events_tx, options.wake.clone());
        let cancel = Cancel::default();
        let permissions = Permissions::default();
        let work = Work {
            backend,
            room: room.clone(),
            seat: seat.clone(),
            jobs: jobs_rx,
            events,
            cancel: cancel.clone(),
            permissions: permissions.clone(),
            policy: options.permissions,
            told: Told::default(),
        };
        // A thread's name can't hold a NUL (spawning would panic): the id, as a short label.
        let thread = format!("assistant {}", info.id.chars().filter(|c| !c.is_control()).take(40).collect::<String>());
        let spawned = std::thread::Builder::new().name(thread).spawn(move || work.run());
        if let Err(e) = spawned {
            room.revoke(&seat.key);
            return Err(AssistError::NoThread(e.to_string()));
        }
        log::info!("assistant {} joined the shared document as {:?}, a {}", info.id, seat.name, seat.role);
        Ok(Assistant {
            info,
            seat,
            room: room.clone(),
            jobs: jobs_tx,
            events: Some(events_rx),
            cancel,
            permissions,
            transcript: Transcript::default(),
            busy: false,
            first: true,
            memory: None,
            recall: None,
            knowledge: Vec::new(),
            choice: (None, None),
            offered: None,
            recalled: false,
            ended: false,
        })
    }

    /// The backend.
    pub fn info(&self) -> &BackendInfo {
        &self.info
    }

    /// The name the assistant joined under: the author of its tracked changes and comments.
    pub fn name(&self) -> &str {
        &self.seat.name
    }

    /// Its role in the shared document.
    pub fn role(&self) -> Role {
        self.seat.role
    }

    /// Whether its edits are tracked changes in its name.
    pub fn tracked(&self) -> bool {
        self.seat.tracked()
    }

    /// The room it joined.
    pub fn room(&self) -> &Arc<Room> {
        &self.room
    }

    /// The conversation so far.
    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    /// For tests: a window's test records what a backend can't send ([`Event::Recalled`]).
    #[doc(hidden)]
    pub fn transcript_mut(&mut self) -> &mut Transcript {
        &mut self.transcript
    }

    /// The conversation in summary, with what runs it: what the pane's Details shows and
    /// `assist.details` answers.
    pub fn details(&self) -> Details {
        Details {
            properties: Some(Properties {
                backend: self.info.label.clone(),
                name: self.seat.name.clone(),
                document: self.seat.document.clone(),
                role: self.seat.role.name().to_string(),
                tracked: self.seat.tracked(),
            }),
            ..self.transcript.details()
        }
    }

    /// Whether a turn is running.
    pub fn is_busy(&self) -> bool {
        self.busy
    }

    /// Whether the conversation ended ([`Assistant::end`]).
    pub fn has_ended(&self) -> bool {
        self.ended
    }

    /// Whether any turn was sent with something to recall from the person's cases
    /// ([`Assistant::set_recall`]), or recalled anything: set as the turn is sent and again from
    /// the event as it is polled, so it can't miss one and never lags the thread.
    pub fn recalled(&self) -> bool {
        self.recalled
    }

    /// The preferences the conversation's answers marked since the last call.
    pub fn take_used(&mut self) -> Vec<String> {
        self.transcript.take_used()
    }

    /// The case markers the conversation's answers carried since the last call
    /// ([`crate::cases::marker`]), for the window to route to this conversation's case.
    pub fn take_case_markers(&mut self) -> Vec<crate::cases::CaseMarker> {
        self.transcript.take_case_markers()
    }

    /// The turn that just ended, once, as one line for this conversation's case record
    /// ([`crate::cases::log`]); `None` while a turn runs or when the last one was taken.
    pub fn take_finished_turn(&mut self) -> Option<crate::cases::Record> {
        self.transcript.take_finished_turn()
    }

    /// A marker's write landed in the conversation's case: the pane shows a quiet row.
    pub fn case_written(&mut self, what: crate::transcript::Written, note: Option<crate::cases::NoteId>, text: &str) {
        self.transcript.case_written(what, note, text);
    }

    /// A marker became a card for the person: its number.
    pub fn case_card(&mut self, card: crate::cases::Card) -> u64 {
        self.transcript.case_card(card)
    }

    /// Card `id`, while the person hasn't answered it.
    pub fn pending_case_card(&self, id: u64) -> Option<&crate::cases::Card> {
        self.transcript.pending_case_card(id)
    }

    /// The person answered card `id`.
    pub fn answer_case_card(&mut self, id: u64, answer: crate::transcript::ProposalAnswer) {
        self.transcript.answer_case_card(id, answer);
    }

    /// What the next turns carry of what the person kept ([`crate::memory::Memory::for_model`]).
    pub fn set_memory(&mut self, memory: Option<Instructions>) {
        self.memory = memory;
    }

    /// What the next turn recalls from the person's cases, run on the conversation's thread just
    /// before the backend's turn and given to it as data ([`crate::cases::recall`]). One turn's:
    /// the window builds it for each turn, with the proof that no one else reads the document.
    pub fn set_recall(&mut self, recall: Option<RecallAsk>) {
        self.recall = recall;
    }

    /// The model and reasoning effort the next turns carry: what the person chose for this
    /// backend, `None` for its default. The conversation goes on.
    pub fn choose(&mut self, model: Option<String>, effort: Option<String>) {
        self.choice = (model, effort);
    }

    /// What the backend last said it offers to choose from: `None` until it has.
    pub fn offered(&self) -> Option<&Offered> {
        self.offered.as_ref()
    }

    /// Ask `prompt`, with `context` (the document around the selection, [`crate::Context`]):
    /// a turn starts on the conversation's thread. Refused while a turn runs, once the
    /// conversation ended, and for an empty or too long prompt.
    pub fn ask(&mut self, prompt: &str, context: Option<String>) -> Result<(), AssistError> {
        self.ask_at(prompt, context, None)
    }

    /// [`Assistant::ask`], with the assistant's copy of the document selecting `selection`
    /// first (the person's selection in the window, from anchor to focus, as
    /// [`wordcraft_doc::Document::anchor_at`] gives them there), so the turn works on what the
    /// person selected. The copy finds the paragraphs by id, which edits it hasn't had yet don't
    /// change. An ACP backend selects in the same room-port copy its agent's MCP or shell
    /// calls use, and fails the turn if the selection can no longer be found.
    pub fn ask_at(&mut self, prompt: &str, context: Option<String>, selection: Option<(Anchor, Anchor)>) -> Result<(), AssistError> {
        self.turn(prompt, context, selection, None)
    }

    /// [`Assistant::ask_at`], with this turn's permission requests answered per `policy` in
    /// place of the conversation's ([`Options::permissions`]): the window asks the person
    /// ([`PermissionPolicy::Ask`]) in the turns they asked for, and answers per their setting
    /// in those a program asked for (the control channel, MCP), which no one is there to answer.
    pub fn ask_with_policy(
        &mut self,
        prompt: &str,
        context: Option<String>,
        selection: Option<(Anchor, Anchor)>,
        policy: PermissionPolicy,
    ) -> Result<(), AssistError> {
        self.turn(prompt, context, selection, Some(policy))
    }

    fn turn(
        &mut self,
        prompt: &str,
        context: Option<String>,
        selection: Option<(Anchor, Anchor)>,
        policy: Option<PermissionPolicy>,
    ) -> Result<(), AssistError> {
        if self.ended {
            return Err(AssistError::Ended);
        }
        if self.busy {
            return Err(AssistError::Busy);
        }
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err(AssistError::Prompt("say what the assistant should do".into()));
        }
        if prompt.len() > MAX_PROMPT {
            return Err(AssistError::Prompt(format!("the prompt is longer than {} KB", MAX_PROMPT >> 10)));
        }
        let note = self.first.then(|| crate::note(&self.seat, self.info.in_process_tools));
        let (model, effort) = self.choice.clone();
        let turn = Turn {
            prompt: prompt.to_string(),
            context,
            note,
            memory: self.memory.clone(),
            selection,
            knowledge: self.knowledge.clone(),
            model,
            effort,
        };
        // Cleared before the turn is sent: a Stop from now on ends this turn, even one the
        // thread hasn't started yet.
        self.cancel.clear();
        let recall = self.recall.take().map(|r| Box::new(r.with_room(self.transcript.recall_room())));
        // From here the thread may read case notes at any moment, so the conversation counts as
        // having recalled before the event says what it found: the guard fails closed.
        self.recalled |= recall.is_some();
        if self.jobs.send(Job::Turn(Box::new(turn), policy, recall)).is_err() {
            self.ended = true;
            return Err(AssistError::Ended);
        }
        self.transcript.asked(prompt);
        self.first = false;
        self.busy = true;
        Ok(())
    }

    /// Bring the conversation's record of what it changed up to date with `session`, the document
    /// it is about, for the pane's Output.
    pub fn note_output(&mut self, session: &wordcraft_engine::Session) {
        let (seen, more) = crate::output::rows(session, &self.seat.name);
        self.transcript.note_output(seen, more, |row| crate::output::ended(&session.doc, row));
    }

    /// What the next turns carry of the sources the person attached.
    pub fn set_knowledge(&mut self, knowledge: Vec<crate::knowledge::TurnSource>) {
        self.knowledge = knowledge;
    }

    /// What the turn just asked was given of the document, for the pane's Sources.
    pub fn gave(&mut self, sent: Sent) {
        self.transcript.sent(sent);
    }

    /// Stop the turn that is running: its backend stops as soon as it can, and a permission
    /// request it waits on is answered cancelled. The conversation goes on.
    pub fn cancel(&mut self) {
        if self.busy {
            self.cancel.set();
            self.permissions.cancel();
        }
    }

    /// Take the events that came since the last poll into the transcript, at most
    /// [`MAX_EVENTS_PER_POLL`]: how many.
    pub fn poll(&mut self) -> usize {
        let mut n = 0;
        while n < MAX_EVENTS_PER_POLL {
            let Some(events) = &self.events else { break };
            match events.try_recv() {
                Ok(e) => {
                    n += 1;
                    if e.ends_turn() {
                        self.busy = false;
                    }
                    if let Event::Options(offered) = &e {
                        self.offered = Some(offered.clone());
                    }
                    if let Event::Recalled(sent) = &e
                        && !sent.items.is_empty()
                    {
                        self.recalled = true;
                    }
                    self.transcript.record(&e);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // The thread is gone: no turn will end with an event any more.
                    if self.busy {
                        self.busy = false;
                        self.transcript.record(&Event::Error { message: "the assistant stopped unexpectedly".into() });
                    }
                    self.ended = true;
                    break;
                }
            }
        }
        n
    }

    /// The person's setting for requests no one can answer (File › Options › Assistant): a
    /// turn that asks no one answers its agent's next request per it, though the turn started
    /// before it changed.
    pub fn set_unattended(&self, allow: bool) {
        self.permissions.set_unattended(allow);
    }

    /// The person answered proposal `id` ([`Entry::Proposal`]): `kept`, what was stored.
    pub fn answer_proposal(&mut self, id: u64, answer: crate::transcript::ProposalAnswer, kept: Option<&str>) {
        self.transcript.answer_proposal(id, answer, kept);
    }

    /// The permission request the turn waits on for the person's answer, if one does.
    pub fn pending_permission(&self) -> Option<&PermissionRequest> {
        let id = self.permissions.waiting()?;
        self.transcript.entries().rev().find_map(|e| match e {
            Entry::Permission { request, answer: None } if request.id == id => Some(request),
            _ => None,
        })
    }

    /// Answer permission request `id` (the one [`Assistant::pending_permission`] gives), with
    /// the option the person chose or none: whether it was waiting. An option the request
    /// didn't offer is refused.
    pub fn answer(&mut self, id: u64, answer: Answer) -> bool {
        if let Answer::Selected { option } = &answer {
            let offered = self.pending_permission().is_some_and(|r| r.id == id && r.options.iter().any(|o| &o.id == option));
            if !offered {
                return false;
            }
        }
        self.permissions.answer(id, answer)
    }

    /// End the conversation: stop the turn, take the invite back (every copy that joined with
    /// it is out of the room, so nothing more of the assistant's reaches the document), and let
    /// the thread end its backend. Pending events are dropped; the transcript is kept, and a
    /// turn that was running ends in it as cancelled, so [`Assistant::take_finished_turn`]
    /// gives it once. Never waits for the thread. Ending twice does nothing.
    pub fn end(&mut self) {
        if self.ended {
            return;
        }
        self.ended = true;
        self.cancel.set();
        self.permissions.cancel();
        // Disconnect retained conversations so a full queue cannot block backend cleanup.
        self.events = None;
        if self.busy {
            self.transcript.cut_turn();
        }
        self.busy = false;
        self.room.revoke(&self.seat.key);
        let _ = self.jobs.send(Job::End);
        log::info!("assistant {} ({:?}) left the shared document", self.info.id, self.seat.name);
    }

    /// Wait until the turn that is running ended or `timeout` has passed (any timeout, however
    /// long), taking its events ([`Assistant::poll`]): whether it ended. For tests and runs with
    /// no window; the window polls each frame instead.
    pub fn wait_idle(&mut self, timeout: Duration) -> bool {
        let until = Instant::now().checked_add(timeout);
        loop {
            self.poll();
            if !self.busy {
                return true;
            }
            if until.is_some_and(|until| Instant::now() >= until) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// The invite's key, for whoever must join with it as the assistant (an ACP agent's MCP
    /// server). Never shown or logged.
    pub fn key(&self) -> &str {
        &self.seat.key
    }
}

impl Drop for Assistant {
    fn drop(&mut self) {
        self.end();
    }
}

/// An invite for an assistant called `author` (or "Assistant") with `role`: under `author` when
/// the room hasn't had a participant or invite of that name, else the first of "author (2)",
/// "author (3)"… it hasn't ([`Room::invite_numbered`]). The name it got, and the key.
fn invite(room: &Room, author: &str, role: Role) -> Result<(String, String), AssistError> {
    let author = match author.trim() {
        "" => "Assistant",
        a => a,
    };
    Ok(room.invite_numbered(author, role)?)
}

/// The conversation's thread: its backend, its seat, and where its jobs come from and its events
/// go.
struct Work {
    backend: Box<dyn Backend>,
    room: Arc<Room>,
    seat: Seat,
    jobs: Receiver<Job>,
    events: Events,
    cancel: Cancel,
    permissions: Permissions,
    policy: PermissionPolicy,
    /// What the backend's session has been sent of the person's cases, when it keeps it.
    told: Told,
}

impl Work {
    fn run(mut self) {
        let in_process = self.backend.info().in_process_tools;
        let mut tools: Box<dyn ToolHost> = if in_process {
            match CopyTools::join(&self.room, &self.seat.key) {
                Ok(t) => Box::new(t.stopped_by(self.cancel.clone())),
                Err(e) => {
                    // A conversation ended on the frame before this thread joined finds its key
                    // revoked: the backend still hears that it ended.
                    self.fail_turns(&format!("the assistant couldn't join the shared document: {e}"));
                    self.end_backend();
                    return;
                }
            }
        } else {
            Box::new(NoTools)
        };
        let begun = guarded(|| self.backend.begin(&self.seat));
        if let Err(e) = begun {
            // What it started ends now, not when the conversation does.
            self.end_backend();
            self.fail_turns(&e);
            return;
        }
        while let Ok(job) = self.jobs.recv() {
            let (mut turn, policy, recall) = match job {
                Job::Turn(t, policy, recall) => (*t, policy.unwrap_or(self.policy), recall.map(|r| *r)),
                Job::End => break,
            };
            // Reading the person's cases is file work, so it runs here, off the frame; what it
            // finds joins the turn's sources, as data, on every backend alike. A case that can't
            // be read makes it find nothing and say so: a memory failure never stops a turn.
            let mut carried = Vec::new();
            if let Some(ask) = recall {
                let recalled = ask.run(&self.told, self.backend.retains());
                self.events.send(Event::Recalled(recalled.sent()));
                carried = recalled.fresh_ids();
                turn.knowledge.extend(recalled.into_sources());
            }
            // The person's selection, in the assistant's copy: WordCraft's own step, not a call
            // of the model's, so the pane doesn't show it. A copy that can't find it (its
            // paragraph is gone) keeps its own.
            if let Some((anchor, focus)) = turn.selection {
                let _ = tools.select(anchor, focus);
            }
            // An agent that changes the document on its own (ACP) has its calls' changes looked
            // for in the room, in the seat's name.
            let room = (!in_process).then_some((&*self.room, self.seat.name.as_str()));
            let mut ctx =
                TurnCtx { tools: tools.as_mut(), events: &self.events, cancel: &self.cancel, permissions: &self.permissions, policy, calls: 0, room };
            let backend = &mut self.backend;
            let ended = guarded(|| backend.turn(&turn, &mut ctx));
            let stop = match ended {
                Ok(stop) => {
                    // The backend's session has the items only once it took the prompt, and the
                    // session that took it is the one running now (an agent that exited was
                    // replaced in the turn).
                    if stop != StopReason::Cancelled {
                        self.told.record(carried, self.backend.retains());
                    }
                    stop
                }
                Err(message) => {
                    self.events.send(Event::Error { message });
                    StopReason::EndTurn
                }
            };
            self.events.send(Event::Done { stop });
        }
        self.end_backend();
    }

    /// Let the backend end (an ACP agent's process stops), guarded like its turns.
    fn end_backend(&mut self) {
        let backend = &mut self.backend;
        let _ = guarded(|| {
            backend.end();
            Ok(())
        });
    }

    /// Answer every turn with `why` until the conversation ends: it can't run any.
    fn fail_turns(&self, why: &str) {
        log::warn!("assistant: {why}");
        while let Ok(Job::Turn(..)) = self.jobs.recv() {
            self.events.send(Event::Error { message: why.to_string() });
            self.events.send(Event::Done { stop: StopReason::EndTurn });
        }
    }
}

/// Run `f`; a panic in it becomes an error, so a backend's bug ends its turn, not the app.
fn guarded<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(_) => {
            // The panic's own message may quote the person's text: the app's panic hook has
            // logged it already, with that text taken out.
            log::error!("assistant: the backend panicked");
            Err("the assistant stopped unexpectedly".into())
        }
    }
}
