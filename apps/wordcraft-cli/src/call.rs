//! `wordcraft-cli call <method> [<params> | -] [--connect HOST:PORT]`: one control-channel
//! request to the running app, or to a shared document's room, and its result printed as JSON,
//! for scripts and for agents that reach WordCraft through a shell (Pi). The key is
//! `WORDCRAFT_CONTROL_KEY` (an invite's key reaches the room), or the app's key file for the
//! port; it goes only to 127.0.0.1, where WordCraft listens, never to another address.

use std::io::{Read, Write};

use serde_json::Value;
use wordcraft_mcp::Backend;

use crate::{Failure, OptionArg, OptionSpec, escape_peer, find_option_arg, positionals, valid_connect_addr, validate_options, write_stdout};

pub(crate) const CALL_OPTIONS: &[OptionSpec] = &[OptionSpec { name: "--connect", takes_value: true }];

/// Where the app or the room is reached when `--connect` doesn't say.
pub(crate) const ADDR_VAR: &str = "WORDCRAFT_CONTROL_ADDR";

/// Most bytes of params read from standard input.
const MAX_PARAMS: u64 = 64 << 20;

/// Longest method name sent.
const MAX_METHOD: usize = 256;

/// `wordcraft-cli call`, with the arguments after `call`, the address in [`ADDR_VAR`] and
/// standard input for `-`.
pub(crate) fn call(rest: &[String], addr_var: Option<String>, stdin: impl Read) -> Result<(), Failure> {
    validate_options("call", rest, CALL_OPTIONS)?;
    let operands = positionals(rest, CALL_OPTIONS);
    let (method, params) = match operands.as_slice() {
        [] => return Err("call: give a method, like `wordcraft-cli call document.inspect`".into()),
        [method] => (method.as_str(), Value::Object(serde_json::Map::new())),
        [method, params] => (method.as_str(), parse_params(params, stdin)?),
        _ => return Err("call: too many arguments: give the method and one JSON object of params (quote it)".into()),
    };
    let method = method.trim();
    if method.is_empty() || method.len() > MAX_METHOD || method.chars().any(char::is_control) {
        return Err(format!("call: `{}` isn't a method", escape_peer(&method.chars().take(64).collect::<String>())).into());
    }
    let addr = address(rest, addr_var)?;
    let mut remote = wordcraft_mcp::Remote::connect(&addr).map_err(|e| format!("call: WordCraft isn't reachable at {addr}: {e}"))?;
    let result = remote.call(method, params).map_err(|e| format!("call: {}", escape_peer(&e)))?;
    let shown = serde_json::to_string_pretty(&result).map_err(|e| format!("call: {e}"))?;
    write_stdout(|o| writeln!(o, "{shown}"))
}

/// The address: `--connect`'s, else [`ADDR_VAR`]'s; a host and a numeric port, at 127.0.0.1 or
/// `localhost`, the only place a key goes.
fn address(rest: &[String], addr_var: Option<String>) -> Result<String, Failure> {
    let addr = match find_option_arg(rest, "--connect") {
        OptionArg::Value(v) => v.trim().to_string(),
        OptionArg::Duplicate => return Err("call: --connect given twice".into()),
        OptionArg::MissingValue => return Err("call: --connect needs HOST:PORT".into()),
        OptionArg::Absent => addr_var.map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).ok_or_else(|| {
            Failure::from(format!("call: no address: give --connect HOST:PORT, or set {ADDR_VAR} (an agent WordCraft started has it)"))
        })?,
    };
    let shown = escape_peer(&addr.chars().take(128).collect::<String>());
    if !valid_connect_addr(&addr) {
        return Err(format!("call: the address needs HOST:PORT with a numeric port, got `{shown}`").into());
    }
    if !wordcraft_mcp::backend::is_app_addr(&addr) {
        return Err(
            format!("call: {shown} isn't WordCraft's address: it listens on 127.0.0.1 (or localhost) only, and the key goes nowhere else").into()
        );
    }
    Ok(addr)
}

/// The params: a JSON object, given as an argument or, for `-`, on standard input.
fn parse_params(arg: &str, stdin: impl Read) -> Result<Value, Failure> {
    let text = if arg == "-" {
        let mut bytes = Vec::new();
        stdin.take(MAX_PARAMS + 1).read_to_end(&mut bytes).map_err(|e| format!("call: can't read the params from standard input: {e}"))?;
        if bytes.len() as u64 > MAX_PARAMS {
            return Err(format!("call: the params on standard input are over {} MiB", MAX_PARAMS >> 20).into());
        }
        String::from_utf8(bytes).map_err(|_| "call: the params on standard input aren't UTF-8 text".to_string())?
    } else {
        arg.to_string()
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(v @ Value::Object(_)) => Ok(v),
        Ok(_) => Err("call: the params must be a JSON object, like '{\"text\": \"Hello\"}'".into()),
        Err(e) => Err(format!("call: the params aren't JSON ({e}): give one JSON object, like '{{\"text\": \"Hello\"}}'").into()),
    }
}
