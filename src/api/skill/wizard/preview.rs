//! What a specification says about itself, read before anything is generated,
//! so the page can prefill the form rather than ask for what the document
//! already answers. Everything here is a suggestion the operator may edit.

use serde::Serialize;
use serde_json::Value;

use super::{MAX_SPEC_BYTES, ParseError, categorise, parse, slugify};

#[derive(Debug, Serialize)]
pub struct Preview {
    pub name: String,
    pub description: String,
    pub slug: String,
    /// `None` when the specification names no server, or names a relative one
    /// with nothing to resolve it against. Common, and asked for rather than
    /// guessed.
    pub base_url: Option<String>,
    /// The header the credential travels in, when the specification says.
    pub auth_header: Option<String>,
    /// A name for the gateway's environment variable, `BIGCAPITAL_API_KEY`.
    pub credential_env: String,
    pub operations: usize,
    pub categories: usize,
}

/// `fetched_from` is the URL the document came from, if it came from one: a
/// relative `servers[0].url` is relative to that, and to nothing otherwise.
pub fn preview(spec_json: &[u8], fetched_from: Option<&str>) -> Result<Preview, ParseError> {
    if spec_json.len() > MAX_SPEC_BYTES {
        return Err(ParseError::TooLarge(spec_json.len()));
    }
    let api = parse::parse(spec_json)?;
    // Parsed twice, once for the operations and once for the parts `parse`
    // has no use for. Cheap beside the rest, and it keeps `parse` about
    // operations.
    let root: Value = serde_json::from_slice(spec_json).map_err(ParseError::InvalidJson)?;

    let info = root.get("info");
    let text = |key: &str| {
        info.and_then(|i| i.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string()
    };
    let name = text("title");
    let description = text("description");
    let slug = slugify(&name).replace('_', "-");

    let auth_header =
        declared_auth_header(&root).or_else(|| api.common_auth.map(|a| a.header_name));

    Ok(Preview {
        credential_env: credential_env(&slug),
        base_url: base_url(&root, fetched_from),
        auth_header,
        categories: categorise(&api.operations).len(),
        operations: api.operations.len(),
        name,
        description,
        slug,
    })
}

fn base_url(root: &Value, fetched_from: Option<&str>) -> Option<String> {
    let server = root.get("servers")?.as_array()?.first()?;
    let mut raw = server.get("url")?.as_str()?.trim().to_string();
    // `https://{region}.example.com`: each variable has a default, and the
    // default is the only value the document promises exists.
    if let Some(vars) = server.get("variables").and_then(Value::as_object) {
        for (name, var) in vars {
            if let Some(default) = var.get("default").and_then(Value::as_str) {
                raw = raw.replace(&format!("{{{name}}}"), default);
            }
        }
    }
    if raw.contains('{') {
        return None;
    }
    let resolved = match url::Url::parse(&raw) {
        Ok(u) => u,
        Err(url::ParseError::RelativeUrlWithoutBase) => {
            url::Url::parse(fetched_from?).ok()?.join(&raw).ok()?
        }
        Err(_) => return None,
    };
    if !matches!(resolved.scheme(), "http" | "https") || resolved.host_str().is_none() {
        return None;
    }
    Some(resolved.as_str().trim_end_matches('/').to_string())
}

/// From `components.securitySchemes`: a bearer scheme travels in
/// `Authorization`, an API key in a header travels in the header it names. A
/// key in a query or a cookie has no header to name, and OAuth flows are a
/// bearer token by the time anything carries them.
fn declared_auth_header(root: &Value) -> Option<String> {
    let schemes = root
        .get("components")?
        .get("securitySchemes")?
        .as_object()?;
    // The schemes the document actually applies come first; one merely
    // defined may be there for a single endpoint.
    let applied: Vec<&str> = root
        .get("security")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .flat_map(|req| req.keys().map(String::as_str))
        .collect();
    let ordered = applied
        .iter()
        .filter_map(|k| schemes.get(*k))
        .chain(schemes.values());
    for scheme in ordered {
        let kind = scheme.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "http" => {
                let s = scheme.get("scheme").and_then(Value::as_str).unwrap_or("");
                if s.eq_ignore_ascii_case("bearer") {
                    return Some("Authorization".into());
                }
            }
            "oauth2" | "openIdConnect" => return Some("Authorization".into()),
            "apiKey" if scheme.get("in").and_then(Value::as_str) == Some("header") => {
                if let Some(n) = scheme.get("name").and_then(Value::as_str) {
                    return Some(n.to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// `bigcapital` -> `BIGCAPITAL_API_KEY`, and `acme-api` the same shape rather
/// than `ACME_API_API_KEY`.
fn credential_env(slug: &str) -> String {
    let stem = slug.to_ascii_uppercase().replace('-', "_");
    let stem = stem.strip_suffix("_API").unwrap_or(&stem);
    if stem.is_empty() {
        "API_KEY".into()
    } else {
        format!("{stem}_API_KEY")
    }
}
