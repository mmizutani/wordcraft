//! The ChatGPT backend against a fake Responses API on this machine: answers, tool rounds that
//! change the window's document, Stop, junk streams, each error and its retries, the round cap
//! and the history.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wordcraft_doc::{Document, StoryRef};
use wordcraft_engine::Session;
use wordcraft_engine::room::{Replica, Room};

use super::backend::Tuning;
use super::fake_server::{Part, Reply, Request, Server};
use super::tests::{Folder, Script, WAIT, eventually, expire, no_api, signed_in};
use super::{AUTHOR, ChatGpt, Endpoints, ID, USAGE_PAGE};
use crate::{Assistant, Backend, Entry, Options, StopReason, ToolStatus};

/// The window: its session, showing `text`, shared under the name Minoru.
struct Window {
    session: Session,
    room: Arc<Room>,
    own: Replica,
}

impl Window {
    fn new(text: &str) -> Window {
        let mut session = Session::new(Document::from_text(text));
        session.author = "Minoru".into();
        let (room, own) = Room::host(&mut session, "Minoru").unwrap();
        Window { session, room, own }
    }

    /// Frames until the window has every change the room took.
    fn settle(&mut self) {
        for _ in 0..200 {
            let _ = self.own.lend(&mut self.session, Replica::sync);
            if !self.own.has_unconfirmed() && self.own.received() == self.room.version() {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("the window doesn't catch up");
    }

    /// The text with every tracked change accepted.
    fn accepted(&self) -> String {
        let mut s = Session::new(self.session.doc.clone());
        s.run("review.acceptAll", &json!({})).unwrap();
        s.doc.plain_text(StoryRef::Body)
    }

    fn authors(&self) -> Vec<String> {
        self.session.doc.revisions.iter().map(|r| r.author.clone()).collect()
    }
}

/// A conversation with ChatGPT, signed in at a fake server.
struct Chat {
    window: Window,
    assistant: Assistant,
    server: Server,
    folder: Folder,
    script: Arc<Script>,
}

fn quick() -> Tuning {
    Tuning { max_rounds: 25, backoff: Duration::from_millis(10), max_wait: Duration::from_secs(2), ..Tuning::default() }
}

/// A conversation in a window showing `text`, whose `/v1/responses` answers come from `api`.
fn chat(text: &str, api: impl Fn(&Request, usize) -> Reply + Send + Sync + 'static) -> Chat {
    chat_with(text, quick(), api, |e| e)
}

fn chat_with(
    text: &str,
    tuning: Tuning,
    api: impl Fn(&Request, usize) -> Reply + Send + Sync + 'static,
    endpoints: impl FnOnce(Endpoints) -> Endpoints,
) -> Chat {
    let (folder, script, server, signed) = signed_in(api);
    let window = Window::new(text);
    let backend = ChatGpt::with_endpoints(folder.path(), endpoints(signed)).tuned(tuning);
    let assistant = Assistant::start(&window.room, Box::new(backend), Options { document: "Notes".into(), ..Options::default() }).unwrap();
    Chat { window, assistant, server, folder, script }
}

/// `/v1/responses` answers `replies` in order, and the last one again after them.
fn replies(replies: Vec<Reply>) -> impl Fn(&Request, usize) -> Reply + Send + Sync + 'static {
    move |r, n| match replies.get(n).or(replies.last()) {
        Some(reply) if r.path == "/v1/responses" => reply.clone(),
        _ => Reply::json(404, &json!({"detail": "Not Found"})),
    }
}

impl Chat {
    fn ask(&mut self, prompt: &str) {
        self.assistant.ask(prompt, None).unwrap();
        assert!(self.assistant.wait_idle(WAIT), "the turn didn't end");
    }

    /// The bodies of the requests to `/v1/responses`.
    fn bodies(&self) -> Vec<Value> {
        self.server.to("/v1/responses").iter().map(Request::json).collect()
    }

    fn answer(&self) -> Option<String> {
        self.assistant.transcript().last_answer()
    }

    /// Frames until the turn shows some of its answer.
    fn wait_for_answer(&mut self) {
        let until = Instant::now() + WAIT;
        while self.answer().is_none() {
            assert!(Instant::now() < until, "nothing streamed");
            self.assistant.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The last turn's error.
    fn error(&self) -> Option<String> {
        self.assistant.transcript().entries().rev().take_while(|e| !matches!(e, Entry::Asked { .. })).find_map(|e| match e {
            Entry::Error { message } => Some(message.clone()),
            _ => None,
        })
    }

    /// How the last turn stopped, when it stopped other than by finishing.
    fn stopped(&self) -> Option<StopReason> {
        self.assistant.transcript().entries().rev().take_while(|e| !matches!(e, Entry::Asked { .. })).find_map(|e| match e {
            Entry::Stopped { stop } => Some(*stop),
            _ => None,
        })
    }

    fn tool_rows(&self) -> Vec<(String, ToolStatus)> {
        self.assistant
            .transcript()
            .entries()
            .filter_map(|e| match e {
                Entry::Tool(c) => Some((c.title.clone(), c.status)),
                _ => None,
            })
            .collect()
    }
}

fn created() -> Value {
    json!({"type": "response.created", "response": {"id": "resp_1", "object": "response", "model": "gpt-test-2026-01-01", "status": "in_progress"}})
}

fn delta(text: &str) -> Value {
    json!({"type": "response.output_text.delta", "item_id": "msg_1", "output_index": 0, "content_index": 0, "delta": text})
}

fn message(text: &str) -> Value {
    json!({"type": "response.output_item.done", "output_index": 0, "item": {
        "id": "msg_1", "type": "message", "role": "assistant", "status": "completed", "phase": "final_answer",
        "content": [{"type": "output_text", "text": text, "annotations": []}],
    }})
}

fn call_item(id: &str, name: &str, args: &Value) -> Value {
    json!({"id": format!("fc_{id}"), "type": "function_call", "status": "completed", "call_id": format!("call_{id}"),
           "name": name, "namespace": "wordcraft", "arguments": args.to_string()})
}

fn call(id: &str, name: &str, args: &Value) -> Value {
    json!({"type": "response.output_item.done", "item": call_item(id, name, args)})
}

fn reasoning(id: &str) -> Value {
    json!({"type": "response.output_item.done", "item": {"id": format!("rs_{id}"), "type": "reasoning", "summary": [], "encrypted_content": "gAAAAB-encrypted"}})
}

fn thought(text: &str) -> Value {
    json!({"type": "response.reasoning_summary_text.delta", "item_id": "rs_1", "summary_index": 0, "delta": text})
}

fn completed() -> Value {
    json!({"type": "response.completed", "response": {"id": "resp_1", "status": "completed", "usage": {"input_tokens": 10, "output_tokens": 5}}})
}

/// An answer that says `text`.
fn says(text: &str) -> Reply {
    Reply::sse(&[created(), delta(text), message(text), completed()])
}

/// An answer that calls `name` with `args`.
fn calls(id: &str, name: &str, args: &Value) -> Reply {
    Reply::sse(&[created(), call(id, name, args), completed()])
}

/// An error answer.
fn refusal(status: u16, code: &str, message: &str) -> Reply {
    Reply::json(status, &json!({"error": {"type": "invalid_request_error", "code": code, "message": message, "param": null}}))
}

fn event(e: &Value) -> Part {
    Part::Bytes(format!("data: {e}\n\n").into_bytes())
}

fn user(text: &str) -> Value {
    json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]})
}

fn assistant(text: &str) -> Value {
    json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]})
}

#[test]
fn it_is_chatgpt_with_its_tools_in_this_process() {
    let folder = Folder::new();
    let info = ChatGpt::new(folder.path()).info();
    assert_eq!((info.id.as_str(), info.label.as_str(), info.author.as_str(), info.in_process_tools), (ID, AUTHOR, "ChatGPT", true));
    // Made by its id once the person is signed in with plan usage.
    let (folder, _script, _server, _endpoints) = signed_in(no_api);
    let host = crate::Host { settings: Some(folder.path().to_path_buf()), ..crate::Host::default() };
    assert_eq!(crate::offers::make(ID, &host).unwrap().info().author, "ChatGPT");
}

#[test]
fn a_text_answer_streams_into_the_pane_and_names_the_model() {
    let answer = Reply::sse(&[created(), thought("Reading the request."), delta("Hello"), delta(" there."), message("Hello there."), completed()]);
    let mut c = chat("one two three", replies(vec![answer, says("Again.")]));
    c.assistant.ask("Say hello", Some("Selected text: two".into())).unwrap();
    assert!(c.assistant.wait_idle(WAIT));
    assert_eq!(c.answer().as_deref(), Some("Hello there."));
    assert_eq!((c.error(), c.stopped()), (None, None));
    let t = c.assistant.transcript();
    assert!(t.entries().any(|e| matches!(e, Entry::Model { name, .. } if name == "GPT Test")), "{t:?}");
    assert!(t.entries().any(|e| matches!(e, Entry::Thought { text } if text == "Reading the request.")));
    let request = &c.server.to("/v1/responses")[0];
    assert_eq!(request.header("authorization"), Some("Bearer access-0"));
    assert_eq!(request.header("content-type"), Some("application/json"));
    assert_eq!(request.header("accept"), Some("text/event-stream"));
    let body = request.json();
    assert_eq!(
        body["input"],
        json!([{"type": "message", "role": "user", "content": [
            {"type": "input_text", "text": "The document as the person sees it now:\n\nSelected text: two"},
            {"type": "input_text", "text": "Say hello"},
        ]}])
    );
    // A second turn: the first one in the history, the note's instructions again, the model
    // list read once.
    c.ask("Again");
    assert_eq!(c.answer().as_deref(), Some("Again."));
    let second = &c.bodies()[1];
    assert_eq!(second["instructions"], body["instructions"]);
    assert_eq!(second["input"], json!([user("Say hello"), assistant("Hello there."), user("Again")]));
    assert_eq!(c.server.to("/v1/models").len(), 1);
}

#[test]
fn a_request_is_what_the_responses_api_takes_from_a_chatgpt_plan() {
    let mut c = chat("one two three", replies(vec![says("Hi.")]));
    c.ask("Hello");
    let body = &c.bodies()[0];
    assert_eq!(body["model"], "gpt-test", "without gpt-6-luna, the first model the account's list shows");
    assert_eq!((&body["store"], &body["stream"]), (&json!(false), &json!(true)));
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    let instructions = body["instructions"].as_str().unwrap();
    assert!(instructions.contains("You are ChatGPT") && instructions.contains("suggester") && instructions.contains("\"Notes\""), "{instructions}");
    let omitted = [
        "background",
        "conversation",
        "max_output_tokens",
        "max_tool_calls",
        "metadata",
        "moderation",
        "multi_agent",
        "prompt",
        "prompt_cache_retention",
        "safety_identifier",
        "temperature",
        "top_logprobs",
        "top_p",
        "truncation",
        "user",
        "previous_response_id",
        "service_tier",
    ];
    for field in omitted {
        assert!(body.get(field).is_none(), "{field}");
    }
    let tools = body["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!((tools[0]["type"].as_str(), tools[0]["name"].as_str()), (Some("namespace"), Some("wordcraft")));
    let functions = tools[0]["tools"].as_array().unwrap();
    let names: Vec<&str> = functions.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, crate::tools::TOOLS);
    assert!(functions.iter().all(|t| t["type"] == "function" && t["strict"] == false && t["parameters"].is_object()));
    // The person's choice of model.
    c.assistant.choose(Some("gpt-test-mini".into()), None);
    c.ask("Again");
    assert_eq!(c.bodies()[1]["model"], "gpt-test-mini");
}

#[test]
fn tool_calls_make_tracked_changes_under_chatgpt_that_the_window_sees() {
    let mut c = chat(
        "one two three",
        replies(vec![
            Reply::sse(&[created(), reasoning("1"), call("1", "select_text", &json!({"text": "two"})), completed()]),
            Reply::sse(&[created(), call("2", "wordcraft.type_text", &json!({"text": "2"})), completed()]),
            says("Done: “two” is now “2”."),
        ]),
    );
    c.ask("Use digits");
    c.window.settle();
    assert_eq!(c.assistant.name(), "ChatGPT");
    assert_eq!(c.window.authors(), ["ChatGPT", "ChatGPT"], "a suggestion in ChatGPT's name");
    assert_eq!(c.window.accepted(), "one 2 three");
    assert_eq!(c.tool_rows(), [("select_text".to_string(), ToolStatus::Completed), ("type_text".to_string(), ToolStatus::Completed)]);
    assert_eq!(c.answer().as_deref(), Some("Done: “two” is now “2”."));
    assert_eq!((c.error(), c.stopped()), (None, None));
    // Each request has the answers' items as they came, and the tools' outputs.
    let bodies = c.bodies();
    assert_eq!(bodies.len(), 3);
    let input = bodies[1]["input"].as_array().unwrap();
    assert_eq!(input.len(), 4, "{input:?}");
    assert_eq!(input[1]["encrypted_content"], "gAAAAB-encrypted", "the reasoning, as received");
    assert_eq!(input[2], call_item("1", "select_text", &json!({"text": "two"})));
    assert_eq!((input[3]["type"].as_str(), input[3]["call_id"].as_str()), (Some("function_call_output"), Some("call_1")));
    assert!(input[3]["output"].is_string());
    let input = bodies[2]["input"].as_array().unwrap();
    assert_eq!(input.len(), 6);
    assert_eq!(input[5]["call_id"], "call_2");
    assert!(!input[5]["output"].as_str().unwrap().starts_with("Error"), "{}", input[5]["output"]);
}

#[test]
fn junk_tool_calls_get_error_outputs_and_the_history_stays_whole() {
    let mut answer = vec![created(), call("1", "format_disk", &json!({})), call("2", "get_text", &json!({}))];
    answer.push(json!({"type": "response.output_item.done", "item": {"id": "fc_3", "type": "function_call", "call_id": "call_3", "name": "get_text", "arguments": "{not json"}}));
    answer.push(json!({"type": "response.output_item.done", "item": {"id": "fc_4", "type": "function_call", "name": "get_text", "arguments": "{}"}}));
    answer.extend((5..45).map(|i| call(&i.to_string(), "get_text", &json!({}))));
    answer.push(json!({"type": "response.output_item.done", "item": "junk"}));
    answer.push(completed());
    let mut c = chat("one two three", replies(vec![Reply::sse(&answer), says("Sorry.")]));
    c.ask("Read it");
    assert_eq!(c.answer().as_deref(), Some("Sorry."));
    let input = c.bodies()[1]["input"].as_array().unwrap().clone();
    let outputs: Vec<(String, String)> = input
        .iter()
        .filter(|i| i["type"] == "function_call_output")
        .map(|i| (i["call_id"].as_str().unwrap().to_string(), i["output"].as_str().unwrap().to_string()))
        .collect();
    // Every call with an id has an output; the one without an id is left out of the history.
    let calls = input.iter().filter(|i| i["type"] == "function_call").count();
    assert_eq!((calls, outputs.len()), (43, 43));
    assert!(outputs[0].1.starts_with("Error: unknown tool"), "{:?}", outputs[0]);
    assert!(outputs[1].1.contains("one two three"));
    assert!(outputs[2].1.starts_with("Error: the arguments aren't JSON"), "{:?}", outputs[2]);
    assert!(outputs[32].1.starts_with("Not run: at most 32"), "{:?}", outputs[32]);
    assert_eq!(c.tool_rows().len(), 31, "the calls that ran");
}

#[test]
fn stop_ends_the_turn_mid_stream_and_before_the_answer_starts_without_waiting_for_the_server() {
    // The server holds the stream open for longer than the test waits (30 s): only Stop can end
    // the turn in time.
    let held = Reply::stream(vec![event(&created()), event(&delta("Partial")), Part::Hold]);
    let mut c = chat("one two three", replies(vec![held, says("After.")]));
    c.assistant.ask("Write a lot", None).unwrap();
    c.wait_for_answer();
    c.assistant.cancel();
    assert!(c.assistant.wait_idle(WAIT), "the stop waited for the server");
    assert_eq!(c.stopped(), Some(StopReason::Cancelled));
    assert_eq!(c.answer().as_deref(), Some("Partial"));
    // The conversation goes on.
    c.ask("Go on");
    assert_eq!(c.answer().as_deref(), Some("After."));
    // A server that doesn't answer at all (until the test lets it, once the turn has ended).
    let release = Arc::new(AtomicBool::new(false));
    let late = release.clone();
    let mut c = chat("one", move |_, _| {
        let until = Instant::now() + Duration::from_secs(30);
        while !late.load(Ordering::Relaxed) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        says("Too late.")
    });
    c.assistant.ask("Hello", None).unwrap();
    eventually("the request is sent", || c.server.to("/v1/responses").len() == 1);
    c.assistant.cancel();
    assert!(c.assistant.wait_idle(WAIT), "the stop waited for the server");
    assert_eq!(c.stopped(), Some(StopReason::Cancelled));
    release.store(true, Ordering::Relaxed);
}

#[test]
fn a_stop_while_the_token_is_renewed_sends_nothing_even_once_the_next_turn_starts() {
    let mut c = chat("x", replies(vec![says("After.")]));
    // The list is read, so the next turn's token is taken for its request.
    c.ask("First");
    expire(c.folder.path());
    *c.script.slow.lock().unwrap() = Duration::from_secs(1);
    c.assistant.ask("Hello", None).unwrap();
    eventually("the token is being renewed", || c.server.to("/oauth/token").len() == 2);
    c.assistant.cancel();
    assert!(c.assistant.wait_idle(WAIT));
    assert_eq!(c.stopped(), Some(StopReason::Cancelled));
    // Asked again at once: the new turn starts (and clears the stop) while the stopped one still
    // renews the token. Only the new turn's request goes out.
    *c.script.slow.lock().unwrap() = Duration::ZERO;
    c.ask("Go on");
    assert_eq!(c.answer().as_deref(), Some("After."));
    assert_eq!(c.bodies().len(), 2, "{:?}", c.bodies());
    assert_eq!(bodies_input(&c, 1), [user("First"), assistant("After."), user("Go on")]);
}

#[test]
fn stop_lets_go_of_a_stream_that_only_trickles_comments() {
    let keep_alive = Part::Trickle(b": keep-alive\n".to_vec(), Duration::from_millis(20));
    let trickle = Reply::stream(vec![event(&created()), event(&delta("Partial")), keep_alive]);
    let mut c = chat("x", replies(vec![trickle]));
    c.assistant.ask("Write", None).unwrap();
    c.wait_for_answer();
    c.assistant.cancel();
    assert!(c.assistant.wait_idle(WAIT));
    assert_eq!(c.stopped(), Some(StopReason::Cancelled), "{:?}", c.error());
    // The request thread lets go of the connection: the server's next keep-alive can't be sent.
    eventually("the stream is let go of", || c.server.closed() == 1);
}

#[test]
fn a_stream_that_goes_quiet_or_runs_too_long_breaks_off() {
    // Only keep-alives after the start, for longer than an answer may go without an event
    // (nothing was shown, so it is tried once more).
    let quiet = Reply::stream(vec![event(&created()), Part::Trickle(b": keep-alive\n".to_vec(), Duration::from_millis(20))]);
    let tuning = Tuning { max_quiet: Duration::from_millis(300), ..quick() };
    let mut c = chat_with("x", tuning, replies(vec![quiet]), |e| e);
    c.ask("Hi");
    let e = c.error().unwrap();
    assert!(e.contains("cut off") && e.contains("sent nothing for 300 ms"), "{e}");
    assert_eq!(c.bodies().len(), 2);
    // Text that keeps trickling in for longer than an answer may take: what came stays.
    let endless = Reply::stream(vec![event(&created()), Part::Trickle(format!("data: {}\n\n", delta("a")).into_bytes(), Duration::from_millis(20))]);
    let tuning = Tuning { max_stream: Duration::from_secs(1), ..quick() };
    let mut c = chat_with("x", tuning, replies(vec![endless]), |e| e);
    c.ask("Hi");
    let e = c.error().unwrap();
    assert!(e.contains("cut off") && e.contains("took longer than 1 s"), "{e}");
    assert!(c.answer().is_some_and(|a| !a.is_empty() && a.chars().all(|ch| ch == 'a')), "{:?}", c.answer());
    assert_eq!(c.bodies().len(), 1);
}

#[test]
fn junk_huge_and_cut_streams_end_the_turn_with_a_plain_error() {
    // Events that aren't JSON: tried again once (nothing was shown), then said.
    let junk: Vec<Part> = (0..40).map(|i| Part::Bytes(format!("data: not json {i}\n\n").into_bytes())).collect();
    let mut c = chat("x", replies(vec![Reply::stream(junk)]));
    c.ask("Hi");
    let e = c.error().unwrap();
    assert!(e.contains("cut off") && e.contains("JSON"), "{e}");
    assert_eq!(c.bodies().len(), 2);
    // One huge event.
    let huge = format!("data: {}\n\n", json!({"type": "response.output_text.delta", "delta": "x".repeat(5 << 20)}));
    let mut c = chat("x", replies(vec![Reply::stream(vec![event(&created()), Part::Bytes(huge.into_bytes())])]));
    c.ask("Hi");
    assert!(c.error().unwrap().contains("longer than"), "{:?}", c.error());
    // Cut after some text: not tried again (it would say it twice), and the text stays.
    let mut c = chat("x", replies(vec![Reply::stream(vec![event(&created()), event(&delta("Half")), Part::Cut])]));
    c.ask("Hi");
    assert!(c.error().unwrap().contains("cut off"), "{:?}", c.error());
    assert_eq!((c.bodies().len(), c.answer().as_deref()), (1, Some("Half")));
    // Ended without saying it was complete.
    let mut c = chat("x", replies(vec![Reply::sse(&[created(), delta("Hm")])]));
    c.ask("Hi");
    assert!(c.error().unwrap().contains("ended before it was finished"), "{:?}", c.error());
}

#[test]
fn comments_event_lines_crlf_split_chunks_and_done_are_read_as_a_stream() {
    let odd = Reply::stream(vec![
        Part::Bytes(b": keep-alive\r\n\r\n".to_vec()),
        Part::Bytes(b"event: response.created\r\ndata: ".to_vec()),
        Part::Bytes(format!("{}\r\n\r\n", created()).into_bytes()),
        Part::Bytes(format!("data: {}\n\n", delta("Fi")).into_bytes()),
        Part::Wait(Duration::from_millis(20)),
        Part::Bytes(format!("id: 7\nretry: 100\ndata: {}\n\n", delta("ne")).into_bytes()),
        Part::Bytes(b"data: {\"type\":\n".to_vec()),
        Part::Bytes(b"data: \"response.completed\"}\n\n".to_vec()),
        Part::Bytes(b"data: [DONE]\n\n".to_vec()),
    ]);
    let mut c = chat("x", replies(vec![odd]));
    c.ask("Hi");
    assert_eq!((c.answer().as_deref(), c.error()), (Some("Fine"), None));
    // An answer whose text isn't streamed in pieces: shown whole.
    let mut c = chat("x", replies(vec![Reply::sse(&[created(), message("All at once."), completed()])]));
    c.ask("Hi");
    assert_eq!((c.answer().as_deref(), c.error()), (Some("All at once."), None));
    // An answer whose items come only with its completion.
    // (Its call's arguments an object rather than JSON text.)
    let mut item = call_item("1", "select_text", &json!({}));
    item["arguments"] = json!({"text": "x"});
    let late = json!({"type": "response.completed", "response": {"status": "completed", "output": [item]}});
    let mut c = chat("x", replies(vec![Reply::sse(&[created(), late]), says("Read.")]));
    c.ask("Read");
    assert_eq!(c.tool_rows(), [("select_text".to_string(), ToolStatus::Completed)]);
    assert_eq!(c.answer().as_deref(), Some("Read."));
}

/// Ask once, with `/v1/responses` answering `answers`: the conversation.
fn asked(answers: Vec<Reply>) -> Chat {
    let mut c = chat("one two three", replies(answers));
    c.ask("Hello");
    c
}

#[test]
fn usage_limits_eligibility_and_refusals_are_said_plainly_and_not_retried() {
    let c = asked(vec![refusal(429, "subscription_sharing_usage_limit_exceeded", "limit")]);
    let e = c.error().unwrap();
    assert!(e.contains("usage limit") && e.contains(USAGE_PAGE), "{e}");
    assert_eq!(c.bodies().len(), 1);
    let c = asked(vec![refusal(403, "subscription_sharing_user_not_eligible", "no")]);
    assert!(c.error().unwrap().contains("Plus or Pro"));
    assert_eq!(c.bodies().len(), 1);
    assert_eq!(c.server.to("/oauth/token").len(), 1, "no sign-in loop");
    let c = asked(vec![refusal(403, "subscription_sharing_route_not_supported", "no")]);
    assert!(c.error().unwrap().contains("doesn't take this kind of request"));
    let c = asked(vec![Reply::json(403, &json!({"detail": "Unsupported country, region, or territory"}))]);
    assert!(c.error().unwrap().contains("region"), "{:?}", c.error());
    let c = asked(vec![refusal(400, "model_not_found", "The model `gpt-test` does not exist.")]);
    assert!(c.error().unwrap().contains("does not exist"), "{:?}", c.error());
    assert_eq!(c.bodies().len(), 1);
    let c = asked(vec![refusal(403, "chatpass_v2_scope_not_authorized", "no")]);
    assert!(c.error().unwrap().contains("allow it"));
    // In the stream.
    let c = asked(vec![Reply::sse(&[
        created(),
        json!({"type": "error", "code": "subscription_sharing_usage_limit_exceeded", "message": "limit", "param": null}),
    ])]);
    assert!(c.error().unwrap().contains(USAGE_PAGE));
    assert_eq!(c.bodies().len(), 1);
    // A refusal by the model.
    let refused = json!({"type": "response.output_item.done", "item": {"id": "msg_1", "type": "message", "role": "assistant",
        "content": [{"type": "refusal", "refusal": "I can't help with that."}]}});
    let c =
        asked(vec![Reply::sse(&[created(), json!({"type": "response.refusal.delta", "delta": "I can't help with that."}), refused, completed()])]);
    assert_eq!((c.stopped(), c.answer().as_deref()), (Some(StopReason::Refusal), Some("I can't help with that.")));
}

#[test]
fn a_busy_plan_or_server_is_tried_again_with_backoff_then_said() {
    let busy = || refusal(503, "subscription_sharing_usage_unavailable", "busy");
    let c = asked(vec![busy(), busy(), busy(), says("Never.")]);
    assert!(c.error().unwrap().contains("right now"), "{:?}", c.error());
    assert_eq!(c.bodies().len(), 3);
    let c = asked(vec![busy(), says("Here.")]);
    assert_eq!((c.answer().as_deref(), c.bodies().len()), (Some("Here."), 2));
    let c = asked(vec![Reply::bytes(500, "text/html", b"<html>oops</html>".to_vec()), says("Here.")]);
    assert_eq!((c.answer().as_deref(), c.bodies().len()), (Some("Here."), 2));
    let c = asked(vec![Reply::bytes(502, "text/html", b"<html>bad gateway</html>".to_vec())]);
    assert!(c.error().unwrap().contains("HTTP 502"), "{:?}", c.error());
    assert_eq!(c.bodies().len(), 3);
    // Too many requests: after what Retry-After says, when it's short; else said at once.
    let started = Instant::now();
    let c = asked(vec![refusal(429, "rate_limit_exceeded", "slow down").header("Retry-After", "1"), says("Here.")]);
    assert_eq!((c.answer().as_deref(), c.bodies().len()), (Some("Here."), 2));
    assert!(started.elapsed() >= Duration::from_secs(1));
    let c = asked(vec![refusal(429, "rate_limit_exceeded", "slow down").header("Retry-After", "3600"), says("Here.")]);
    assert!(c.error().unwrap().contains("too many requests"), "{:?}", c.error());
    assert_eq!(c.bodies().len(), 1);
    // Failed in the stream before anything was shown: once more.
    let failed = json!({"type": "response.failed", "response": {"status": "failed", "error": {"code": "server_error", "message": "oops"}}});
    let c = asked(vec![Reply::sse(&[created(), failed]), says("Here.")]);
    assert_eq!((c.answer().as_deref(), c.bodies().len()), (Some("Here."), 2));
}

#[test]
fn an_unsupported_field_is_left_out_from_then_on() {
    let unsupported = Reply::json(
        400,
        &json!({"error": {"code": "subscription_sharing_unsupported_capability", "message": "no summaries", "param": "reasoning.summary"}}),
    );
    let mut c = chat("x", replies(vec![unsupported.clone(), says("Here."), says("Again.")]));
    c.ask("Hello");
    assert_eq!(c.answer().as_deref(), Some("Here."));
    c.ask("Again");
    let bodies = c.bodies();
    assert_eq!(bodies.len(), 3);
    assert_eq!(bodies[0]["reasoning"], json!({"effort": "medium", "summary": "auto"}));
    assert_eq!(bodies[1]["reasoning"], json!({"effort": "medium"}), "a refused summary doesn't lose the effort");
    assert_eq!(bodies[2]["reasoning"], json!({"effort": "medium"}));
    assert!(bodies[2].get("include").is_some(), "only what was refused is left out");
    // Refused again without it: said, not looped.
    let c = asked(vec![unsupported.clone(), unsupported]);
    assert!(c.error().unwrap().contains("no summaries"), "{:?}", c.error());
    assert_eq!(c.bodies().len(), 2);
}

#[test]
fn a_turned_away_token_is_refreshed_once_and_then_the_person_signs_in_again() {
    let unauthorized = || refusal(401, "subscription_sharing_invalid_user", "who are you");
    let c = asked(vec![unauthorized(), says("Here.")]);
    assert_eq!(c.answer().as_deref(), Some("Here."));
    let sent = c.server.to("/v1/responses");
    assert_eq!((sent[0].header("authorization"), sent[1].header("authorization")), (Some("Bearer access-0"), Some("Bearer access-1")));
    assert_eq!(c.server.to("/oauth/token").len(), 2, "the exchange, then one refresh");
    let c = asked(vec![unauthorized(), unauthorized(), says("Never.")]);
    assert!(c.error().unwrap().contains("didn't accept your sign-in"), "{:?}", c.error());
    assert_eq!((c.bodies().len(), c.server.to("/oauth/token").len()), (2, 2));
    // The refresh is refused for good: signed out, and the person is told to sign in.
    let mut c = chat("x", replies(vec![Reply::json(401, &json!({"detail": "Unauthorized"})), says("Never.")]));
    *c.script.refuse.lock().unwrap() = Some((400, json!({"error": "invalid_grant"})));
    c.ask("Hello");
    assert!(c.error().unwrap().contains("sign in again"), "{:?}", c.error());
    assert_eq!(super::account(c.folder.path()), None);
    c.ask("Hello?");
    assert!(c.error().unwrap().starts_with("Sign in with ChatGPT first"), "{:?}", c.error());
}

#[test]
fn a_models_list_that_turns_the_token_away_doesnt_end_the_sign_in() {
    let mut c = chat("x", replies(vec![says("Here.")]));
    c.script.turned_away.lock().unwrap().push("access-0".into());
    c.ask("Hello");
    assert_eq!(c.answer().as_deref(), Some("Here."), "{:?}", c.error());
    assert_eq!(c.server.to("/oauth/token").len(), 2, "the exchange, then one refresh");
    assert_eq!(c.server.to("/v1/responses")[0].header("authorization"), Some("Bearer access-1"));
    assert!(super::account(c.folder.path()).is_some());
}

#[test]
fn errors_never_show_a_token_and_a_closed_server_is_said_plainly() {
    let leaky = refusal(400, "invalid_request_error", &format!("bad token access-0 {}", "k".repeat(60)));
    let c = asked(vec![leaky]);
    let e = c.error().unwrap();
    assert!(!e.contains(&"k".repeat(60)) && !e.contains("access-0") && !e.contains("Bearer"), "{e}");
    // Nobody at the API's address: tried again once, then said.
    let mut c = chat_with("x", quick(), no_api, |e| Endpoints { api: "http://127.0.0.1:1/v1".into(), ..e });
    c.ask("Hello");
    assert!(c.error().unwrap().starts_with("ChatGPT can't be reached"), "{:?}", c.error());
    // Signed in without plan usage, or not signed in.
    let mut c = chat("x", replies(vec![says("Never.")]));
    let mut saved = super::store::read_credentials(c.folder.path()).unwrap();
    saved.scopes.retain(|s| s != super::PLAN_SCOPE);
    super::store::write_credentials(c.folder.path(), &saved).unwrap();
    c.ask("Hello");
    assert!(c.error().unwrap().contains("plan usage wasn't allowed"), "{:?}", c.error());
    assert!(c.bodies().is_empty());
    let folder = Folder::new();
    let window = Window::new("x");
    let backend = ChatGpt::with_endpoints(folder.path(), Endpoints::local("http://127.0.0.1:1")).tuned(quick());
    let mut a = Assistant::start(&window.room, Box::new(backend), Options::default()).unwrap();
    a.ask("Hello", None).unwrap();
    assert!(a.wait_idle(WAIT));
    let error = a.transcript().entries().find_map(|e| if let Entry::Error { message } = e { Some(message.clone()) } else { None });
    assert!(error.as_deref().is_some_and(|e| e.contains("Sign in with ChatGPT first")), "{error:?}");
}

#[test]
fn a_model_that_keeps_calling_tools_stops_at_the_round_cap() {
    let tuning = Tuning { max_rounds: 3, ..quick() };
    let mut c = chat_with("one two three", tuning, |_, n| calls(&n.to_string(), "get_text", &json!({})), |e| e);
    c.ask("Loop");
    assert_eq!(c.stopped(), Some(StopReason::MaxTurnRequests));
    assert_eq!(c.bodies().len(), 3);
    assert_eq!(c.tool_rows().len(), 2, "the last answer's call isn't run: nobody would read its output");
    assert_eq!(super::MAX_ROUNDS, 25);
}

#[test]
fn earlier_tool_outputs_are_cut_and_earlier_turns_kept_as_words() {
    let text = "word ".repeat(2000);
    let mut c = chat(
        &text,
        replies(vec![
            calls("1", "get_text", &json!({})),
            calls("2", "get_text", &json!({})),
            calls("3", "get_text", &json!({})),
            says("Read it."),
            says("Second."),
        ]),
    );
    c.ask("Read the document");
    assert_eq!(c.answer().as_deref(), Some("Read it."));
    let outputs = |body: &Value| -> Vec<String> {
        body["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["type"] == "function_call_output")
            .map(|i| i["output"].as_str().unwrap().to_string())
            .collect()
    };
    let bodies = c.bodies();
    let last = outputs(&bodies[3]);
    assert_eq!(last.len(), 3);
    assert!(last[0].contains("left out") && last[1].contains("left out"), "{:?}", &last[..2]);
    assert!(last[0].len() < 2 << 10);
    assert!(!last[2].contains("left out") && last[2].len() > 9000, "the latest output is whole: {}", last[2].len());
    let before = outputs(&bodies[2]);
    assert!(before[0].contains("left out") && !before[1].contains("left out"));
    // The next turn has the first one as words: what was asked, what was said, the tools used.
    c.ask("Second?");
    let input = bodies_input(&c, 4);
    assert_eq!(input, [user("Read the document"), assistant("Read it.\n\n[Tools used in this turn: get_text]"), user("Second?")]);
}

fn bodies_input(c: &Chat, n: usize) -> Vec<Value> {
    c.bodies()[n]["input"].as_array().unwrap().clone()
}

/// The two models the issue's list has, in its order: `gpt-6.1-sol` first.
fn real_list() -> Reply {
    Reply::json(
        200,
        &json!({"models": [
            {"slug": "gpt-6.1-sol", "display_name": "GPT-6.1-Sol", "default_reasoning_level": "low", "visibility": "list",
             "supported_reasoning_levels": [{"effort": "low"}, {"effort": "medium"}, {"effort": "high"}, {"effort": "ultra"}]},
            {"slug": "gpt-6-luna", "display_name": "GPT-6-Luna", "default_reasoning_level": "medium", "visibility": "list",
             "supported_reasoning_levels": [{"effort": "low"}, {"effort": "medium"}, {"effort": "high"}]},
        ]}),
    )
}

/// A conversation whose account's list answers `list`, and its `/v1/responses` answers `replies`.
fn listing(list: Reply, answers: Vec<Reply>) -> Chat {
    let c = chat("one two three", replies(answers));
    *c.script.models.lock().unwrap() = Some(list);
    c
}

/// The last turn's notes.
fn notes(c: &Chat) -> Vec<String> {
    c.assistant
        .transcript()
        .entries()
        .rev()
        .take_while(|e| !matches!(e, Entry::Asked { .. }))
        .filter_map(|e| match e {
            Entry::Note { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// The last turn's model line.
fn model_line(c: &Chat) -> Option<(String, Option<String>, bool)> {
    c.assistant.transcript().entries().rev().take_while(|e| !matches!(e, Entry::Asked { .. })).find_map(|e| match e {
        Entry::Model { name, effort, asked } => Some((name.clone(), effort.clone(), *asked)),
        _ => None,
    })
}

/// An answer that says what ran.
fn says_it_ran(model: &str, effort: Option<&str>) -> Reply {
    let created = json!({"type": "response.created", "response": {"model": model, "reasoning": {"effort": effort, "summary": "auto"}}});
    Reply::sse(&[created, delta("Hi."), message("Hi."), completed()])
}

#[test]
fn with_nothing_chosen_the_request_asks_for_gpt_6_luna_at_medium_with_a_summary() {
    let mut c = listing(real_list(), vec![says("Hi.")]);
    c.ask("Hello");
    let body = &c.bodies()[0];
    assert_eq!(body["model"], "gpt-6-luna");
    assert_eq!(body["reasoning"]["effort"], "medium");
    assert_eq!(body["reasoning"]["summary"], "auto");
}

#[test]
fn without_gpt_6_luna_the_first_runs_at_medium_or_else_at_its_default() {
    let at = |levels: Value, default: &str| {
        let list =
            json!({"models": [{"slug": "gpt-a", "supported_reasoning_levels": levels, "default_reasoning_level": default}, {"slug": "gpt-b"}]});
        let mut c = listing(Reply::json(200, &list), vec![says("Hi.")]);
        c.ask("Hello");
        let body = c.bodies()[0].clone();
        assert_eq!(body["model"], "gpt-a");
        body["reasoning"]["effort"].clone()
    };
    assert_eq!(at(json!(["low", "medium"]), "low"), "medium");
    assert_eq!(at(json!(["low", "high"]), "high"), "high");
}

#[test]
fn a_list_with_no_levels_sends_medium() {
    let mut c = chat("x", replies(vec![says("Hi.")]));
    c.ask("Hello");
    assert_eq!(c.bodies()[0]["reasoning"], json!({"effort": "medium", "summary": "auto"}));
}

#[test]
fn the_pane_shows_the_model_by_its_name_and_the_effort_that_ran() {
    let mut c = listing(real_list(), vec![says_it_ran("gpt-6-luna-2026-10-01", Some("high"))]);
    c.ask("Hello");
    assert_eq!(model_line(&c), Some(("GPT-6-Luna".to_string(), Some("high".to_string()), false)));
    // Another model than the one asked for is shown as the server named it.
    let mut c = listing(real_list(), vec![says_it_ran("gpt-6-luna-mini", Some("high"))]);
    c.ask("Hello");
    assert_eq!(model_line(&c).map(|l| l.0), Some("gpt-6-luna-mini".to_string()));
}

#[test]
fn a_stream_that_reports_no_effort_shows_what_was_asked_and_says_so() {
    let mut c = listing(real_list(), vec![says_it_ran("gpt-6-luna", None)]);
    c.ask("Hello");
    assert_eq!(model_line(&c), Some(("GPT-6-Luna".to_string(), Some("medium".to_string()), true)));
    // A stream that says nothing at all: the same, once the answer is complete.
    let mut c = listing(real_list(), vec![Reply::sse(&[message("Hi."), completed()])]);
    c.ask("Hello");
    assert_eq!(model_line(&c), Some(("GPT-6-Luna".to_string(), Some("medium".to_string()), true)));
}

#[test]
fn the_line_follows_what_the_stream_reports_and_keeps_it_through_events_that_dont() {
    let event = |kind: &str, effort: Option<&str>| json!({"type": kind, "response": {"model": "gpt-6-luna", "reasoning": {"effort": effort}}});
    let answer = Reply::sse(&[
        event("response.created", None),
        event("response.in_progress", Some("high")),
        event("response.in_progress", None),
        message("Hi."),
        completed(),
    ]);
    let mut c = listing(real_list(), vec![answer]);
    c.ask("Hello");
    assert_eq!(model_line(&c), Some(("GPT-6-Luna".to_string(), Some("high".to_string()), false)));
}

/// `unsupported_value` on the effort, as the Responses API says it.
fn effort_refused() -> Reply {
    Reply::json(
        400,
        &json!({"error": {"type": "invalid_request_error", "code": "unsupported_value", "param": "reasoning.effort",
        "message": "Unsupported value: 'medium' is not supported with this model."}}),
    )
}

#[test]
fn a_refused_effort_is_retried_once_without_it_keeps_the_summary_and_says_so() {
    let mut c = listing(real_list(), vec![effort_refused(), says_it_ran("gpt-6-luna", None)]);
    c.ask("Hello");
    let bodies = c.bodies();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0]["reasoning"]["effort"], "medium");
    assert_eq!(bodies[1]["reasoning"], json!({"summary": "auto"}));
    assert_eq!(c.answer().as_deref(), Some("Hi."));
    assert!(notes(&c).iter().any(|n| n.contains("medium") && n.contains("wasn't applied")), "{:?}", notes(&c));
    assert_eq!(model_line(&c), Some(("GPT-6-Luna".to_string(), None, false)));
    // Refused again without it: said, not looped.
    let mut c = listing(real_list(), vec![effort_refused(), effort_refused()]);
    c.ask("Hello");
    assert_eq!(c.bodies().len(), 2);
    assert!(c.error().unwrap().contains("Unsupported value"), "{:?}", c.error());
}

#[test]
fn an_effort_refused_in_the_stream_corrects_the_line_already_shown() {
    let failed = json!({"type": "response.failed", "response": {"status": "failed", "error": {"code": "unsupported_value", "param": "reasoning.effort", "message": "no"}}});
    let first = Reply::sse(&[json!({"type": "response.created", "response": {"model": "gpt-6-luna"}}), failed]);
    let mut c = listing(real_list(), vec![first, says_it_ran("gpt-6-luna", None)]);
    c.ask("Hello");
    assert_eq!(c.bodies().len(), 2);
    assert_eq!(model_line(&c), Some(("GPT-6-Luna".to_string(), None, false)));
    let lines = c.assistant.transcript().entries().filter(|e| matches!(e, Entry::Model { .. })).count();
    assert_eq!(lines, 1, "the line is replaced, not added to");
}

#[test]
fn an_effort_refused_in_the_stream_corrects_the_line_even_if_the_retry_fails() {
    let failed = json!({"type": "response.failed", "response": {"status": "failed", "error": {"code": "unsupported_value", "param": "reasoning.effort", "message": "no"}}});
    let first = Reply::sse(&[json!({"type": "response.created", "response": {"model": "gpt-6-luna"}}), failed]);
    let mut c = listing(real_list(), vec![first, refusal(400, "bad_request", "No.")]);
    c.ask("Hello");
    assert_eq!(c.bodies().len(), 2);
    assert!(c.error().is_some());
    assert_eq!(model_line(&c), Some(("GPT-6-Luna".to_string(), None, false)));
}

#[test]
fn a_list_with_nothing_usable_is_read_again_next_turn() {
    let mut c = listing(Reply::json(200, &json!({})), vec![says("Hi.")]);
    c.ask("Hello");
    assert!(c.error().unwrap().contains("offers no model"), "{:?}", c.error());
    *c.script.models.lock().unwrap() = Some(real_list());
    c.ask("Again");
    assert_eq!(c.error(), None);
    assert_eq!(c.bodies()[0]["model"], "gpt-6-luna");
    assert_eq!(c.server.to("/v1/models").len(), 2);
}

#[test]
fn a_refused_effort_stays_left_out_until_the_model_changes() {
    let mut c = listing(real_list(), vec![effort_refused(), says("Hi."), says("Again."), says("Other.")]);
    c.ask("Hello");
    c.ask("Again");
    assert_eq!(c.bodies()[2]["reasoning"], json!({"summary": "auto"}), "the next turn doesn't ask again");
    assert_eq!(notes(&c), Vec::<String>::new());
    assert_eq!(model_line(&c).map(|l| l.1), Some(None), "and doesn't show what it didn't ask for");
    c.assistant.choose(Some("gpt-6.1-sol".into()), None);
    c.ask("Other");
    assert_eq!(c.bodies()[3]["reasoning"]["effort"], "medium", "another model may take it");
}

#[test]
fn a_chosen_model_that_left_the_list_falls_back_and_the_pane_says_so() {
    let mut c = listing(real_list(), vec![says("Hi.")]);
    c.assistant.choose(Some("gpt-gone".into()), None);
    c.ask("Hello");
    assert_eq!(c.bodies()[0]["model"], "gpt-6-luna");
    assert!(notes(&c).iter().any(|n| n.contains("gpt-gone") && n.contains("GPT-6-Luna")), "{:?}", notes(&c));
}

#[test]
fn a_chosen_effort_the_model_lacks_falls_back_and_the_pane_says_so() {
    let mut c = listing(real_list(), vec![says("Hi.")]);
    c.assistant.choose(None, Some("ultra".into()));
    c.ask("Hello");
    assert_eq!(c.bodies()[0]["reasoning"]["effort"], "medium");
    assert!(notes(&c).iter().any(|n| n.contains("ultra") && n.contains("medium")), "{:?}", notes(&c));
}

#[test]
fn a_chosen_model_and_effort_reach_the_next_request_with_the_history_intact() {
    let mut c = listing(real_list(), vec![says("First."), says("Second."), says("Third.")]);
    c.ask("One");
    assert_eq!((&c.bodies()[0]["model"], &c.bodies()[0]["reasoning"]["effort"]), (&json!("gpt-6-luna"), &json!("medium")));
    c.assistant.choose(Some("gpt-6.1-sol".into()), Some("ultra".into()));
    c.ask("Two");
    let second = &c.bodies()[1];
    assert_eq!((&second["model"], &second["reasoning"]["effort"]), (&json!("gpt-6.1-sol"), &json!("ultra")));
    assert_eq!(second["input"], json!([user("One"), assistant("First."), user("Two")]));
    // Back to the default: the same conversation goes on.
    c.assistant.choose(None, None);
    c.ask("Three");
    let third = &c.bodies()[2];
    assert_eq!((&third["model"], &third["reasoning"]["effort"]), (&json!("gpt-6-luna"), &json!("medium")));
    assert_eq!(third["input"], json!([user("One"), assistant("First."), user("Two"), assistant("Second."), user("Three")]));
    assert_eq!(c.server.to("/v1/models").len(), 1, "the list is read once a conversation");
}

#[test]
fn a_list_that_cant_be_read_doesnt_stop_a_turn_with_a_choice() {
    let mut c = listing(Reply::json(500, &json!({"error": "down"})), vec![says_it_ran("gpt-saved", None)]);
    c.ask("Nothing chosen");
    assert!(c.error().unwrap().contains("couldn't be listed"), "{:?}", c.error());
    c.assistant.choose(Some("gpt-saved".into()), Some("high".into()));
    c.ask("Hello");
    assert_eq!(c.error(), None);
    assert_eq!(c.bodies()[0]["model"], "gpt-saved");
    assert_eq!(c.bodies()[0]["reasoning"]["effort"], "high");
    assert_eq!(model_line(&c).map(|l| l.0), Some("gpt-saved".to_string()));
}
