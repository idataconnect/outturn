//! The declaration at the top of a skill's operation file.
//!
//! See `docs/approvals.md` for what the keys mean and `docs/skill-packages.md`
//! for why a skill file is a safe place to put a governance rule -- versioned
//! with its body, immutable once published, written under `SkillsWrite`, and
//! read-only to the agent it governs.
//!
//! Frontmatter is for the *platform*, not the model. The prose below the fences
//! is what an agent is told; a declaration up here is what this tier acts on.
//! Anything a model needs to know belongs in the body where it can see it.
//!
//! Two headings are understood: `approval`, and `operation` -- what request an
//! operation is, so the agent can be offered it as a tool (see
//! `docs/skill-operations.md`).
//!
//! Parsed by hand rather than with a YAML crate. The contract is scalar keys
//! and flat lists under a heading, and it is deliberately that narrow: a full YAML
//! reader would accept anchors, aliases, multi-document streams and nested
//! collections, none of which mean anything here, and every one of which would
//! be a shape somebody eventually writes and expects to work. What this accepts
//! is what `docs/approvals.md` documents, and it refuses the rest by not
//! understanding it.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

/// What an operation's file declares about needing approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRule {
    /// The act, in a word. Compared rather than read: two operations declaring
    /// the same act are the same act for the purposes of one grant.
    pub requires: String,
    /// The request this applies to, as the gateway will see it: a method, a
    /// space, and a path. `POST /charges`, or `DELETE /bookings/*` where a
    /// trailing star stands for anything below.
    ///
    /// Declared rather than read out of the prose below. The body says
    /// `fetch_url with POST http://.../charges` because that is what a model
    /// needs; deriving a security gate by parsing that sentence would make the
    /// gate depend on how somebody phrased a paragraph. Absent means the rule
    /// cannot be enforced automatically, which is refused: a declaration that
    /// looks like a gate and gates nothing is the failure this whole module is
    /// arranged against.
    pub matches: String,
    /// The unit a wider grant may span, when the approver ticks it. Absent
    /// means an approval covers this call and its retries and nothing else,
    /// which is the default because it needs no judgment from the approver.
    pub covers: Option<String>,
    /// Which field of the request names that unit. Required when `covers` is
    /// present and meaningless without it.
    pub identified_by: Option<String>,
    /// The request-body fields a person approving this is really approving, in
    /// declared order.
    ///
    /// What a `call` grant is keyed on. Required, because a grant keyed on less
    /// than what made the request distinctive covers requests nobody looked at:
    /// keyed on the path alone, a yes to charging £40 let £4,000 through. See
    /// `docs/approvals.md`.
    pub binds: Vec<String>,
}

/// The request an operation is, declared so the agent can be offered it as a
/// tool and the request built for it. See `docs/skill-operations.md`.
///
/// Arguments are flat lists of names rather than a schema: a schema would mean
/// nested collections, which this parser refuses to read, and the names with a
/// type where one is not a string are what a tool's parameters need.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operation {
    /// Upper case.
    pub method: String,
    /// Absolute, `http` or `https`, with a path parameter as `{name}` in its
    /// path. No query string: that is `query`.
    pub url: String,
    /// The JSON body's fields.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub body: Vec<String>,
    /// The query string's parameters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub query: Vec<String>,
    /// Body or query fields that may be left out. Everything else is required,
    /// path parameters always.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub optional: Vec<String>,
    /// Body or query fields by type, where the type is not a string.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub integer: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub number: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boolean: Vec<String>,
    /// Body or query fields that are lists of their type: `tags` in a query
    /// as `?tags=a&tags=b`, or a JSON array in a body. A list of anything but
    /// scalars is not something a flat declaration can say.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub list: Vec<String>,
    /// The arguments as JSON Schema, where the flat lists above cannot say
    /// them -- nested objects, enums. Written in the file with references to
    /// the skill's shared types (`{"$ref": "#/types/Category"}`, from its
    /// `types.json`), and stored with them expanded at publish, since a
    /// provider takes each tool's schema standing alone. See
    /// `docs/skill-operations.md`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<serde_json::Value>,
    /// The body or query fields whose values together are what this write
    /// is: a booking is its room, guest and dates. The host derives an
    /// idempotency key from them, so the same write sent twice is one write.
    /// Empty for an operation whose repetition may be meant. See
    /// `docs/idempotency.md`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identity: Vec<String>,
    /// Where the recipient takes that key, if it does: `header Idempotency-Key`
    /// or `field idempotency_key`. Absent, the host keeps the record itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<IdempotencyKey>,
    /// What the operation is, for the tool's description: the first paragraph
    /// of the prose below the fence. Taken rather than declared, because the
    /// prose already says it and a second copy is one to fall out of date.
    #[serde(default)]
    pub summary: String,
}

/// Where a recipient takes an idempotency key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdempotencyKey {
    Header(String),
    Field(String),
}

impl Operation {
    /// The tool's arguments as JSON Schema: the declared one, or the one the
    /// flat lists describe. One shape for the guest to read either way.
    pub fn arguments_schema(&self) -> serde_json::Value {
        if let Some(schema) = &self.schema {
            return schema.clone();
        }
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();
        for name in self
            .path_params()
            .iter()
            .chain(&self.body)
            .chain(&self.query)
        {
            let kind = if self.integer.contains(name) {
                "integer"
            } else if self.number.contains(name) {
                "number"
            } else if self.boolean.contains(name) {
                "boolean"
            } else {
                "string"
            };
            let schema = if self.list.contains(name) {
                serde_json::json!({ "type": "array", "items": { "type": kind } })
            } else {
                serde_json::json!({ "type": kind })
            };
            properties.insert(name.clone(), schema);
            if !self.optional.contains(name) {
                required.push(name.clone());
            }
        }
        serde_json::json!({ "type": "object", "properties": properties, "required": required })
    }

    /// The URL's path parameters, in the order they appear.
    pub fn path_params(&self) -> Vec<String> {
        placeholders(url_path(&self.url)).unwrap_or_default()
    }
}

/// What a file's frontmatter said, and the body below it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed<'a> {
    pub approval: Option<ApprovalRule>,
    pub operation: Option<Operation>,
    /// Everything after the closing fence. The whole file when there was no
    /// frontmatter, which is nearly every file.
    pub body: &'a str,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FrontmatterError {
    #[error("the frontmatter was opened with --- and never closed")]
    Unterminated,
    #[error("{0}")]
    Invalid(String),
}

/// Splits a file into what it declares and what it says.
///
/// A file not beginning with `---` has no frontmatter and is all body, which is
/// the common case and not an error. Nor is frontmatter that declares nothing
/// this understands: a key we do not know is left alone rather than refused, so
/// a file written for a later version of this platform still reads as prose
/// here. What *is* refused is a declaration that is present and malformed --
/// `approval:` with no `requires`, or `covers` with nothing to identify the unit
/// by -- because a rule nobody can act on is worse than no rule, and silently
/// dropping one means an operation that was meant to be gated is not.
pub fn parse(source: &str) -> Result<Parsed<'_>, FrontmatterError> {
    let Some(rest) = strip_open_fence(source) else {
        return Ok(Parsed {
            approval: None,
            operation: None,
            body: source,
        });
    };

    let Some((yaml, body)) = split_at_close_fence(rest) else {
        return Err(FrontmatterError::Unterminated);
    };

    let approval = approval_from(yaml)?;
    let operation = operation_from(yaml)?.map(|op| Operation {
        summary: first_paragraph(body),
        ..op
    });
    if let (Some(rule), Some(op)) = (&approval, &operation) {
        gate_fits_operation(rule, op)?;
    }
    Ok(Parsed {
        approval,
        operation,
        body,
    })
}

/// The text after an opening `---` line, or `None` when there is not one.
///
/// The fence has to be the very first thing: a `---` further down is a
/// horizontal rule in somebody's prose, and reading it as a fence would eat
/// half their file.
fn strip_open_fence(source: &str) -> Option<&str> {
    let rest = source.strip_prefix("---")?;
    // Only a newline may follow, so `----` and `--- x` are not fences.
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))?;
    Some(rest)
}

/// Splits at the closing `---` line, returning the YAML and the body.
fn split_at_close_fence(rest: &str) -> Option<(&str, &str)> {
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            let body = &rest[offset + line.len()..];
            return Some((&rest[..offset], body));
        }
        offset += line.len();
    }
    // A file that is nothing but frontmatter and never closes it.
    None
}

/// The `key: value` lines under one top-level heading, in order, or `None`
/// when the heading is not there.
///
/// Deliberately not a general YAML reader: one heading at column zero, and
/// `key: value` lines indented under it. Anything else under the heading is
/// refused rather than ignored, because a declaration half-read is one
/// half-applied. Values come back unquoted and never empty.
/// A block's keys and values, in the order written.
type Entries<'y> = Vec<(&'y str, Cow<'y, str>)>;

fn block<'y>(yaml: &'y str, heading: &str) -> Result<Option<Entries<'y>>, FrontmatterError> {
    let mut inside = false;
    let mut found: Option<Entries<'y>> = None;
    let mut lines = yaml.lines().peekable();

    while let Some(raw) = lines.next() {
        let line = raw.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }

        let indented = line.starts_with(' ') || line.starts_with('\t');
        if !indented {
            // A new top-level key ends the block. Split at the colon and trim
            // the key rather than matching a literal `approval:`: YAML accepts
            // `approval :`, and comparing against the literal left that spelling
            // unrecognized, its children skipped, and the operation ungated with
            // nothing said -- which is the one outcome this module exists to
            // prevent.
            let (key, value) = match line.split_once(':') {
                Some((key, value)) => (key.trim(), value.trim()),
                // Not a key at all. A stray line in frontmatter is somebody
                // else's business, not ours.
                None => {
                    inside = false;
                    continue;
                }
            };
            inside = key == heading;
            // A block written as a scalar is not the shape, and skipping it as an
            // unknown key would leave a gated operation ungated.
            if inside && !value.is_empty() {
                return Err(FrontmatterError::Invalid(format!(
                    "{heading} takes a block of keys, not a value"
                )));
            }
            if inside {
                if found.is_some() {
                    return Err(FrontmatterError::Invalid(format!(
                        "{heading} is given twice"
                    )));
                }
                found = Some(Vec::new());
            }
            continue;
        }
        if !inside {
            continue;
        }

        let Some((key, value)) = line.split_once(':') else {
            return Err(FrontmatterError::Invalid(format!(
                "{} is not a key and a value",
                line.trim()
            )));
        };
        let key = key.trim();
        let value = if value.trim() == "|" {
            // A block of text: every following line indented past the key's,
            // with the indentation they share taken off. The one value that
            // spans lines -- an operation's schema, which is JSON and wants to
            // be read laid out rather than on one line.
            let own = line.len() - line.trim_start().len();
            let mut taken: Vec<&str> = Vec::new();
            while let Some(next) = lines.peek() {
                let depth = next.len() - next.trim_start().len();
                if !next.trim().is_empty() && depth <= own {
                    break;
                }
                taken.push(next.trim_end());
                lines.next();
            }
            let common = taken
                .iter()
                .filter(|l| !l.trim().is_empty())
                .map(|l| l.len() - l.trim_start().len())
                .min()
                .unwrap_or(0);
            let text: Vec<&str> = taken
                .iter()
                .map(|l| l.get(common..).unwrap_or(""))
                .collect();
            Cow::Owned(text.join("\n").trim().to_string())
        } else {
            Cow::Borrowed(unquote(value.trim())?)
        };
        if value.is_empty() {
            return Err(FrontmatterError::Invalid(format!("{key} has no value")));
        }
        // Two values for one key is a contradiction, and picking one means a
        // reviewer who read the first has approved something else. Refused
        // rather than resolved.
        let entries = found.as_mut().expect("inside a block that was found");
        if entries.iter().any(|(k, _)| *k == key) {
            return Err(FrontmatterError::Invalid(format!("{key} is given twice")));
        }
        entries.push((key, value));
    }
    Ok(found)
}

/// Reads the `approval:` block, if there is one.
fn approval_from(yaml: &str) -> Result<Option<ApprovalRule>, FrontmatterError> {
    let mut requires = None;
    let mut matches = None;
    let mut covers = None;
    let mut identified_by = None;
    let mut binds: Option<Vec<String>> = None;

    for (key, value) in block(yaml, "approval")?.unwrap_or_default() {
        let slot = match key {
            "requires" => &mut requires,
            "matches" => &mut matches,
            "covers" => &mut covers,
            "identified_by" => &mut identified_by,
            // A list rather than a scalar, so it is taken here and the scalar
            // slots above are left for the keys that are one value.
            "binds" => {
                binds = Some(parse_list(key, &value)?);
                continue;
            }
            other => {
                return Err(FrontmatterError::Invalid(format!(
                    "{other} is not a key of approval"
                )));
            }
        };
        *slot = Some(value.to_string());
    }

    let Some(requires) = requires else {
        // The block was opened and said nothing that names the act.
        if matches.is_some() || covers.is_some() || identified_by.is_some() || binds.is_some() {
            return Err(FrontmatterError::Invalid(
                "approval needs a requires saying what is being asked".into(),
            ));
        }
        // Nothing was declared. Which is only safe to conclude if the word does
        // not appear at all: a declaration this parser failed to recognize --
        // nested under another key, or in a shape nobody anticipated -- would
        // otherwise leave an operation meant to be gated ungated, silently. So
        // the presence of the word with no rule to show for it is refused.
        if yaml.contains("approval") {
            return Err(FrontmatterError::Invalid(
                "this file mentions approval but declares no rule this understands; \
                 an approval block belongs at the top level of the frontmatter"
                    .into(),
            ));
        }
        return Ok(None);
    };

    // A unit nobody can identify cannot be keyed on, so a grant for it would
    // silently become a grant for every call -- which is the widening
    // docs/approvals.md is most careful about.
    if covers.is_some() && identified_by.is_none() {
        return Err(FrontmatterError::Invalid(
            "covers needs an identified_by naming the field that identifies the unit".into(),
        ));
    }
    if identified_by.is_some() && covers.is_none() {
        return Err(FrontmatterError::Invalid(
            "identified_by means nothing without a covers".into(),
        ));
    }

    // A rule with nothing to match is a rule the gateway cannot apply. Refused
    // rather than stored, because a file that declares an approval and gates
    // nothing is worse than one that declares none: it says the operation is
    // gated, and a reader believes it.
    let Some(matches) = matches else {
        return Err(FrontmatterError::Invalid(
            "approval needs a matches saying which request it applies to, e.g. `POST /charges`"
                .into(),
        ));
    };
    check_matches(&matches)?;

    let binds = binds.unwrap_or_default();
    if binds.is_empty() {
        // Refused for the same reason `matches` is: a rule half-applied is an
        // operation the file says is gated and the platform does not gate. An
        // approval with nothing bound would produce a grant covering every
        // request to that path, which is a yes to one charge covering the next.
        return Err(FrontmatterError::Invalid(
            "approval needs binds naming the request fields the approval is about".into(),
        ));
    }

    Ok(Some(ApprovalRule {
        requires,
        binds,
        matches,
        covers,
        identified_by,
    }))
}

/// Reads the `operation:` block, if there is one.
///
/// Refused when malformed, as `approval` is, though for a gentler reason: an
/// operation half-read is a tool offered with the wrong arguments, which the
/// model then calls and the recipient refuses, and nobody is told why.
fn operation_from(yaml: &str) -> Result<Option<Operation>, FrontmatterError> {
    let Some(entries) = block(yaml, "operation")? else {
        return Ok(None);
    };
    let mut method = None;
    let mut url = None;
    let mut schema = None;
    let mut identity: Vec<String> = Vec::new();
    let mut idempotency_key = None;
    let mut lists: [(&str, Vec<String>); 7] = [
        ("body", Vec::new()),
        ("query", Vec::new()),
        ("optional", Vec::new()),
        ("integer", Vec::new()),
        ("number", Vec::new()),
        ("boolean", Vec::new()),
        ("list", Vec::new()),
    ];
    for (key, value) in entries {
        match key {
            "method" => method = Some(value.to_ascii_uppercase()),
            "url" => url = Some(value.to_string()),
            "identity" => identity = parse_list(key, &value)?,
            "key" => {
                idempotency_key = Some(match value.split_once(' ') {
                    Some(("header", name)) if !name.trim().is_empty() => {
                        IdempotencyKey::Header(name.trim().to_string())
                    }
                    Some(("field", name)) if !name.trim().is_empty() => {
                        IdempotencyKey::Field(name.trim().to_string())
                    }
                    _ => {
                        return Err(FrontmatterError::Invalid(format!(
                            "key is `header <name>` or `field <name>`, not {value}"
                        )));
                    }
                })
            }
            "schema" => {
                let parsed: serde_json::Value = serde_json::from_str(&value)
                    .map_err(|e| FrontmatterError::Invalid(format!("schema is not JSON: {e}")))?;
                if parsed.get("type").and_then(|t| t.as_str()) != Some("object")
                    || !parsed.get("properties").is_some_and(|p| p.is_object())
                {
                    return Err(FrontmatterError::Invalid(
                        "schema is an object with properties: the tool's arguments".into(),
                    ));
                }
                schema = Some(parsed);
            }
            other => {
                let Some((_, list)) = lists.iter_mut().find(|(name, _)| *name == other) else {
                    return Err(FrontmatterError::Invalid(format!(
                        "{other} is not a key of operation"
                    )));
                };
                *list = parse_list(key, &value)?;
            }
        }
    }
    let [
        (_, body),
        (_, query),
        (_, optional),
        (_, integer),
        (_, number),
        (_, boolean),
        (_, list),
    ] = lists;

    let Some(method) = method else {
        return Err(FrontmatterError::Invalid(
            "operation needs a method, e.g. `POST`".into(),
        ));
    };
    if !outturn_core::egress::gate::METHODS.contains(&method.as_str()) {
        return Err(FrontmatterError::Invalid(format!(
            "{method} is not a method an operation can use"
        )));
    }
    let Some(url) = url else {
        return Err(FrontmatterError::Invalid(
            "operation needs a url saying where the request goes".into(),
        ));
    };
    check_url(&url)?;
    if !body.is_empty() && matches!(method.as_str(), "GET" | "HEAD") {
        return Err(FrontmatterError::Invalid(format!(
            "a {method} has no body; its arguments go in query"
        )));
    }

    // One name, one place. A field that is both in the body and the query, or
    // both a path parameter and either, is sent twice or not at all, and which
    // is a guess.
    let path = placeholders(url_path(&url))?;
    let mut seen: Vec<&str> = Vec::new();
    for name in path.iter().chain(&body).chain(&query) {
        if seen.contains(&name.as_str()) {
            return Err(FrontmatterError::Invalid(format!(
                "{name} is named twice among the path, body and query"
            )));
        }
        seen.push(name);
    }
    // What qualifies a field has to name one the request carries. A path
    // parameter is always a required string, so it is not among them.
    let carried = |name: &String| body.contains(name) || query.contains(name);
    for (key, names) in [
        ("optional", &optional),
        ("integer", &integer),
        ("number", &number),
        ("boolean", &boolean),
        ("list", &list),
    ] {
        if let Some(stray) = names.iter().find(|n| !carried(n)) {
            return Err(FrontmatterError::Invalid(format!(
                "{key} names {stray}, which is not in body or query"
            )));
        }
    }
    let typed: Vec<&String> = integer.iter().chain(&number).chain(&boolean).collect();
    if let Some(twice) = typed
        .iter()
        .enumerate()
        .find(|(i, n)| typed[..*i].contains(n))
        .map(|(_, n)| n)
    {
        return Err(FrontmatterError::Invalid(format!(
            "{twice} is given more than one type"
        )));
    }

    // With a schema, it says the types and what is required, and the body is
    // whatever it names that the path and the query do not.
    let mut body = body;
    if let Some(schema) = &schema {
        if !(optional.is_empty() && integer.is_empty() && number.is_empty())
            || !(boolean.is_empty() && list.is_empty())
        {
            return Err(FrontmatterError::Invalid(
                "with a schema, the schema says which fields are optional and what type each is"
                    .into(),
            ));
        }
        let props: Vec<&String> = schema["properties"]
            .as_object()
            .map(|p| p.keys().collect())
            .unwrap_or_default();
        if let Some(missing) = path.iter().chain(&query).find(|n| !props.contains(n)) {
            return Err(FrontmatterError::Invalid(format!(
                "{missing} is in the url or query but not in the schema"
            )));
        }
        let rest: Vec<String> = props
            .into_iter()
            .filter(|p| !path.contains(p) && !query.contains(p))
            .cloned()
            .collect();
        if !body.is_empty() && body != rest {
            return Err(FrontmatterError::Invalid(
                "with a schema, the body is what it names beyond the path and query; leave body out"
                    .into(),
            ));
        }
        if !rest.is_empty() && matches!(method.as_str(), "GET" | "HEAD") {
            return Err(FrontmatterError::Invalid(format!(
                "a {method} has no body, so {} has to be in query",
                rest.join(", ")
            )));
        }
        body = rest;
    }

    // An identity is fields the request carries; a key goes nowhere without
    // one, and is meaningless on a read.
    if let Some(stray) = identity
        .iter()
        .find(|n| !body.contains(n) && !query.contains(n))
    {
        return Err(FrontmatterError::Invalid(format!(
            "identity names {stray}, which is not in the body or query"
        )));
    }
    if idempotency_key.is_some() && identity.is_empty() {
        return Err(FrontmatterError::Invalid(
            "key says where an idempotency key goes; identity says what it is made of".into(),
        ));
    }
    if !identity.is_empty() && matches!(method.as_str(), "GET" | "HEAD") {
        return Err(FrontmatterError::Invalid(format!(
            "a {method} is a read, and a read is never recorded"
        )));
    }

    Ok(Some(Operation {
        method,
        url,
        body,
        query,
        optional,
        integer,
        number,
        boolean,
        list,
        schema,
        identity,
        key: idempotency_key,
        summary: String::new(),
    }))
}

/// The first paragraph of prose, past any heading, on one line.
///
/// Capped, because it becomes a tool description sent with every request the
/// tool is offered in, and a file whose opening runs on would cost that on
/// every round.
fn first_paragraph(body: &str) -> String {
    const MAX: usize = 400;
    let paragraph: Vec<&str> = body
        .lines()
        .map(str::trim)
        .skip_while(|l| l.is_empty() || l.starts_with('#'))
        .take_while(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    let joined = paragraph.join(" ");
    match joined.char_indices().nth(MAX) {
        Some((cut, _)) => format!("{}...", joined[..cut].trim_end()),
        None => joined,
    }
}

/// Checks an operation's URL is absolute and carries no query of its own.
fn check_url(value: &str) -> Result<(), FrontmatterError> {
    // Placeholders are not valid URL characters, so they are swapped for a
    // stand-in before parsing; their shape is checked by `placeholders`.
    let plain: String = {
        let mut out = String::new();
        let mut inside = false;
        for c in value.chars() {
            match c {
                '{' => inside = true,
                '}' => {
                    inside = false;
                    out.push('x');
                }
                _ if inside => {}
                c => out.push(c),
            }
        }
        out
    };
    let parsed = url::Url::parse(&plain)
        .map_err(|e| FrontmatterError::Invalid(format!("{value} is not a URL: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(FrontmatterError::Invalid(format!(
            "{value} is not http or https"
        )));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(FrontmatterError::Invalid(format!(
            "{value} carries a query or fragment; query parameters go in query"
        )));
    }
    if value[..value.find("://").map_or(0, |i| i + 3)].contains('{')
        || url_authority(value).contains('{')
    {
        return Err(FrontmatterError::Invalid(format!(
            "{value} has a parameter outside its path; only the path may have one"
        )));
    }
    Ok(())
}

/// The part of an absolute URL between `://` and the path.
fn url_authority(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split('/').next().unwrap_or("")
}

/// The path of an absolute URL, as written: placeholders left in.
fn url_path(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.find('/').map_or("/", |i| &rest[i..])
}

/// The `{name}` parameters in a path, in order.
///
/// A parameter is a whole segment: `/bookings/{id}`, not `/bookings/bk_{id}`.
/// Part of a segment would have to be matched by a gate's pattern, which can
/// only end in a star.
fn placeholders(path: &str) -> Result<Vec<String>, FrontmatterError> {
    let mut names = Vec::new();
    for segment in path.split('/') {
        if !segment.contains('{') && !segment.contains('}') {
            continue;
        }
        let name = segment
            .strip_prefix('{')
            .and_then(|s| s.strip_suffix('}'))
            .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
            .ok_or_else(|| {
                FrontmatterError::Invalid(format!(
                    "{segment} is not a path parameter; one is a whole segment, like {{id}}"
                ))
            })?;
        names.push(name.to_string());
    }
    Ok(names)
}

/// Checks a gate in the same file as an operation applies to that operation.
///
/// Both are written by hand in one file, so they can disagree: a gate on
/// `POST /charges` beside an operation posting to `/charge` gates nothing the
/// agent sends. Refused at publish, where whoever wrote it finds out, rather
/// than by a charge nobody approved.
fn gate_fits_operation(rule: &ApprovalRule, op: &Operation) -> Result<(), FrontmatterError> {
    let (method, pattern) = rule
        .matches
        .split_once(' ')
        .map(|(m, p)| (m.trim().to_ascii_uppercase(), p.trim()))
        .unwrap_or_default();
    let path = url_path(&op.url);
    let fits = method == op.method
        && match pattern.strip_suffix('*') {
            Some(prefix) => path.starts_with(prefix),
            // Without a star the gate matches one literal path, which a path
            // with a parameter in it never is.
            None => pattern == path && !path.contains('{'),
        };
    if !fits {
        return Err(FrontmatterError::Invalid(format!(
            "the approval matches {}, which is not this operation's {} {path}",
            rule.matches, op.method
        )));
    }
    // A grant is keyed on body fields. One the operation never sends would be
    // keyed on nothing, and cover every request.
    for field in rule.binds.iter().chain(&rule.identified_by) {
        if !op.body.contains(field) {
            return Err(FrontmatterError::Invalid(format!(
                "the approval binds {field}, which is not in this operation's body"
            )));
        }
    }
    Ok(())
}

/// Checks a `matches` is a method and an absolute path.
///
/// Narrow on purpose. A star is allowed only at the very end, because a pattern
/// that can match in the middle is one somebody writes `*` into and gates far
/// more than they meant -- and a gate that is too wide refuses work nobody
/// intended to gate, which reads as the platform being broken rather than as a
/// rule being wrong.
fn check_matches(value: &str) -> Result<(), FrontmatterError> {
    let Some((method, path)) = value.split_once(' ') else {
        return Err(FrontmatterError::Invalid(format!(
            "{value} is not a method and a path, e.g. `POST /charges`"
        )));
    };
    let method = method.trim();
    let path = path.trim();

    // The one list, in `egress::gate`. A method this validator accepts and the
    // gateway will not send is a rule that can never fire; one it refuses that the
    // gateway would send is an operation nobody can gate.
    if !outturn_core::egress::gate::METHODS.contains(&method.to_ascii_uppercase().as_str()) {
        return Err(FrontmatterError::Invalid(format!(
            "{method} is not a method this can gate"
        )));
    }
    if !path.starts_with('/') {
        return Err(FrontmatterError::Invalid(format!(
            "{path} is not an absolute path"
        )));
    }
    if path.trim_end_matches('*').contains('*') {
        return Err(FrontmatterError::Invalid(format!(
            "{path} has a star somewhere other than the end, which would gate more than it names"
        )));
    }
    Ok(())
}

/// Strips one layer of matching quotes, so `requires: "charge"` reads the same
/// as `requires: charge`.
///
/// Both ends have to be the same character and there have to be two of them: a
/// single `"` otherwise strips against itself and becomes a value, and an
/// unclosed `"charge` becomes the act `"charge`, which compares unequal to every
/// grant for `charge`. A typo that changes the act is worse than one that is
/// refused, because nothing about it looks wrong.
fn unquote(value: &str) -> Result<&str, FrontmatterError> {
    for quote in ['"', '\''] {
        if !value.starts_with(quote) && !value.ends_with(quote) {
            continue;
        }
        let inner = value
            .strip_prefix(quote)
            .filter(|rest| !rest.is_empty())
            .and_then(|rest| rest.strip_suffix(quote));
        return match inner {
            Some(inner) => Ok(inner),
            None => Err(FrontmatterError::Invalid(format!(
                "{value} opens a quote it does not close"
            ))),
        };
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_with_no_fence_is_all_body() {
        let parsed = parse("# charge_payment_account\n\nTakes money.").expect("parse");
        assert_eq!(parsed.approval, None);
        assert_eq!(parsed.body, "# charge_payment_account\n\nTakes money.");
    }

    #[test]
    fn a_rule_is_read_and_the_body_starts_after_the_fence() {
        let parsed = parse(
            "---\napproval:\n  requires: charge\n  matches: POST /charges\n  binds: [payment_account_id, amount_pence]\n  covers: booking\n  identified_by: booking_id\n---\n\n# charge\n",
        )
        .expect("parse");
        assert_eq!(
            parsed.approval,
            Some(ApprovalRule {
                requires: "charge".into(),
                matches: "POST /charges".into(),
                covers: Some("booking".into()),
                identified_by: Some("booking_id".into()),
                binds: vec!["payment_account_id".into(), "amount_pence".into()],
            })
        );
        assert_eq!(parsed.body, "\n# charge\n");
    }

    #[test]
    fn the_narrow_form_needs_only_requires() {
        let parsed =
            parse("---\napproval:\n  requires: charge\n  matches: POST /charges\n  binds: [amount_pence]\n---\nbody")
                .expect("parse");
        let rule = parsed.approval.expect("a rule");
        assert_eq!(rule.requires, "charge");
        assert_eq!(rule.covers, None);
    }

    #[test]
    fn covers_without_identified_by_is_refused() {
        // A unit nobody can identify cannot be keyed on, so the grant would
        // quietly become one for every call.
        let err =
            parse("---\napproval:\n  requires: charge\n  matches: POST /charges\n  binds: [amount_pence]\n  covers: booking\n---\n").unwrap_err();
        assert!(matches!(err, FrontmatterError::Invalid(_)));
    }

    #[test]
    fn identified_by_without_covers_is_refused() {
        let err = parse("---\napproval:\n  requires: charge\n  matches: POST /charges\n  binds: [amount_pence]\n  identified_by: booking_id\n---\n")
            .unwrap_err();
        assert!(matches!(err, FrontmatterError::Invalid(_)));
    }

    #[test]
    fn a_block_with_no_requires_is_refused_rather_than_ignored() {
        let err = parse("---\napproval:\n  covers: booking\n---\n").unwrap_err();
        assert!(matches!(err, FrontmatterError::Invalid(_)));
    }

    #[test]
    fn an_empty_block_is_refused() {
        // This used to be allowed, on the reasoning that a block saying nothing
        // declares nothing. That was the wrong way round: an `approval:` with
        // nothing under it is a rule somebody started writing far more often than
        // it is a deliberate statement of no rule, and reading it as the latter
        // leaves an operation ungated by a file that looks like it gates one. A
        // file that needs no approval says so by having no block.
        let err = parse("---\napproval:\n---\nbody").unwrap_err();
        assert!(matches!(err, FrontmatterError::Invalid(_)));
    }

    #[test]
    fn an_unknown_key_under_approval_is_refused() {
        // Under a block we act on, a key we do not know may be the difference
        // between gated and not.
        let err = parse(
            "---\napproval:\n  requires: charge\n  matches: POST /charges\n  binds: [amount_pence]\n  unless: friday\n---\n",
        )
        .unwrap_err();
        assert!(matches!(err, FrontmatterError::Invalid(_)));
    }

    #[test]
    fn an_unknown_top_level_key_is_left_alone() {
        // A file written for a later version of this platform still reads here.
        let parsed =
            parse("---\ntitle: Charging\napproval:\n  requires: charge\n  matches: POST /charges\n  binds: [amount_pence]\n---\nbody").expect("parse");
        let rule = parsed.approval.expect("a rule");
        assert_eq!(rule.requires, "charge");
        // The list too: a carriage return left on the end of the last field
        // would make it compare unequal to the field a request carries, and the
        // grant would be keyed on a name nothing matches.
        assert_eq!(rule.binds, vec!["amount_pence".to_string()]);
    }

    #[test]
    fn approval_as_a_scalar_is_refused() {
        // Otherwise it reads as a top-level key we do not know and is skipped,
        // and an operation meant to be gated is not.
        let err = parse("---\napproval: charge\n---\n").unwrap_err();
        assert!(matches!(err, FrontmatterError::Invalid(_)));
    }

    #[test]
    fn a_fence_that_never_closes_is_refused() {
        let err = parse("---\napproval:\n  requires: charge\n").unwrap_err();
        assert_eq!(err, FrontmatterError::Unterminated);
    }

    #[test]
    fn a_rule_further_down_the_file_is_prose() {
        // A `---` mid-file is a horizontal rule, and reading it as a fence
        // would eat half of somebody's document.
        let source = "# charge\n\n---\napproval:\n  requires: charge\n---\n";
        let parsed = parse(source).expect("parse");
        assert_eq!(parsed.approval, None);
        assert_eq!(parsed.body, source);
    }

    #[test]
    fn four_dashes_are_not_a_fence() {
        let parsed = parse("----\nnot frontmatter\n").expect("parse");
        assert_eq!(parsed.approval, None);
    }

    #[test]
    fn quotes_and_comments_are_tolerated() {
        let parsed = parse(
            "---\n# what this needs\napproval:\n  requires: \"charge\"\n  matches: POST /charges\n  binds: [amount_pence]\n  covers: 'booking'\n  identified_by: booking_id\n---\n",
        )
        .expect("parse");
        let rule = parsed.approval.expect("a rule");
        assert_eq!(rule.requires, "charge");
        assert_eq!(rule.covers.as_deref(), Some("booking"));
    }

    #[test]
    fn windows_line_endings_parse() {
        let parsed = parse(
            "---\r\napproval:\r\n  requires: charge\r\n  matches: POST /charges\r\n  binds: [amount_pence]\r\n---\r\nbody",
        )
        .expect("parse");
        assert_eq!(parsed.approval.expect("a rule").requires, "charge");
    }

    #[test]
    fn a_key_with_no_value_is_refused() {
        let err = parse("---\napproval:\n  requires:\n---\n").unwrap_err();
        assert!(matches!(err, FrontmatterError::Invalid(_)));
    }
}

#[cfg(test)]
mod against_the_real_files {
    use super::*;

    /// The declaration in the file the demo installs, parsed by this.
    ///
    /// Read from disk rather than pasted in, so an edit to the skill that broke
    /// the rule fails here rather than in a cluster. The path is relative to the
    /// crate root, which is where cargo runs tests from.
    #[test]
    fn hollowbrooks_charge_operation_declares_what_it_should() {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../k8s/components/hollowbrook/skill/charge_payment_account.md"
        ))
        .expect("the charge operation's file");
        let parsed = parse(&source).expect("its frontmatter parses");
        let rule = parsed.approval.expect("it declares an approval");
        assert_eq!(rule.requires, "charge");
        assert_eq!(rule.matches, "POST /charges");
        assert_eq!(rule.covers.as_deref(), Some("booking"));
        assert_eq!(rule.identified_by.as_deref(), Some("booking_id"));
        assert!(
            parsed
                .body
                .trim_start()
                .starts_with("# charge_payment_account"),
            "the body should start after the fence"
        );
    }

    /// And the operations that need no approval declare none -- but every
    /// operation declares the request it is, so the agent is offered it as a
    /// tool, and the manifest declares nothing.
    #[test]
    fn the_reads_declare_an_operation_and_no_approval() {
        for name in [
            "list_rooms",
            "get_room",
            "check_availability",
            "list_bookings",
            "get_booking",
            "list_payment_accounts",
        ] {
            let path = format!(
                "{}/../../k8s/components/hollowbrook/skill/{name}.md",
                env!("CARGO_MANIFEST_DIR")
            );
            let source = std::fs::read_to_string(&path).expect(&path);
            let parsed = parse(&source).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(parsed.approval, None, "{name} should need no approval");
            let op = parsed
                .operation
                .unwrap_or_else(|| panic!("{name} declares no operation"));
            assert_eq!(op.method, "GET", "{name}");
            assert!(!op.summary.is_empty(), "{name} has no summary for its tool");
            assert!(
                parsed.body.trim_start().starts_with(&format!("# {name}")),
                "{name}: the body should start after the fence"
            );
        }
    }

    #[test]
    fn the_writes_declare_what_they_send() {
        let read = |name: &str| {
            std::fs::read_to_string(format!(
                "{}/../../k8s/components/hollowbrook/skill/{name}.md",
                env!("CARGO_MANIFEST_DIR")
            ))
            .expect(name)
        };
        let booking = parse(&read("create_booking"))
            .expect("parses")
            .operation
            .expect("an operation");
        assert_eq!(booking.method, "POST");
        assert_eq!(
            booking.body,
            ["room_id", "guest_name", "arrival", "departure"]
        );

        let charge_source = read("charge_payment_account");
        let charge = parse(&charge_source).expect("parses");
        let op = charge.operation.expect("an operation");
        assert_eq!(op.integer, ["amount_pence"]);
        // The key is the platform's to derive and send, not the model's.
        assert!(!op.body.contains(&"idempotency_key".to_string()));
        assert_eq!(op.identity, ["booking_id", "amount_pence"]);
        assert_eq!(op.key, Some(IdempotencyKey::Field("idempotency_key".into())));

        let index_source = read("index");
        let index = parse(&index_source).expect("parses");
        assert_eq!(index.operation, None, "the manifest is not an operation");
    }
}

#[cfg(test)]
mod what_it_must_not_do_quietly {
    use super::*;

    /// Everything in here was found by review, and every one of them parsed
    /// successfully into something wrong before it was fixed. They share a
    /// failure mode: a file that looks like it declares a rule, and a parser
    /// that produces a different rule or none, with nothing said either way. For
    /// a governance declaration that is the worst outcome available -- worse than
    /// refusing, because an operation meant to be gated is not, and the file says
    /// it is.

    #[test]
    fn a_space_before_the_colon_is_still_the_approval_key() {
        // YAML accepts `approval :`, so somebody writes it. Matching the literal
        // `approval:` left this unrecognized, its children skipped, and the
        // operation ungated.
        let parsed = parse("---\napproval :\n  requires: charge\n  matches: POST /charges\n  binds: [amount_pence]\n---\n")
            .expect("parse");
        assert_eq!(parsed.approval.expect("a rule").requires, "charge");
    }

    #[test]
    fn a_rule_nested_under_another_key_is_refused_not_skipped() {
        // Not a shape the docs invite, but the silent-drop hole is the same:
        // there has to be no way for the word to appear and nothing to happen.
        let err = parse("---\nother:\n  approval: charge\n---\n").unwrap_err();
        assert!(matches!(err, FrontmatterError::Invalid(_)));
    }

    #[test]
    fn a_wholly_indented_block_is_refused_rather_than_ignored() {
        let err =
            parse("---\n  approval:\n    requires: charge\n    matches: POST /charges\n  binds: [amount_pence]\n---\n")
                .unwrap_err();
        assert!(matches!(err, FrontmatterError::Invalid(_)));
    }

    #[test]
    fn an_unclosed_quote_does_not_become_part_of_the_act() {
        // `"charge` parsed to the act `"charge`, which compares unequal to every
        // grant for `charge` -- so the rule existed and matched nothing.
        let err = parse("---\napproval:\n  requires: \"charge\n---\n").unwrap_err();
        assert!(matches!(err, FrontmatterError::Invalid(_)));
    }

    #[test]
    fn a_lone_quote_is_not_a_value() {
        // A single `"` stripped against itself and became the act `"`.
        let err = parse("---\napproval:\n  requires: \"\n---\n").unwrap_err();
        assert!(matches!(err, FrontmatterError::Invalid(_)));
    }

    #[test]
    fn mismatched_quotes_are_refused() {
        let err = parse("---\napproval:\n  requires: \"charge'\n---\n").unwrap_err();
        assert!(matches!(err, FrontmatterError::Invalid(_)));
    }

    #[test]
    fn a_key_given_twice_is_refused_rather_than_resolved() {
        // Taking the last means a reviewer who read the first approved something
        // else.
        let err = parse("---\napproval:\n  requires: a\n  requires: b\n---\n").unwrap_err();
        assert!(matches!(err, FrontmatterError::Invalid(_)));
    }

    #[test]
    fn a_file_that_never_mentions_approval_declares_nothing() {
        // The backstop must not turn every ordinary file into an error.
        let parsed = parse("---\ntitle: Listing rooms\n---\n# list_rooms\n").expect("parse");
        assert_eq!(parsed.approval, None);
    }

    #[test]
    fn a_stray_line_in_the_frontmatter_does_not_open_the_block() {
        let parsed = parse(
            "---\njust prose\napproval:\n  requires: charge\n  matches: POST /charges\n  binds: [amount_pence]\n---\n",
        )
        .expect("parse");
        assert_eq!(parsed.approval.expect("a rule").requires, "charge");
    }
}

/// A list of names: `[a, b]` or a bare `a`.
///
/// Hand-parsed like the rest of this module, and narrow on purpose. A flow
/// sequence of quoted or bare scalars is the whole contract; a block sequence,
/// a nested collection or an anchor is refused rather than half-read.
fn parse_list(key: &str, value: &str) -> Result<Vec<String>, FrontmatterError> {
    let inner = match value.strip_prefix('[') {
        Some(rest) => rest.strip_suffix(']').ok_or_else(|| {
            FrontmatterError::Invalid(format!("{key} opens a list and does not close it"))
        })?,
        // A bare scalar, which is the one-field case written the obvious way.
        None => value,
    };

    let mut fields = Vec::new();
    for raw in inner.split(',') {
        let field = unquote(raw.trim())?;
        if field.is_empty() {
            return Err(FrontmatterError::Invalid(format!(
                "{key} has an empty field name"
            )));
        }
        // In `binds` a duplicate would be hashed twice and mean nothing more
        // than once, but it is a declaration somebody got wrong and the digest
        // it produces is not the one they meant. Anywhere else it is the same
        // mistake.
        if fields.iter().any(|f| f == field) {
            return Err(FrontmatterError::Invalid(format!(
                "{key} names {field} twice"
            )));
        }
        fields.push(field.to_string());
    }
    Ok(fields)
}

#[cfg(test)]
mod operations {
    use super::*;

    const BOOKING: &str = "---\noperation:\n  method: post\n  url: http://outturn-hollowbrook:8084/bookings\n  body: [room_id, guest_name, arrival, departure, notes]\n  optional: [notes]\n---\n# create_booking\n";

    fn refused(source: &str) -> String {
        match parse(source) {
            Err(FrontmatterError::Invalid(why)) => why,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_operation_is_read() {
        let parsed = parse(BOOKING).expect("parse");
        let op = parsed.operation.expect("an operation");
        assert_eq!(op.method, "POST", "upper case, as a request's method is");
        assert_eq!(op.url, "http://outturn-hollowbrook:8084/bookings");
        assert_eq!(
            op.body,
            ["room_id", "guest_name", "arrival", "departure", "notes"]
        );
        assert_eq!(op.optional, ["notes"]);
        assert!(op.path_params().is_empty());
        assert_eq!(op.summary, "");
        assert_eq!(parsed.approval, None);
        assert_eq!(parsed.body, "# create_booking\n");
    }

    #[test]
    fn the_summary_is_the_first_paragraph_of_the_prose() {
        let parsed = parse(
            "---\noperation:\n  method: POST\n  url: http://h/bookings\n---\n\n# create_booking\n\nReserve a room\nfor a date range.\n\n## The call\n",
        )
        .expect("parse");
        assert_eq!(
            parsed.operation.expect("op").summary,
            "Reserve a room for a date range."
        );
    }

    #[test]
    fn path_parameters_come_from_the_url() {
        let parsed = parse(
            "---\noperation:\n  method: GET\n  url: http://h:8084/rooms/{room_id}/nights/{date}\n---\n",
        )
        .expect("parse");
        assert_eq!(
            parsed.operation.expect("op").path_params(),
            ["room_id", "date"]
        );
    }

    #[test]
    fn query_and_types_are_read() {
        let parsed = parse(
            "---\noperation:\n  method: GET\n  url: https://h/availability\n  query: [arrival, departure, guests]\n  integer: [guests]\n  optional: [guests]\n---\n",
        )
        .expect("parse");
        let op = parsed.operation.expect("op");
        assert_eq!(op.query, ["arrival", "departure", "guests"]);
        assert_eq!(op.integer, ["guests"]);
    }

    #[test]
    fn a_method_and_a_url_are_required() {
        refused("---\noperation:\n  url: http://h/x\n---\n");
        refused("---\noperation:\n  method: GET\n---\n");
        refused("---\noperation:\n  method: FETCH\n  url: http://h/x\n---\n");
    }

    #[test]
    fn the_url_must_be_absolute_http_without_a_query() {
        refused("---\noperation:\n  method: GET\n  url: /bookings\n---\n");
        refused("---\noperation:\n  method: GET\n  url: ftp://h/x\n---\n");
        refused("---\noperation:\n  method: GET\n  url: http://h/x?all=1\n---\n");
    }

    #[test]
    fn a_parameter_is_a_whole_path_segment() {
        refused("---\noperation:\n  method: GET\n  url: http://h/bookings/bk_{id}\n---\n");
        refused("---\noperation:\n  method: GET\n  url: http://h/bookings/{}\n---\n");
        refused("---\noperation:\n  method: GET\n  url: http://{host}/bookings\n---\n");
    }

    #[test]
    fn a_get_has_no_body() {
        let why = refused("---\noperation:\n  method: GET\n  url: http://h/x\n  body: [a]\n---\n");
        assert!(why.contains("query"), "{why}");
    }

    #[test]
    fn a_name_lives_in_one_place() {
        refused(
            "---\noperation:\n  method: POST\n  url: http://h/x\n  body: [a]\n  query: [a]\n---\n",
        );
        refused("---\noperation:\n  method: PUT\n  url: http://h/x/{a}\n  body: [a]\n---\n");
    }

    #[test]
    fn a_qualifier_must_name_a_field_that_is_sent() {
        // A typo in `optional` would leave the field it meant required, and the
        // model asked for something the author meant it could leave out.
        refused(
            "---\noperation:\n  method: POST\n  url: http://h/x\n  body: [a]\n  optional: [b]\n---\n",
        );
        refused(
            "---\noperation:\n  method: POST\n  url: http://h/x\n  body: [a]\n  integer: [b]\n---\n",
        );
        // A path parameter is always a required string.
        refused("---\noperation:\n  method: GET\n  url: http://h/x/{a}\n  optional: [a]\n---\n");
    }

    #[test]
    fn a_list_is_read_and_must_name_a_field_that_is_sent() {
        let parsed = parse(
            "---\noperation:\n  method: GET\n  url: http://h/pets\n  query: [tags, ids]\n  list: [tags, ids]\n  integer: [ids]\n---\n",
        )
        .expect("parse");
        let op = parsed.operation.expect("op");
        assert_eq!(op.list, ["tags", "ids"]);
        assert_eq!(
            op.integer,
            ["ids"],
            "a list of integers is a list and a type"
        );
        refused("---\noperation:\n  method: GET\n  url: http://h/pets\n  list: [tags]\n---\n");
    }

    #[test]
    fn a_schema_is_read_laid_out_and_names_the_body() {
        let parsed = parse(
            "---\noperation:\n  method: POST\n  url: http://h/pets/{owner}\n  query: [notify]\n  schema: |\n    {\n      \"type\": \"object\",\n      \"required\": [\"name\"],\n      \"properties\": {\n        \"owner\": {\"type\": \"string\"},\n        \"notify\": {\"type\": \"boolean\"},\n        \"name\": {\"type\": \"string\"},\n        \"category\": {\"$ref\": \"#/types/Category\"}\n      }\n    }\n---\n# add\n",
        )
        .expect("parse");
        let op = parsed.operation.expect("op");
        assert_eq!(
            op.body,
            ["category", "name"],
            "what the path and query do not take"
        );
        assert_eq!(
            op.arguments_schema()["required"],
            serde_json::json!(["name"])
        );
        assert_eq!(parsed.body, "# add\n");
    }

    #[test]
    fn a_schema_must_cover_the_path_and_leave_types_to_itself() {
        refused(
            "---\noperation:\n  method: GET\n  url: http://h/pets/{id}\n  schema: |\n    {\"type\": \"object\", \"properties\": {}}\n---\n",
        );
        refused(
            "---\noperation:\n  method: POST\n  url: http://h/pets\n  integer: [n]\n  schema: |\n    {\"type\": \"object\", \"properties\": {\"n\": {\"type\": \"integer\"}}}\n---\n",
        );
        refused(
            "---\noperation:\n  method: GET\n  url: http://h/pets\n  schema: |\n    {\"type\": \"object\", \"properties\": {\"q\": {\"type\": \"string\"}}}\n---\n",
        );
    }

    #[test]
    fn an_identity_and_where_its_key_goes_are_read() {
        let op = parse("---\noperation:\n  method: POST\n  url: http://h/bookings\n  body: [room_id, guest_name, notes]\n  identity: [room_id, guest_name]\n  key: field idempotency_key\n---\n")
            .expect("parse")
            .operation
            .expect("op");
        assert_eq!(op.identity, ["room_id", "guest_name"]);
        assert_eq!(
            op.key,
            Some(IdempotencyKey::Field("idempotency_key".into()))
        );
    }

    #[test]
    fn an_identity_is_fields_the_write_sends() {
        refused(
            "---\noperation:\n  method: POST\n  url: http://h/b\n  body: [a]\n  identity: [b]\n---\n",
        );
        refused(
            "---\noperation:\n  method: POST\n  url: http://h/b\n  body: [a]\n  key: header Idempotency-Key\n---\n",
        );
        refused(
            "---\noperation:\n  method: GET\n  url: http://h/b\n  query: [a]\n  identity: [a]\n---\n",
        );
        refused(
            "---\noperation:\n  method: POST\n  url: http://h/b\n  body: [a]\n  identity: [a]\n  key: cookie x\n---\n",
        );
    }

    #[test]
    fn a_field_has_one_type() {
        refused(
            "---\noperation:\n  method: POST\n  url: http://h/x\n  body: [a]\n  integer: [a]\n  boolean: [a]\n---\n",
        );
    }

    #[test]
    fn an_unknown_key_under_operation_is_refused() {
        refused("---\noperation:\n  method: POST\n  url: http://h/x\n  headers: [x]\n---\n");
    }

    #[test]
    fn operation_as_a_scalar_is_refused() {
        refused("---\noperation: create_booking\n---\n");
    }

    #[test]
    fn a_block_given_twice_is_refused() {
        refused(
            "---\noperation:\n  method: GET\n  url: http://h/a\noperation:\n  url: http://h/b\n---\n",
        );
    }

    const CHARGE: &str = "  url: http://h:8084/charges\n  body: [payment_account_id, booking_id, amount_pence, idempotency_key]\n  integer: [amount_pence]\n";

    #[test]
    fn a_gate_that_fits_its_operation_is_accepted() {
        let source = format!(
            "---\napproval:\n  requires: charge\n  matches: POST /charges\n  binds: [payment_account_id, booking_id, amount_pence]\n  covers: booking\n  identified_by: booking_id\noperation:\n  method: POST\n{CHARGE}---\n"
        );
        let parsed = parse(&source).expect("parse");
        assert!(parsed.approval.is_some() && parsed.operation.is_some());
    }

    #[test]
    fn a_gate_on_another_path_is_refused() {
        // The file gates /charges and the agent posts to /charge: nothing it
        // sends is gated, and the file says otherwise.
        let why = refused(
            "---\napproval:\n  requires: charge\n  matches: POST /charges\n  binds: [amount]\noperation:\n  method: POST\n  url: http://h/charge\n  body: [amount]\n---\n",
        );
        assert!(why.contains("/charge"), "{why}");
    }

    #[test]
    fn a_gate_on_another_method_is_refused() {
        refused(
            "---\napproval:\n  requires: charge\n  matches: PUT /charges\n  binds: [amount]\noperation:\n  method: POST\n  url: http://h/charges\n  body: [amount]\n---\n",
        );
    }

    #[test]
    fn a_star_covers_a_path_parameter_and_a_literal_does_not() {
        parse(
            "---\napproval:\n  requires: cancel\n  matches: DELETE /bookings/*\n  binds: [reason]\noperation:\n  method: DELETE\n  url: http://h/bookings/{id}\n  body: [reason]\n---\n",
        )
        .expect("a star covers the id");
        refused(
            "---\napproval:\n  requires: cancel\n  matches: DELETE /bookings/{id}\n  binds: [reason]\noperation:\n  method: DELETE\n  url: http://h/bookings/{id}\n  body: [reason]\n---\n",
        );
    }

    #[test]
    fn a_gate_binding_a_field_the_operation_never_sends_is_refused() {
        // A grant keyed on a field that is never there is keyed on nothing.
        let why = refused(
            "---\napproval:\n  requires: charge\n  matches: POST /charges\n  binds: [amount_pence]\noperation:\n  method: POST\n  url: http://h/charges\n  body: [amount]\n---\n",
        );
        assert!(why.contains("amount_pence"), "{why}");
    }
}
