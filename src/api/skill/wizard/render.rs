use std::collections::BTreeMap;
use std::fmt::Write;

use super::{Annotation, AnnotationKind, Category, Target, parse};

/// The annotations that matched, arranged for rendering.
#[derive(Default)]
pub struct Notes<'a> {
    skill: Vec<&'a str>,
    category: BTreeMap<&'a str, Vec<&'a str>>,
    operation: BTreeMap<&'a str, Vec<&'a str>>,
    prefer: BTreeMap<&'a str, &'a str>,
    hidden: std::collections::HashSet<&'a str>,
    approval: BTreeMap<&'a str, &'a str>,
}

impl<'a> Notes<'a> {
    pub fn from(annotations: &'a [Annotation], unmatched: &[usize]) -> Self {
        let mut notes = Notes::default();
        for (i, a) in annotations.iter().enumerate() {
            if unmatched.contains(&i) {
                continue;
            }
            match (&a.target, &a.kind) {
                (Target::Skill, AnnotationKind::Note(text)) => notes.skill.push(text),
                (Target::Category(tag), AnnotationKind::Note(text)) => {
                    notes.category.entry(tag).or_default().push(text)
                }
                (Target::Operation(op), AnnotationKind::Note(text)) => {
                    notes.operation.entry(op).or_default().push(text)
                }
                (Target::Operation(op), AnnotationKind::Prefer(other)) => {
                    notes.prefer.insert(op, other);
                }
                (Target::Operation(op), AnnotationKind::Hidden) => {
                    notes.hidden.insert(op);
                }
                (Target::Operation(op), AnnotationKind::Approval(yaml)) => {
                    notes.approval.insert(op, yaml);
                }
                // Prefer, hidden and approval are about one operation; anywhere
                // else they mean nothing, and the API refuses to store them.
                _ => {}
            }
        }
        notes
    }

    /// Whether to leave an operation out. Never one with an approval rule: the
    /// rule lives only in the operation's file, so hiding it would remove the
    /// gate while the host stays allowed, and hiding is advice, not a fence.
    pub fn hidden(&self, op: &str) -> bool {
        self.hidden.contains(op) && !self.approval.contains_key(op)
    }
}

/// Notes as a list under their own heading, so a reader can tell what a
/// person added from what the specification said.
fn write_notes(out: &mut String, notes: &[&str]) {
    if notes.is_empty() {
        return;
    }
    writeln!(out, "## Notes from your workspace").unwrap();
    writeln!(out).unwrap();
    for note in notes {
        writeln!(out, "- {}", note.trim()).unwrap();
    }
    writeln!(out).unwrap();
}

pub fn flat_body(
    slug: &str,
    operations: &[parse::Operation],
    auth_header: Option<&str>,
    notes: &Notes,
) -> String {
    let mut out = String::new();

    write_auth_preamble(&mut out, auth_header);
    // A category's notes have no file of their own to go in when there are no
    // categories, so they come up to the body, labelled.
    let mut first = notes.skill.clone();
    let labelled: Vec<String> = notes
        .category
        .iter()
        .flat_map(|(tag, ns)| ns.iter().map(move |n| format!("{tag}: {}", n.trim())))
        .collect();
    first.extend(labelled.iter().map(String::as_str));
    write_notes(&mut out, &first);

    writeln!(
        out,
        "Each operation has a file saying how to call it; read the file for an"
    )
    .unwrap();
    writeln!(
        out,
        "operation before using it, with `read_object`. Do not guess a call from its"
    )
    .unwrap();
    writeln!(
        out,
        "name here — this list says what exists, not how to ask for it."
    )
    .unwrap();
    writeln!(out).unwrap();

    for op in operations {
        write_manifest_line(&mut out, slug, op, notes);
    }

    out
}

pub fn category_body(
    slug: &str,
    categories: &BTreeMap<String, Category>,
    auth_header: Option<&str>,
    notes: &Notes,
) -> String {
    let mut out = String::new();

    write_auth_preamble(&mut out, auth_header);
    write_notes(&mut out, &notes.skill);

    writeln!(
        out,
        "Operations are grouped by category. Read the category file to see what"
    )
    .unwrap();
    writeln!(
        out,
        "operations are available, then read an operation's own file before calling"
    )
    .unwrap();
    writeln!(
        out,
        "it. Use `read_object` for both. Do not guess a call from its name."
    )
    .unwrap();
    writeln!(out).unwrap();

    for cat in categories.values() {
        writeln!(
            out,
            "- **{}** ({} operations) — `skill/{slug}/{}.md`",
            cat.label,
            cat.operations.len(),
            cat.slug,
        )
        .unwrap();
    }

    out
}

pub fn category_manifest_with_ops(
    cat: &Category,
    operations: &[parse::Operation],
    slug: &str,
    notes: &Notes,
) -> String {
    let mut out = String::new();

    writeln!(out, "# {}", cat.label).unwrap();
    writeln!(out).unwrap();
    write_notes(
        &mut out,
        notes
            .category
            .get(cat.label.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default(),
    );

    for &i in &cat.operations {
        let op = &operations[i];
        write_manifest_line(&mut out, slug, op, notes);
    }

    out
}

pub fn detail(
    op: &parse::Operation,
    base_url: &str,
    auth_header: Option<&str>,
    notes: &Notes,
) -> String {
    let mut out = String::new();

    // The approval rule first, as frontmatter: what the platform reads, before
    // anything the model does.
    if let Some(yaml) = notes.approval.get(op.name.as_str()) {
        writeln!(out, "---\n{}\n---\n", yaml.trim()).unwrap();
    }
    writeln!(out, "# {}", op.name).unwrap();
    writeln!(out).unwrap();
    if !op.summary.is_empty() {
        writeln!(out, "{}", op.summary).unwrap();
        writeln!(out).unwrap();
    }
    // Before the call, because a business rule is worth reading before the URL
    // rather than after the error table.
    if let Some(other) = notes.prefer.get(op.name.as_str()) {
        writeln!(
            out,
            "**Prefer `{other}`** over this operation; read `{other}`'s file instead."
        )
        .unwrap();
        writeln!(out).unwrap();
    }
    write_notes(
        &mut out,
        notes
            .operation
            .get(op.name.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default(),
    );

    // The call
    writeln!(out, "## The call").unwrap();
    writeln!(out).unwrap();

    let url = format!("{}{}", base_url.trim_end_matches('/'), op.path);
    let query_params: Vec<_> = op
        .parameters
        .iter()
        .filter(|p| p.location == "query" && p.required)
        .collect();

    if query_params.is_empty() {
        writeln!(out, "`fetch_url` with `{} {}`.", op.method, url).unwrap();
    } else {
        let qs: Vec<String> = query_params
            .iter()
            .map(|p| format!("{}=<{}>", p.name, p.schema.type_name))
            .collect();
        writeln!(
            out,
            "`fetch_url` with `{} {}?{}`.",
            op.method,
            url,
            qs.join("&")
        )
        .unwrap();
    }

    if let Some(header) = auth_header {
        writeln!(out).unwrap();
        writeln!(
            out,
            "The `{header}` header is attached by the platform; do not set it."
        )
        .unwrap();
    }

    // Parameters (non-auth, non-query-required already shown in URL)
    let path_params: Vec<_> = op
        .parameters
        .iter()
        .filter(|p| p.location == "path")
        .collect();
    let optional_query: Vec<_> = op
        .parameters
        .iter()
        .filter(|p| p.location == "query" && !p.required)
        .collect();
    let other_headers: Vec<_> = op
        .parameters
        .iter()
        .filter(|p| p.location == "header")
        .collect();

    let has_params = !path_params.is_empty()
        || !query_params.is_empty()
        || !optional_query.is_empty()
        || !other_headers.is_empty();

    if has_params {
        writeln!(out).unwrap();
        writeln!(out, "## Parameters").unwrap();
        writeln!(out).unwrap();

        if !path_params.is_empty() {
            for p in &path_params {
                write_parameter(&mut out, p, true);
            }
        }

        // Required query params (already in URL, but document them)
        for p in &query_params {
            write_parameter(&mut out, p, true);
        }

        if !optional_query.is_empty() {
            if !path_params.is_empty() || !query_params.is_empty() {
                writeln!(out).unwrap();
                writeln!(out, "Optional:").unwrap();
                writeln!(out).unwrap();
            }
            for p in &optional_query {
                // Only include optional params that have a description
                if !p.description.is_empty() {
                    write_parameter(&mut out, p, false);
                }
            }
        }

        for p in &other_headers {
            write_parameter(&mut out, p, p.required);
        }
    }

    // Request body
    if let Some(ref body) = op.request_body {
        writeln!(out).unwrap();
        writeln!(out, "## What to send").unwrap();
        writeln!(out).unwrap();
        writeln!(
            out,
            "JSON body{}.",
            if body.required { " (required)" } else { "" }
        )
        .unwrap();
        writeln!(out).unwrap();

        let mut required: Vec<_> = body.fields.iter().filter(|f| f.required).collect();
        let mut optional: Vec<_> = body.fields.iter().filter(|f| !f.required).collect();
        required.sort_by(|a, b| a.name.cmp(&b.name));
        optional.sort_by(|a, b| a.name.cmp(&b.name));

        for f in &required {
            write_body_field(&mut out, f);
        }

        if !optional.is_empty() {
            if !required.is_empty() {
                writeln!(out).unwrap();
                writeln!(out, "Optional:").unwrap();
                writeln!(out).unwrap();
            }
            for f in &optional {
                if !f.description.is_empty() {
                    write_body_field(&mut out, f);
                }
            }
        }
    }

    // Responses
    if !op.responses.is_empty() {
        writeln!(out).unwrap();
        writeln!(out, "## What comes back").unwrap();
        writeln!(out).unwrap();

        for (status, resp) in &op.responses {
            let label = if !resp.description.is_empty() {
                format!("`{status}` — {}", resp.description)
            } else {
                format!("`{status}`")
            };

            if resp.fields.is_empty() {
                writeln!(out, "{label}.").unwrap();
            } else {
                writeln!(out, "{label}, with:").unwrap();
                writeln!(out).unwrap();
                for f in &resp.fields {
                    write_body_field(&mut out, f);
                }
            }
            writeln!(out).unwrap();
        }
    }

    out
}

fn write_auth_preamble(out: &mut String, auth_header: Option<&str>) {
    if let Some(header) = auth_header {
        writeln!(
            out,
            "Authentication is handled by the platform — the `{header}` header is attached \
             to every request automatically. Do not set it yourself."
        )
        .unwrap();
        writeln!(out).unwrap();
    }
}

fn write_manifest_line(out: &mut String, slug: &str, op: &parse::Operation, notes: &Notes) {
    let summary = if op.summary.is_empty() {
        format!("{} {}", op.method, op.path)
    } else {
        let s = op.summary.trim_end_matches('.');
        let mut chars = s.chars();
        match chars.next() {
            Some(c) => {
                let first = c.to_lowercase().to_string();
                format!("{first}{}", chars.as_str())
            }
            None => s.to_string(),
        }
    };
    // Where the choice between operations is made, so a preference is seen
    // before the wrong file is read.
    let prefer = notes
        .prefer
        .get(op.name.as_str())
        .map(|other| format!(" Prefer `{other}`."))
        .unwrap_or_default();
    writeln!(
        out,
        "- `{}` — {}.{prefer}\n  Detail: `skill/{slug}/{}.md`",
        op.name, summary, op.name,
    )
    .unwrap();
}

fn write_parameter(out: &mut String, p: &parse::Parameter, required: bool) {
    let mut parts = Vec::new();
    parts.push(format!("`{}`", p.name));

    let mut meta = Vec::new();
    meta.push(p.schema.type_name.clone());
    if let Some(ref fmt) = p.schema.format {
        meta.push(format!("format: {fmt}"));
    }
    if required {
        meta.push("required".to_string());
    }
    if let Some(ref vals) = p.schema.enum_values {
        meta.push(format!(
            "one of: {}",
            vals.iter()
                .map(|v| format!("`{v}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let desc = if p.description.is_empty() {
        meta.join(", ")
    } else {
        format!("{} — {}", meta.join(", "), p.description)
    };

    writeln!(out, "- {}: {desc}", parts.join(" ")).unwrap();
}

fn write_body_field(out: &mut String, f: &parse::BodyField) {
    let mut meta = Vec::new();
    match &f.schema.items_type {
        Some(items) if f.schema.type_name == "array" => meta.push(format!("array of {items}")),
        _ => meta.push(f.schema.type_name.clone()),
    }
    if let Some(ref fmt) = f.schema.format {
        meta.push(format!("format: {fmt}"));
    }
    if let Some(ref vals) = f.schema.enum_values {
        meta.push(format!(
            "one of: {}",
            vals.iter()
                .map(|v| format!("`{v}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(min) = f.schema.minimum {
        meta.push(format!("min: {min}"));
    }
    if let Some(max) = f.schema.maximum {
        meta.push(format!("max: {max}"));
    }

    let type_str = meta.join(", ");
    if f.description.is_empty() {
        writeln!(out, "- `{}` — {type_str}", f.name).unwrap();
    } else {
        writeln!(out, "- `{}` — {type_str}. {}", f.name, f.description).unwrap();
    }
}
