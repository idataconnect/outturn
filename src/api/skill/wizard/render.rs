use std::collections::BTreeMap;
use std::fmt::Write;

use super::{Category, parse};

pub fn flat_body(slug: &str, operations: &[parse::Operation], auth_header: Option<&str>) -> String {
    let mut out = String::new();

    write_auth_preamble(&mut out, auth_header);

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
        write_manifest_line(&mut out, slug, op);
    }

    out
}

pub fn category_body(
    slug: &str,
    categories: &BTreeMap<String, Category>,
    auth_header: Option<&str>,
) -> String {
    let mut out = String::new();

    write_auth_preamble(&mut out, auth_header);

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
) -> String {
    let mut out = String::new();

    writeln!(out, "# {}", cat.label).unwrap();
    writeln!(out).unwrap();

    for &i in &cat.operations {
        let op = &operations[i];
        write_manifest_line(&mut out, slug, op);
    }

    out
}

pub fn detail(op: &parse::Operation, base_url: &str, auth_header: Option<&str>) -> String {
    let mut out = String::new();

    writeln!(out, "# {}", op.name).unwrap();
    writeln!(out).unwrap();
    if !op.summary.is_empty() {
        writeln!(out, "{}", op.summary).unwrap();
        writeln!(out).unwrap();
    }

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

fn write_manifest_line(out: &mut String, slug: &str, op: &parse::Operation) {
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
    writeln!(
        out,
        "- `{}` — {}.\n  Detail: `skill/{slug}/{}.md`",
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
