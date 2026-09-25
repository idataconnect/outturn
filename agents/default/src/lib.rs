//! The platform's default agent.
//!
//! Runs a conversational turn: hands the conversation to the model, runs any
//! tool the model asks for, and returns its reply. Deliberately minimal -- it
//! exists to make the runtime tier real, and to be the reference for what a
//! guest looks like.
//!
//! Everything this component can reach is a host import. It has no sockets, no
//! filesystem and no credentials: a model call goes through `chat`, the clock
//! comes from `current-time`, and the host attaches the token.

#[allow(warnings)]
mod bindings;

mod archive;

use std::collections::BTreeSet;

use bindings::exports::outturn::agent::agent::Guest;
use bindings::outturn::agent::host::{
    ContentPart,
    self, Arrival, CompletionRequest, Message, ToolActivity, ToolCall, ToolDefinition, ToolOutcome,
};

struct Component;

/// The model's name for the clock tool.
const CURRENT_TIME: &str = "get_current_time";
const READ_OBJECT: &str = "read_object";
const WRITE_OBJECT: &str = "write_object";
const DELETE_OBJECT: &str = "delete_object";
const LIST_OBJECTS: &str = "list_objects";

/// The model's name for the outbound request tool.
const FETCH: &str = "fetch_url";
const EXPAND_ARCHIVE: &str = "expand_archive";
/// The model's name for asking what is in an image.
const DESCRIBE_IMAGE: &str = "describe_image";
const CREATE_ARCHIVE: &str = "create_archive";

/// The model's name for the tool that loads other tools.
///
/// Always offered, and never itself deferred: it is the only way to reach
/// anything in the deferred set, so a turn that did not have it would be a
/// turn with no tools at all.
const LOAD_TOOLS: &str = "load_tools";

/// Tools offered from the first round, without being asked for.
///
/// The deployment's, not the component's: it comes from `host::eager_tools`,
/// and what the host puts there is the host's business -- today it sends
/// nothing, so the default below is the whole of it. A name here is offered
/// eagerly, a name absent is deferred, and nothing else decides it. Empty --
/// the default -- defers everything, so every tool is reached through
/// `load_tools`.
///
/// Read once per turn rather than per round. It cannot change mid-turn, and a
/// tool set that shifted under the model would strand a definition it had
/// already been given.
fn eager_tools() -> BTreeSet<String> {
    host::eager_tools().into_iter().collect()
}

/// How much of a file a single read puts in front of the model.
///
/// Comfortably under the generic tool ceiling, so an assembled head-and-tail
/// is not then cut in half again by the truncator that follows it.
const READ_BUDGET: u64 = 32 * 1024;

/// How that budget splits when both ends are worth having.
///
/// Head-heavy because a file's beginning usually establishes what it is --
/// headers, imports, structure -- while its end is only sometimes the
/// interesting part. Where the end is what matters, as in a log, the model
/// asks for it directly rather than relying on this number, which is why the
/// ratio can afford to be a guess.
const HEAD_SHARE: u64 = 70;

/// The argument every tool carries so the user can see what is happening.
///
/// Asked of the model rather than inferred: only the model knows what it is
/// about to do, and a label written by the guest would be a plausible fiction
/// attributed to the agent.
const ACTION: &str = "action";

/// Every tool this agent can run, whether or not it is currently offered.
///
/// The single source of what exists. Both the offered set and the name list in
/// `load_tools`'s own description are derived from this, so a tool cannot be
/// added to one and forgotten in the other -- the same reason the ceilings
/// below are interpolated rather than written out.
fn all_tools() -> Vec<ToolDefinition> {
    vec![
    ToolDefinition {
        name: READ_OBJECT.to_string(),
        // The ceilings are interpolated rather than written out, so the
        // numbers the model is told and the numbers enforced cannot drift.
        description: format!(
            "Read a stored file. Every path starts with its scope: session/ for \
             this conversation's files, including anything the user shared; \
             agent/ for files this agent keeps between conversations; workspace/ \
             for files the whole workspace shares. There is nothing above \
             those to reach, so do not try. At most {} lines or {}KB comes \
             back at once; a file larger than that returns its start and its \
             end, and says at what offset to read again for the middle.",
            MAX_TOOL_LINES,
            MAX_TOOL_BYTES / 1024
        ),
        parameters: r#"{"type":"object","properties":{"path":{"type":"string","description":"Scoped path, e.g. session/upload.pdf, agent/notes.md or workspace/reports/q3.csv"},"offset":{"type":"integer","description":"Byte to start from. Omit for the beginning.","default":0},"from_end":{"type":"boolean","description":"Read the end of the file instead of the start. Use this for logs, where what went wrong is at the bottom.","default":false},"action":{"type":"string","description":"A short phrase naming what you are doing, in the present continuous, for the user to read while it happens. For example: Reading last quarter's figures."}},"required":["path","action"]}"#
            .to_string(),
    },
    ToolDefinition {
        name: DESCRIBE_IMAGE.to_string(),
        description:
            "Ask a model that can see what is in a stored image. The answer is \
             about the question you ask, so ask for what you actually need: \
             \"what is in this picture\" and \"what is the total on this \
             receipt\" get different answers, and asking again with a sharper \
             question is normal rather than wasteful. PNG, JPEG, GIF and WebP. \
             You never receive the image itself, only the answer."
                .to_string(),
        parameters: r#"{"type":"object","properties":{"path":{"type":"string","description":"Scoped path to the image, e.g. session/screenshot.png"},"question":{"type":"string","description":"What you want to know about it. Be specific: this is what the model is asked to look for."},"action":{"type":"string","description":"A short phrase naming what you are doing, in the present continuous, for the user to read while it happens. For example: Reading the receipt."}},"required":["path","question","action"]}"#
            .to_string(),
    },
    ToolDefinition {
        name: WRITE_OBJECT.to_string(),
        description: "Write a file, replacing whatever was there. Every path starts \
                      with its scope: session/ for scratch and results of this \
                      conversation, which is where most writes belong; agent/ for \
                      things this agent should keep; workspace/ for the whole \
                      workspace. Some scopes may be read-only for this agent, and \
                      the error will say so and where to write instead."
            .to_string(),
        parameters: r#"{"type":"object","properties":{"path":{"type":"string","description":"Scoped path, e.g. session/summary.md"},"content":{"type":"string","description":"The complete new contents."},"action":{"type":"string","description":"A short phrase naming what you are doing, in the present continuous, for the user to read while it happens. For example: Saving the summary."}},"required":["path","content","action"]}"#
            .to_string(),
    },
    ToolDefinition {
        name: DELETE_OBJECT.to_string(),
        description: "Delete a stored file. It goes for good: there is nothing to \
                      undo it with, so only delete what was asked for, and one \
                      file at a time rather than a folder at a guess. Scopes work \
                      as they do for writing -- a scope this agent may only read \
                      is one it may not delete from, and the error will say so. \
                      A path that names nothing is an error, not a quiet success: \
                      never tell someone a file is gone unless this said it went."
            .to_string(),
        parameters: r#"{"type":"object","properties":{"path":{"type":"string","description":"Scoped path of the file to delete, e.g. session/draft.md"},"action":{"type":"string","description":"A short phrase naming what you are doing, in the present continuous, for the user to read while it happens. For example: Deleting the draft."}},"required":["path","action"]}"#
            .to_string(),
    },
    ToolDefinition {
        name: LIST_OBJECTS.to_string(),
        description: "List stored files. Omit the prefix to see every scope; give a \
                      scope such as session/ or a folder such as workspace/reports/ \
                      to narrow it."
            .to_string(),
        parameters: r#"{"type":"object","properties":{"prefix":{"type":"string","description":"Scoped prefix, e.g. session/ or workspace/reports/. Omit for everything."},"action":{"type":"string","description":"A short phrase naming what you are doing, in the present continuous, for the user to read while it happens. For example: Looking through the stored files."}},"required":["action"]}"#
            .to_string(),
    },
    ToolDefinition {
        name: EXPAND_ARCHIVE.to_string(),
        description: "Unpack a zip archive into a folder of files. Nothing is \
                      unpacked until you ask: an archive stays one file until \
                      something in it is needed. Entries land under the \
                      destination and are then ordinary files -- documents \
                      among them become readable the way any upload does. \
                      Expanding into session/ needs no permission; workspace/ \
                      needs the right to write there. Entries that would \
                      escape the destination are skipped and named in the \
                      result rather than failing it."
            .to_string(),
        parameters: r#"{"type":"object","properties":{"path":{"type":"string","description":"Scoped path of the archive, e.g. session/invoices.zip"},"destination":{"type":"string","description":"Scoped folder to unpack into. Omit for a folder beside the archive named after it."},"action":{"type":"string","description":"A short phrase naming what you are doing, in the present continuous, for the user to read while it happens. For example: Unpacking last year's invoices."}},"required":["path","action"]}"#
            .to_string(),
    },
    ToolDefinition {
        name: CREATE_ARCHIVE.to_string(),
        description: "Put everything under a folder -- or one named file -- \
                      into one zip archive. Files go in as stored -- a PDF as \
                      the PDF, not as its text -- with names relative to the \
                      folder. The originals are left where they are; delete \
                      them yourself if the archive is meant to replace them."
            .to_string(),
        parameters: r#"{"type":"object","properties":{"prefix":{"type":"string","description":"Scoped folder to archive, e.g. workspace/invoices/2025/, or one file, e.g. session/main.pdf"},"path":{"type":"string","description":"Scoped path for the archive, e.g. workspace/archive/invoices-2025.zip"},"action":{"type":"string","description":"A short phrase naming what you are doing, in the present continuous, for the user to read while it happens. For example: Archiving 2025's invoices."}},"required":["prefix","path","action"]}"#
            .to_string(),
    },
    ToolDefinition {
        name: FETCH.to_string(),
        description: "Make an HTTP request to an external API. Only hosts this \
                      workspace has allowed can be reached; anything else comes \
                      back refused, and no amount of rephrasing changes that -- \
                      say so rather than trying another address. Credentials \
                      are attached automatically where they are configured, so \
                      never put a key in the URL or in a header. Redirects are \
                      not followed: a 3xx response means asking again for the \
                      new location."
            .to_string(),
        parameters: r#"{"type":"object","properties":{"url":{"type":"string","description":"Absolute https URL."},"method":{"type":"string","description":"GET, POST, PUT, PATCH, DELETE or HEAD. Defaults to GET.","enum":["GET","POST","PUT","PATCH","DELETE","HEAD"]},"headers":{"type":"object","description":"Extra headers, as a flat object. Leave authorization out; it is added for you.","additionalProperties":{"type":"string"}},"body":{"type":"string","description":"Request body, for methods that take one."},"action":{"type":"string","description":"A short phrase naming what you are doing, in the present continuous, for the user to read while it happens. For example: Looking up the exchange rate."}},"required":["url","action"]}"#
            .to_string(),
    },
    ToolDefinition {
        name: CURRENT_TIME.to_string(),
        description: "The current date and time in the user's own timezone. \
                      Call this whenever the answer depends on what time it is \
                      now -- today's date, the day of the week, how long until \
                      something. Do not guess it."
            .to_string(),
        // No timezone argument: the zone is the user's, and letting the
        // model name one would invite it to invent a plausible wrong answer.
        // `action` is the one thing asked of it, and it is for the user.
        parameters: r#"{"type":"object","properties":{"action":{"type":"string","description":"A short phrase naming what you are doing, in the present continuous, for the user to read while it happens. For example: Checking today's date. Not an explanation of why."}},"required":["action"]}"#
            .to_string(),
    },
    ]
}

/// The names of every tool not offered from the start.
///
/// Derived, never written down: a tool is deferred by not being eager, so the
/// two sets cannot disagree about a tool and cannot leave one unreachable.
fn deferred_names(eager: &BTreeSet<String>) -> Vec<String> {
    all_tools()
        .into_iter()
        .map(|t| t.name)
        .filter(|name| !eager.contains(name))
        .collect()
}

/// The tool that offers the others.
///
/// Its description carries the name of every deferred tool, and that is the
/// point: a model cannot ask for what it does not know exists, so withholding
/// the names along with the schemas would mean a tool nothing ever reaches.
/// What is deferred is each tool's description and arguments -- the bulk of
/// what a definition costs -- while the fact of it stays in front of the model
/// on every round.
///
/// Names alone, deliberately. A line of explanation each would defeat the
/// saving, and these names were written to be read by a model: `create_archive`
/// and `describe_image` say what they do. A tool whose name does not is a tool
/// that needs renaming rather than annotating.
///
/// The instruction is one sentence for the same reason. An earlier version
/// spent four on when not to call it and on not ending the turn after a load
/// -- naming a failure and asking the model not to commit it, which is a
/// weak way to prevent anything and was being paid for on every round.
fn loader_tool(eager: &BTreeSet<String>) -> ToolDefinition {
    let names = deferred_names(eager);
    ToolDefinition {
        name: LOAD_TOOLS.to_string(),
        description: format!(
            "Call load_tools with one or more tool names of these unloaded \
             tools before using them: {}",
            names.join(", ")
        ),
        parameters: r#"{"type":"object","properties":{"names":{"type":"array","description":"The tools to load, by exact name.","items":{"type":"string"}},"action":{"type":"string","description":"A short phrase naming what you are doing, in the present continuous, for the user to read while it happens. For example: Getting ready to unpack the archive."}},"required":["names","action"]}"#
            .to_string(),
    }
}

/// What the model is offered this round.
///
/// The eager set, plus whatever has been loaded so far, plus the loader itself
/// while anything is still unloaded. The loader drops out once nothing is left
/// to load, so a turn that has loaded everything does not carry a tool whose
/// whole description is an empty list.
fn offered_tools(eager: &BTreeSet<String>, loaded: &BTreeSet<String>) -> Vec<ToolDefinition> {
    let mut offered: Vec<ToolDefinition> = all_tools()
        .into_iter()
        .filter(|t| eager.contains(&t.name) || loaded.contains(&t.name))
        .collect();

    if deferred_names(eager).iter().any(|name| !loaded.contains(name)) {
        offered.push(loader_tool(eager));
    }

    offered
}

/// Whether a tool may be called this round.
///
/// Names the model took for tools that are really a bound skill's operations,
/// each with the file that says how to call it.
///
/// A skill's manifest lists its operations by name, and a weaker model reads
/// that list as tools and asks the loader for them. Told only "no such tool",
/// it concludes the skill is broken; told where the operation is documented, it
/// can go and read it. Found by listing `skill/` rather than by being told, so
/// the guest needs nothing new from the host.
fn skill_operations(names: &[String]) -> Vec<(String, String)> {
    let Ok(files) = host::list_objects("skill/") else {
        return Vec::new();
    };
    names
        .iter()
        .filter_map(|name| {
            let file = format!("/{name}.md");
            files
                .iter()
                .find(|f| f.path.ends_with(&file))
                .map(|f| (name.clone(), f.path.clone()))
        })
        .collect()
}

/// What to say about those names, or nothing if there are none.
fn operations_hint(names: &[String]) -> Option<String> {
    let found = skill_operations(names);
    if found.is_empty() {
        return None;
    }
    Some(
        found
            .iter()
            .map(|(name, path)| {
                format!(
                    "{name} is not a tool: it is an operation of a skill. Read {path} with \
                     read_object, then make the call it describes with fetch_url."
                )
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// The loader is always callable; everything else has to be eager or loaded.
fn is_offered(name: &str, eager: &BTreeSet<String>, loaded: &BTreeSet<String>) -> bool {
    name == LOAD_TOOLS || eager.contains(name) || loaded.contains(name)
}

/// Loads tools, reporting what was recognised.
///
/// Unknown names are named back rather than failing: a model that misremembered
/// a name can correct itself, where an error would end the turn. A name that is
/// already loaded is a success, not a complaint -- it is loaded, which is what
/// was asked for.
fn load_tools(
    args: &serde_json::Value,
    eager: &BTreeSet<String>,
    loaded: &mut BTreeSet<String>,
) -> String {
    let available = deferred_names(eager);
    let requested: Vec<String> = args
        .get("names")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    if requested.is_empty() {
        return serde_json::json!({
            "error": "name at least one tool to load",
            "available": available,
        })
        .to_string();
    }

    let (found, missing): (Vec<String>, Vec<String>) = requested
        .into_iter()
        .partition(|name| available.contains(name));

    for name in &found {
        loaded.insert(name.clone());
    }

    if missing.is_empty() {
        serde_json::json!({ "loaded": found }).to_string()
    } else if found.is_empty() {
        // Nothing matched, so nothing was loaded and the call achieved
        // nothing. Reported as a failure rather than as an empty success:
        // a model told the call succeeded will go on to use tools it does
        // not have, and a reader watching the turn sees a tick against work
        // that did not happen.
        //
        // Where the names are a skill's operations, that leads: it is the
        // thing to do next, and a list of tools that do not include them only
        // confirms the wrong conclusion.
        let hint = operations_hint(&missing);
        let error = format!(
            "no such tool: {}. Available: {}",
            missing.join(", "),
            available.join(", ")
        );
        serde_json::json!({
            "error": match &hint {
                Some(hint) => format!("{hint} ({error})"),
                None => error,
            },
            "no_such_tool": missing,
            "available": available,
        })
        .to_string()
    } else {
        // Both halves are reported. Some tools became available even though a
        // name was wrong, and a model told only about the failure would load
        // them again. Not an error: what was asked for partly happened, and
        // the turn can continue with what did load.
        let mut out = serde_json::json!({
            "loaded": found,
            "no_such_tool": missing,
            "available": available,
        });
        if let Some(hint) = operations_hint(&missing) {
            out["hint"] = serde_json::json!(hint);
        }
        out.to_string()
    }
}

/// What earlier turns in this conversation already loaded.
///
/// The loaded set is rebuilt from the transcript rather than carried, because
/// the guest is instantiated fresh for every turn and has nowhere to carry it.
/// Every `load_tools` call the model has made is in the history it is handed,
/// so the evidence is already there and costs nothing to read.
///
/// Scoped to what the model can still see, which is the whole point: a tool
/// stays loaded while the round that loaded it is in the conversation, and
/// deferral reasserts itself once compaction drops that round -- which is also
/// the point where the model has stopped being able to remember the schema.
/// Carrying the set forward unconditionally would end a long session offering
/// every tool on every turn, which is the cost deferral exists to avoid.
///
/// Only the arguments are read, never the result. A call whose result was
/// dropped to fit, or whose turn died before it answered, still tells us the
/// model asked -- and re-offering a tool that was never really loaded costs a
/// definition, where withholding one the model believes it has costs a round
/// and reads as the platform forgetting what it just did.
fn already_loaded(conversation: &[Message], eager: &BTreeSet<String>) -> BTreeSet<String> {
    let available = deferred_names(eager);
    let mut loaded = BTreeSet::new();

    for call in conversation
        .iter()
        .flat_map(|m| m.parts.iter())
        .filter_map(|part| match part {
            ContentPart::Call(c) => Some(c),
            ContentPart::Text(_) => None,
        })
        .filter(|c| c.name == LOAD_TOOLS)
    {
        let Ok(args) = serde_json::from_str::<serde_json::Value>(&call.arguments) else {
            // Arguments a model produced, so they may not be JSON at all --
            // and a round cut partway leaves them truncated mid-object. A call
            // we cannot read names no tools, which is the safe direction: the
            // model is offered the loader again rather than a tool nobody
            // asked for.
            continue;
        };

        let names = args
            .get("names")
            .and_then(|v| v.as_array())
            .map(|items| items.iter().filter_map(|v| v.as_str()))
            .into_iter()
            .flatten();

        for name in names {
            // Checked against the deferred set, so a name the model invented
            // does not enter the loaded set and get offered as a tool that
            // does not exist.
            if available.contains(&name.to_string()) {
                loaded.insert(name.to_string());
            }
        }
    }

    loaded
}

/// Reads a JSON string argument, or an empty string if it is missing.
fn arg<'a>(args: &'a serde_json::Value, name: &str) -> &'a str {
    args.get(name).and_then(|v| v.as_str()).unwrap_or("")
}

/// Turns what the user said mid-turn into messages for the model.
///
/// Marked as having arrived during the work, because that is true and the
/// model should treat it as a correction to what it is doing rather than as
/// the next question in an orderly exchange.
///
/// Several at once are numbered and the model is told to answer each. Given
/// two bare user messages in a row a model answers the last one and drops
/// the first -- someone who typed "two" then "three" was told about three.
/// A message that is only prose, which is most of them.
fn text_message(role: &str, text: String) -> Message {
    Message {
        role: role.to_string(),
        parts: vec![ContentPart::Text(text)],
        tool_call_id: None,
    }
}

/// The prose of a reply, with the calls between it left out.
fn text_of(parts: &[ContentPart]) -> String {
    let mut out = String::new();
    for part in parts {
        if let ContentPart::Text(t) = part {
            out.push_str(t);
        }
    }
    out
}

/// The calls a reply asked for, in the order it asked.
fn calls_of(parts: &[ContentPart]) -> Vec<ToolCall> {
    parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::Call(c) => Some(c.clone()),
            ContentPart::Text(_) => None,
        })
        .collect()
}

fn injected(arrivals: &[Arrival]) -> Vec<Message> {
    let total = arrivals.len();
    arrivals
        .iter()
        .enumerate()
        .map(|(i, arrival)| {
            let text = if total == 1 {
                format!("[mid-turn message from user] {}", arrival.content)
            } else {
                format!(
                    "[mid-turn message {} of {} from user; respond to each of the {} in order] {}",
                    i + 1,
                    total,
                    total,
                    arrival.content
                )
            };
            text_message("user", text)
        })
        .collect()
}

/// Two independent ceilings on what a tool may put in front of the model.
///
/// Whichever is reached first wins, because they fail differently: a single
/// enormous line passes any line count, and a hundred thousand short ones pass
/// any byte count.
const MAX_TOOL_LINES: usize = 2000;
const MAX_TOOL_BYTES: usize = 50 * 1024;

/// Cuts a result down to what the model should see.
///
/// Never mid-line: half a line of JSON or code reads as though it were whole,
/// and a model will act on it. What was removed is described rather than
/// silently dropped, so the model can ask for it differently instead of
/// concluding the file was short.
fn for_the_model(full: &str) -> String {
    let total_lines = full.lines().count();
    let total_bytes = full.len();
    if total_lines <= MAX_TOOL_LINES && total_bytes <= MAX_TOOL_BYTES {
        return full.to_string();
    }

    // One line larger than the whole budget cannot be usefully cut: what comes
    // back is a fragment of minified source or an encoded blob, which costs the
    // full budget and tells the model nothing it can act on. Better to return
    // none of it and say how to ask for a part.
    if full.lines().next().is_some_and(|first| first.len() > MAX_TOOL_BYTES) {
        let first = full.lines().next().unwrap_or_default();
        return format!(
            "[the first line is {} bytes, larger than the {}KB that can be shown. \
             It is likely minified or encoded. Read a part of it with offset and \
             work forward, or read a different file.]",
            first.len(),
            MAX_TOOL_BYTES / 1024
        );
    }

    let mut kept = String::new();
    let mut lines = 0usize;
    for line in full.lines() {
        if lines >= MAX_TOOL_LINES || kept.len() + line.len() + 1 > MAX_TOOL_BYTES {
            break;
        }
        kept.push_str(line);
        kept.push('\n');
        lines += 1;
    }

    // Which ceiling was reached decides what to do next, so it is named
    // rather than left to be inferred: a result stopped by bytes has long
    // lines in it and reading further will not help much, where one stopped by
    // lines has many short ones and reading on will.
    let by = if lines >= MAX_TOOL_LINES { "lines" } else { "bytes" };
    format!(
        "{kept}\n[truncated by {by}: showing {lines} of {total_lines} lines, \
         {} of {total_bytes} bytes. Read again from offset {} for the rest.]",
        kept.len(),
        kept.len()
    )
}

/// Reads a range and turns the bytes into something a model can use.
///
/// Not text is reported as not text, rather than mangled into replacement
/// characters: a model told "42KB of binary" can decide what to do, where a
/// model shown mojibake will try to read it.
fn slice(path: &str, offset: u64, len: u64) -> Result<String, String> {
    let bytes = host::read_object(path, offset, len.min(u32::MAX as u64) as u32)?;

    // A range is named in bytes, so its start can land inside a character
    // rather than before one. That is not a file which is "not text" -- it is
    // a text file read from an unlucky offset, and calling it binary would be
    // wrong about the file and would send the model looking for a problem that
    // does not exist. It happens in proportion to how much of the file is not
    // ASCII, so a document of em-dashes trips it and a log of ASCII never
    // does.
    //
    // Both ends can be cut: a range starting at an offset begins wherever that
    // offset lands, and a range of a given length ends wherever the length
    // runs out. A head read starts at zero and still ends mid-character.
    //
    // A character is at most four bytes, so at most three lead in to one and
    // at most three trail off the end. Dropping them costs a fragment that was
    // already incomplete, and the caller trims to whole lines afterwards
    // regardless.
    //
    // Refused rather than replaced: `from_utf8_lossy` would substitute
    // replacement characters and hand back something that reads as though it
    // were the file. A genuinely binary file should still say so.
    // The front first, then the back, and in that order: a range that begins
    // mid-character has no valid prefix at all, so `valid_up_to` is zero and
    // trimming by it alone leaves nothing. Continuation bytes are 10xxxxxx and
    // a character is at most four, so at most three lead in to one.
    let mut start = 0usize;
    while start < bytes.len() && start < 3 && (bytes[start] & 0xC0) == 0x80 {
        start += 1;
    }
    let bytes = &bytes[start..];

    match std::str::from_utf8(bytes) {
        Ok(text) => Ok(text.to_string()),
        Err(e) => {
            // Now the only bad bytes left are a character cut off the end,
            // which is exactly what `valid_up_to` reports.
            let end = e.valid_up_to();
            if end == 0 {
                // Nothing decoded from either end: this is not text.
                return Err("this file is not text and cannot be shown".to_string());
            }
            std::str::from_utf8(&bytes[..end])
                .map(|text| text.to_string())
                .map_err(|_| "this file is not text and cannot be shown".to_string())
        }
    }
}

/// Trims a fragment back to whole lines.
///
/// A range lands wherever the byte count says, which is usually mid-line. Half
/// a line reads as though it were whole and a model will act on it, so the
/// partial line at the cut is dropped -- from the end of a head, from the
/// start of a tail.
fn whole_lines(fragment: &str, drop_from_start: bool) -> &str {
    if drop_from_start {
        match fragment.find('\n') {
            Some(at) => &fragment[at + 1..],
            None => fragment,
        }
    } else {
        match fragment.rfind('\n') {
            Some(at) => &fragment[..at],
            None => fragment,
        }
    }
}

/// Reads a file, showing both ends when it will not fit.
///
/// Head-only truncation loses exactly the part that matters in a log or a
/// stack trace, where the setup is at the top and the failure at the bottom.
/// Both ends are shown instead, and what was skipped is described precisely
/// enough to go and get: a model that wants the middle can ask for it by
/// offset rather than guessing.
/// Asks what is in an image, and hands back what the model said.
///
/// Thin on purpose: the host reads the bytes, picks the model and asks. What
/// is left here is the shape of the answer, and the failure -- an image that
/// is not one, or a deployment with nowhere to send it -- which the model is
/// told plainly so it can say so rather than describe a picture it never saw.
fn describe_image(args: &serde_json::Value) -> String {
    let path = arg(args, "path");
    let question = arg(args, "question");
    match host::describe_image(path, question) {
        Ok(answer) => serde_json::json!({
            "path": path,
            "question": question,
            "answer": answer,
        })
        .to_string(),
        Err(e) => serde_json::json!({ "path": path, "error": e }).to_string(),
    }
}

fn read_object(args: &serde_json::Value) -> String {
    let path = arg(args, "path");
    let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0);
    let from_end = args.get("from_end").and_then(|v| v.as_bool()).unwrap_or(false);

    let size = match host::stat_object(path) {
        Ok(info) => info.size,
        Err(e) => return serde_json::json!({ "path": path, "error": e }).to_string(),
    };

    // The end, asked for directly. Possible only because the size is known
    // here -- a caller cannot name an offset it would have to have measured.
    if from_end {
        let start = size.saturating_sub(READ_BUDGET);
        return match slice(path, start, READ_BUDGET) {
            Ok(text) => {
                let shown = if start > 0 { whole_lines(&text, true) } else { &text[..] };
                serde_json::json!({
                    "path": path, "size": size, "showing": "end",
                    "content": if start > 0 {
                        format!("[{start} earlier bytes not shown]\n{shown}")
                    } else {
                        shown.to_string()
                    },
                })
                .to_string()
            }
            Err(e) => serde_json::json!({ "path": path, "size": size, "error": e }).to_string(),
        };
    }

    let remaining = size.saturating_sub(offset);
    if remaining <= READ_BUDGET {
        return match slice(path, offset, remaining.max(1)) {
            Ok(text) => serde_json::json!({
                "path": path, "size": size, "offset": offset,
                "showing": "all", "content": text,
            })
            .to_string(),
            Err(e) => serde_json::json!({ "path": path, "size": size, "error": e }).to_string(),
        };
    }

    let head_len = READ_BUDGET * HEAD_SHARE / 100;
    let tail_len = READ_BUDGET - head_len;
    let tail_start = size - tail_len;

    let head = match slice(path, offset, head_len) {
        Ok(text) => text,
        Err(e) => return serde_json::json!({ "path": path, "size": size, "error": e }).to_string(),
    };
    let tail = match slice(path, tail_start, tail_len) {
        Ok(text) => text,
        Err(e) => return serde_json::json!({ "path": path, "size": size, "error": e }).to_string(),
    };

    // A file with no line breaks in the part read is one long line -- minified
    // source, an encoded blob, a single-line export. Trimming to whole lines
    // cannot help, and what would come back is a fragment costing the whole
    // budget and carrying nothing the model can act on. Said, rather than
    // shown.
    if !head.contains('\n') && !tail.contains('\n') {
        return serde_json::json!({
            "path": path,
            "size": size,
            "error": format!(
                "this file is {size} bytes on a single line, so no useful part of \
                 it can be shown. It is likely minified or encoded. Read a span \
                 of it with offset if you need one."
            ),
        })
        .to_string();
    }

    let head = whole_lines(&head, false);
    let tail = whole_lines(&tail, true);
    let skipped_from = offset + head.len() as u64;
    let skipped = tail_start.saturating_sub(skipped_from);

    serde_json::json!({
        "path": path,
        "size": size,
        "showing": "start and end",
        // Said precisely enough to act on: a model wanting the middle reads
        // again from this offset rather than guessing where it went.
        "content": format!(
            "{head}\n[{skipped} bytes not shown. Read again with offset={skipped_from} \
             for the middle]\n{tail}"
        ),
    })
    .to_string()
}

fn write_object(args: &serde_json::Value) -> String {
    let path = arg(args, "path");
    match host::write_object(path, arg(args, "content").as_bytes()) {
        Ok(written) => serde_json::json!({ "path": path, "bytes": written }).to_string(),
        Err(e) => serde_json::json!({ "path": path, "error": e }).to_string(),
    }
}

fn delete_object(args: &serde_json::Value) -> String {
    let path = arg(args, "path");
    match host::delete_object(path) {
        Ok(()) => serde_json::json!({ "path": path, "deleted": true }).to_string(),
        Err(e) => serde_json::json!({ "path": path, "error": e }).to_string(),
    }
}

fn list_objects(args: &serde_json::Value) -> String {
    match host::list_objects(arg(args, "prefix")) {
        Ok(found) => serde_json::json!({
            "files": found
                .iter()
                .map(|f| serde_json::json!({ "path": f.path, "size": f.size }))
                .collect::<Vec<_>>(),
        })
        .to_string(),
        Err(e) => serde_json::json!({ "error": e }).to_string(),
    }
}

fn expand_archive(args: &serde_json::Value) -> String {
    let path = arg(args, "path");
    let destination = match arg(args, "destination") {
        "" => path
            .strip_suffix(".zip")
            .or_else(|| path.strip_suffix(".ZIP"))
            .unwrap_or(path)
            .to_string(),
        d => d.to_string(),
    };
    match archive::expand(path, &destination) {
        Ok(done) => serde_json::json!({
            "archive": path,
            "destination": destination,
            "written": done.written.len(),
            "bytes": done.bytes,
            "files": done.written,
            "skipped": done.skipped,
        })
        .to_string(),
        Err(e) => serde_json::json!({ "archive": path, "error": e }).to_string(),
    }
}

fn create_archive(args: &serde_json::Value) -> String {
    let prefix = arg(args, "prefix");
    let path = arg(args, "path");
    match archive::create(prefix, path) {
        Ok((entries, bytes)) => {
            serde_json::json!({ "archive": path, "entries": entries, "bytes": bytes }).to_string()
        }
        Err(e) => serde_json::json!({ "archive": path, "error": e }).to_string(),
    }
}

/// Asks the host to make a request.
///
/// Everything that decides whether this is allowed happens on the other side
/// of the boundary. What is left here is turning the model's arguments into a
/// request and its answer into something worth reading: a refusal says why in
/// a sentence, because a tool that fails opaquely gets called again the same
/// way, and a body that was cut short says so rather than trailing off.
fn fetch_url(args: &serde_json::Value) -> String {
    let url = arg(args, "url");
    if url.is_empty() {
        return serde_json::json!({ "error": "no url was given" }).to_string();
    }

    let method = match arg(args, "method") {
        "" => "GET".to_string(),
        given => given.to_string(),
    };

    // A flat object, because that is what a model reliably produces. Anything
    // else is dropped rather than guessed at.
    let headers: Vec<(String, String)> = args
        .get("headers")
        .and_then(|h| h.as_object())
        .map(|h| {
            h.iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                .collect()
        })
        .unwrap_or_default();

    let body = args
        .get("body")
        .and_then(|b| b.as_str())
        .map(str::to_string);

    // Echoed in the result, so the reader can see what was asked for.
    let requested = method.clone();
    match host::fetch(&host::HttpRequest {
        method,
        url: url.to_string(),
        headers,
        body,
    }) {
        Ok(response) => serde_json::json!({
            "method": requested,
            "url": url,
            "status": response.status,
            "body": response.body,
            "truncated": response.truncated,
            // The location, so a 3xx is actionable rather than a dead end --
            // the host will not follow one, and the model has to decide
            // whether the new address is worth asking for.
            "location": response
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("location"))
                .map(|(_, value)| value.clone()),
        })
        .to_string(),
        Err(e) => serde_json::json!({ "method": requested, "url": url, "error": e }).to_string(),
    }
}

/// Runs one tool call and returns the message answering it.
///
/// `loaded` is carried in rather than owned here because `load_tools` writes to
/// it: which tools are available is turn state, and the next round's offer has
/// to see what this round loaded.
fn run_tool(
    call: &ToolCall,
    eager: &BTreeSet<String>,
    loaded: &mut BTreeSet<String>,
) -> Message {
    let args: serde_json::Value =
        serde_json::from_str(&call.arguments).unwrap_or(serde_json::Value::Null);

    // A tool that was not offered is not run, however well the call is formed.
    //
    // Models call tools they were never given. Asked to list files with only
    // the loader on offer, gemma4 emitted `list_objects` carrying the loader's
    // own arguments, and it worked -- that tool needs none, so the stray key
    // was ignored. It then wrote a file with `content_bytes` and `object_name`,
    // names it had never read, and reported success for a write that failed.
    // Running a guess makes the deferral cosmetic: the prompt gets smaller
    // while nothing is actually withheld.
    //
    // Refused rather than loaded on the model's behalf, because arguments
    // written without the schema they are meant to satisfy are not worth
    // honouring, and loading here would reward the guess.
    let content = if !is_offered(&call.name, eager, loaded) {
        // Unless it is no tool at all but a skill's operation, where sending
        // the model to the loader is sending it round a loop.
        let error = operations_hint(std::slice::from_ref(&call.name))
            .unwrap_or_else(|| format!("{} is not loaded. Call {LOAD_TOOLS} first.", call.name));
        serde_json::json!({ "error": error }).to_string()
    } else {
        match call.name.as_str() {
        LOAD_TOOLS => load_tools(&args, eager, loaded),
        READ_OBJECT => read_object(&args),
        WRITE_OBJECT => write_object(&args),
        DELETE_OBJECT => delete_object(&args),
        LIST_OBJECTS => list_objects(&args),
        FETCH => fetch_url(&args),
        DESCRIBE_IMAGE => describe_image(&args),
        EXPAND_ARCHIVE => expand_archive(&args),
        CREATE_ARCHIVE => create_archive(&args),
        CURRENT_TIME => {
            let clock = host::current_time();
            // The weekday is given rather than left to be worked out: a model
            // doing calendar arithmetic on a date gets it wrong often enough
            // to matter, and the host already knows the answer exactly.
            format!(
                r#"{{"now":"{}","weekday":"{}","timezone":"{}","abbreviation":"{}"}}"#,
                clock.now,
                clock.weekday,
                if clock.timezone.is_empty() { "UTC" } else { &clock.timezone },
                clock.abbreviation,
            )
        }
        // Reported to the model rather than failing the turn: it can recover
        // by answering without the tool, where an error ends the conversation.
        other => {
            let error = match operations_hint(&[other.to_string()]) {
                Some(hint) => hint,
                None => format!("no such tool: {other}"),
            };
            serde_json::json!({ "error": error }).to_string()
        }
        }
    };

    // The reader gets everything; the model gets what fits. Cutting it down
    // happens here and once: what the model is handed now is what it will be
    // shown on every later turn, so the two must be the same string.
    //
    // A failure is a result whose top-level object carries `error`. Matching
    // the substring instead flagged any file that happened to contain the
    // word, and a read of an error log was reported as the read having failed.
    let is_error = serde_json::from_str::<serde_json::Value>(&content)
        .ok()
        .and_then(|v| v.get("error").map(|e| !e.is_null()))
        .unwrap_or(false);
    let for_model = for_the_model(&content);
    host::tool_finished(&ToolOutcome {
        id: call.id.clone(),
        details: content.clone(),
        content: for_model.clone(),
        is_error,
    });

    Message {
        role: "tool".to_string(),
        parts: vec![ContentPart::Text(for_model)],
        tool_call_id: Some(call.id.clone()),
    }
}

/// Separates the user-facing label from the arguments the model gets back.
///
/// Returns the label and the arguments without it. Unparseable arguments are
/// passed through untouched: the model wrote them, and a tool that rejects
/// them gives a better error than the guest silently rewriting them.
fn split_action(arguments: &str) -> (String, String) {
    let Ok(serde_json::Value::Object(mut object)) =
        serde_json::from_str::<serde_json::Value>(arguments)
    else {
        return (String::new(), arguments.to_string());
    };

    let action = match object.remove(ACTION) {
        Some(serde_json::Value::String(action)) => action,
        _ => String::new(),
    };
    let rest = serde_json::to_string(&object).unwrap_or_else(|_| "{}".to_string());
    (action, rest)
}

impl Guest for Component {
    fn run(conversation: Vec<Message>, system_prompt: String) -> Result<String, String> {
        // The system prompt leads the conversation rather than being stored
        // with it, so editing an agent takes effect on its next turn instead
        // of only on new sessions.
        // One line, and deliberately only one: it is paid for on every round of
        // every turn forever. The model is otherwise never told what ends a
        // turn -- it writes "I am about to look that up" as a preamble, we read
        // the missing tool call as "finished", and the user is left holding a
        // promise nobody kept.
        const TURN_RULE: &str = "A reply that contains no tool calls ends the turn.";

        let mut messages = Vec::with_capacity(conversation.len() + 1);
        let system = if system_prompt.is_empty() {
            TURN_RULE.to_string()
        } else {
            format!("{system_prompt}\n\n{TURN_RULE}")
        };
        messages.push(text_message("system", system));
        messages.extend(conversation);

        if messages.iter().all(|m| m.role != "user") {
            return Err("conversation contains nothing to respond to".to_string());
        }

        // Everything the model says across every round, which is exactly
        // what the user watched arrive. The host streams each call as it
        // happens and cannot take it back, so the reply has to account for
        // all of it or the browser ends up holding text the transcript does
        // not have.
        let mut reply = String::new();

        // Asked of the host rather than decided here: the limit is the
        // platform's to set, and the host enforces it whether or not a guest
        // bothers to look. Reading it is what allows a graceful ending instead
        // of being refused mid-loop.
        let max_rounds = host::current_limits().max_tool_rounds;

        // The deployment's eager set, read once: it cannot change mid-turn.
        let eager = eager_tools();

        // Which deferred tools are loaded, seeded from what earlier turns in
        // this conversation already loaded and added to as this turn loads
        // more. Session state rather than turn state: a model that read a
        // schema four turns ago still has it in front of it, and refusing the
        // call it then makes costs a round to reload something it never
        // forgot.
        let mut loaded = already_loaded(&messages, &eager);

        let mut round: u32 = 0;
        loop {
            // Asked again every round rather than once at the start, because
            // the thing worth knowing arrives mid-turn: somebody presses stop
            // while the model is already writing.
            if host::current_limits().cancelled {
                // What has been written is kept. The words already streamed
                // are on the reader's screen, and a stop that erased them
                // would be a worse answer to "stop" than leaving them there.
                host::log("info", "the turn was asked to stop; ending here");
                return Ok(reply);
            }

            // On the last permitted round tools are withheld, so the model
            // must answer in prose rather than asking for something it cannot
            // be given -- otherwise the turn ends with nothing to show. Zero
            // means unbounded, so that round never arrives.
            let exhausted = max_rounds > 0 && round + 1 >= max_rounds;

            let completion = host::chat(&CompletionRequest {
                messages: messages.clone(),
                tools: if exhausted { Vec::new() } else { offered_tools(&eager, &loaded) },
                model: None,
                temperature: None,
                max_tokens: None,
            })?;

            // Rounds are joined with a blank line. The host streams the same
            // blank line before this round's first token whenever an earlier
            // round has shown text, so the reply assembled here matches what
            // the browser has already been shown. It is not emitted through
            // `progress` from here: by the time this code runs, the round's
            // text has already streamed, and a separator sent now would land
            // after it.
            let round_text = text_of(&completion.parts);
            let round_calls = calls_of(&completion.parts);

            if !round_text.is_empty() {
                if !reply.is_empty() {
                    reply.push_str("\n\n");
                }
                reply.push_str(&round_text);
            }

            if round_calls.is_empty() {
                // The turn would end here. Anything the user has said since it
                // began extends it instead of being answered separately --
                // which is what makes a follow-up feel like part of the same
                // exchange rather than a new one.
                let waiting = host::pending_input();
                if !waiting.is_empty() {
                    messages.push(Message {
                        role: "assistant".to_string(),
                        parts: completion.parts,
                        tool_call_id: None,
                    });
                    messages.extend(injected(&waiting));
                    round += 1;
                    continue;
                }
                return Ok(reply);
            }

            // A reply that stopped before it said it had finished may carry
            // tool calls whose arguments were cut mid-JSON. Some will still
            // parse -- into something the model never meant -- so none of them
            // are run. The model is told instead, and can ask again.
            //
            // Two ways to arrive here. "length" is the token limit, which the
            // provider names. No finish reason at all is a stream that ended
            // without one, which is what a cancelled turn looks like from
            // inside: the gateway cut the connection partway through a round.
            //
            // The second matters more than the first. The round boundary below
            // is where a turn stops safely, and reaching it having already run
            // a tool with half-written arguments is precisely the harm that
            // boundary exists to prevent -- a file written, a request sent,
            // nobody able to say with what.
            let truncated = completion.finish_reason.as_deref() == Some("length")
                || (completion.finish_reason.is_none() && !round_calls.is_empty());
            if truncated {
                host::log("warn", "reply was cut short; refusing its tool calls");
                messages.push(Message {
                    role: "assistant".to_string(),
                    parts: completion.parts.clone(),
                    tool_call_id: None,
                });
                let why = if completion.finish_reason.is_none() {
                    r#"{"error":"not run: the reply was cut short and these arguments may be incomplete"}"#
                } else {
                    r#"{"error":"not run: the message was cut off at the token limit and these arguments may be incomplete"}"#
                };
                for call in &round_calls {
                    messages.push(Message {
                        role: "tool".to_string(),
                        parts: vec![ContentPart::Text(why.to_string())],
                        tool_call_id: Some(call.id.clone()),
                    });
                }
                round += 1;
                continue;
            }

            host::log(
                "info",
                &format!("running {} tool call(s)", round_calls.len()),
            );

            // Announced before running, so the browser shows what is happening
            // while it happens rather than explaining it afterwards.
            let mut echoed = Vec::with_capacity(round_calls.len());
            for call in &round_calls {
                let (action, without_action) = split_action(&call.arguments);
                host::tool_started(&ToolActivity {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    action,
                    arguments: without_action.clone(),
                });
                echoed.push(ToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: without_action,
                });
            }

            // The model's own request has to go back into the conversation
            // before its answers do, or the results refer to a call the model
            // cannot see. It goes back without the label: that was written
            // for the user, and replaying it would pay for those tokens on
            // every subsequent turn.
            let mut echoed = echoed.into_iter();
            messages.push(Message {
                role: "assistant".to_string(),
                parts: completion
                    .parts
                    .iter()
                    .map(|p| match p {
                        ContentPart::Text(t) => ContentPart::Text(t.clone()),
                        // Replaced in place, so a call keeps its position
                        // among the text rather than being moved to the end.
                        ContentPart::Call(_) => match echoed.next() {
                            Some(c) => ContentPart::Call(c),
                            None => ContentPart::Text(String::new()),
                        },
                    })
                    .collect(),
                tool_call_id: None,
            });
            for call in &round_calls {
                messages.push(run_tool(call, &eager, &mut loaded));
            }

            // Injected after the results and before the next model call: the
            // only point in the loop where the conversation is consistent and
            // nothing is half-done.
            messages.extend(injected(&host::pending_input()));

            round += 1;
            if exhausted {
                // The round that withheld tools still asked for them, which
                // means the model ignored their absence. Nothing further can
                // be offered, so the turn ends with whatever was said.
                return Ok(reply);
            }
        }
    }
}

bindings::export!(Component with_types_in bindings);
