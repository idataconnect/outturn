//! What a specification says about itself, read before anything is generated,
//! so the page can prefill the form rather than ask for what the document
//! already answers. Everything here is a suggestion the operator may edit.

use serde::Serialize;
use serde_json::Value;

use super::{MAX_SPEC_BYTES, ParseError, categorize, parse, slugify};

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
    /// What kind of scheme `auth_header` serves, when it came from a declared
    /// scheme rather than a guess from parameters.
    pub auth_kind: Option<Kind>,
    /// Operations that require a credential the chosen header does not carry
    /// -- OAuth, a key in a query, anything a static header cannot be -- and
    /// so will not authenticate on their own. Zero is the case to want.
    pub unserved_operations: usize,
    /// The kinds those operations ask for instead, without repeats.
    pub unserved_kinds: Vec<Kind>,
    /// A name for the gateway's environment variable, `OUTTURN_EGRESS_BIGCAPITAL_API_KEY`.
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
    let root = parse::document(spec_json)?;

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

    let auth = declared_auth(&root);
    let auth_header = auth
        .chosen
        .as_ref()
        .map(|(_, header, _)| header.clone())
        .or_else(|| api.common_auth.map(|a| a.header_name));

    Ok(Preview {
        credential_env: credential_env(&slug),
        base_url: base_url(&root, fetched_from),
        auth_header,
        auth_kind: auth.chosen.map(|(_, _, kind)| kind),
        unserved_operations: auth.unserved_operations,
        unserved_kinds: auth.unserved_kinds,
        categories: categorize(&api.operations).len(),
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

/// The scheme the egress rule should serve, from `components.securitySchemes`:
/// the one the most operations ask for, among those a rule can serve at all.
///
/// Per operation, because that is where many specifications say it -- the
/// petstore declares no root `security` and puts OAuth on seven operations
/// and an API key on two, and reading only the root then falling back to the
/// first scheme by key order chose by alphabet. An operation that declares
/// nothing inherits the root's. A scheme defined and applied nowhere counts
/// for nothing but is still a candidate, since some documents define their
/// scheme and never get round to applying it.
fn declared_auth(root: &Value) -> DeclaredAuth {
    let none = Default::default();
    let schemes = root
        .get("components")
        .and_then(|c| c.get("securitySchemes"))
        .and_then(Value::as_object)
        .unwrap_or(&none);
    let uses = scheme_uses(root);
    let chosen = schemes
        .iter()
        .filter_map(|(name, scheme)| {
            let kind = Kind::of(scheme);
            let header = kind.header(scheme)?;
            let n = uses.get(name.as_str()).copied().unwrap_or(0);
            Some((n, name.as_str(), header, kind))
        })
        // Most used; on a tie, the first by name, so the answer is stable.
        .max_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(a.1)))
        .map(|(_, name, header, kind)| (name.to_string(), header, kind));

    // An operation is served if it needs nothing, offers an alternative that
    // needs nothing, or offers one that needs the chosen scheme alone. An
    // alternative needing it alongside another is not: a rule carries one.
    let chosen_name = chosen.as_ref().map(|(name, _, _)| name.as_str());
    let mut unserved_operations = 0;
    let mut unserved_kinds = std::collections::BTreeSet::new();
    for alternatives in requirements(root) {
        let served = alternatives.is_empty()
            || alternatives
                .iter()
                .any(|alt| alt.is_empty() || (alt.len() == 1 && Some(alt[0]) == chosen_name));
        if served {
            continue;
        }
        unserved_operations += 1;
        for name in alternatives.iter().flatten() {
            if Some(*name) != chosen_name {
                unserved_kinds.insert(schemes.get(*name).map_or(Kind::Other, Kind::of));
            }
        }
    }
    DeclaredAuth {
        chosen,
        unserved_operations,
        unserved_kinds: unserved_kinds.into_iter().collect(),
    }
}

#[derive(Default)]
struct DeclaredAuth {
    /// The scheme's name, the header it travels in, and its kind.
    chosen: Option<(String, String, Kind)>,
    unserved_operations: usize,
    unserved_kinds: Vec<Kind>,
}

const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// The security requirements each operation carries: its own `security` if it
/// declares one, even empty, and the root's otherwise. Each is a list of
/// alternatives, and each alternative the schemes it needs together.
fn requirements(root: &Value) -> Vec<Vec<Vec<&str>>> {
    let inherited = read_security(root.get("security")).unwrap_or_default();
    root.get("paths")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|paths| paths.values())
        .flat_map(|item| METHODS.iter().filter_map(move |m| item.get(*m)))
        .map(|op| read_security(op.get("security")).unwrap_or_else(|| inherited.clone()))
        .collect()
}

fn read_security(v: Option<&Value>) -> Option<Vec<Vec<&str>>> {
    Some(
        v?.as_array()?
            .iter()
            .filter_map(Value::as_object)
            .map(|req| req.keys().map(String::as_str).collect())
            .collect(),
    )
}

/// How many operations name each scheme, counting an operation once however
/// many of its alternatives name it.
fn scheme_uses(root: &Value) -> std::collections::HashMap<&str, usize> {
    let mut uses = std::collections::HashMap::new();
    for alternatives in requirements(root) {
        let named: std::collections::BTreeSet<&str> = alternatives.into_iter().flatten().collect();
        for name in named {
            *uses.entry(name).or_insert(0) += 1;
        }
    }
    uses
}

/// What a scheme is, as far as an egress rule is concerned: a rule attaches
/// one static header per host, so an API key in a header, a static bearer
/// token and basic credentials as a precomputed value are servable, and
/// nothing else is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    ApiKeyHeader,
    Bearer,
    Basic,
    ApiKeyQuery,
    ApiKeyCookie,
    #[serde(rename = "oauth2")]
    OAuth2,
    OpenIdConnect,
    MutualTls,
    Other,
}

impl Kind {
    fn of(scheme: &Value) -> Kind {
        let field = |k: &str| scheme.get(k).and_then(Value::as_str).unwrap_or("");
        match field("type") {
            "apiKey" => match field("in") {
                "header" => Kind::ApiKeyHeader,
                "query" => Kind::ApiKeyQuery,
                "cookie" => Kind::ApiKeyCookie,
                _ => Kind::Other,
            },
            "http" => match field("scheme").to_ascii_lowercase().as_str() {
                "bearer" => Kind::Bearer,
                "basic" => Kind::Basic,
                _ => Kind::Other,
            },
            "oauth2" => Kind::OAuth2,
            "openIdConnect" => Kind::OpenIdConnect,
            "mutualTLS" => Kind::MutualTls,
            _ => Kind::Other,
        }
    }

    /// The header a rule would attach the credential in, if a rule can.
    fn header(self, scheme: &Value) -> Option<String> {
        match self {
            Kind::ApiKeyHeader => scheme.get("name").and_then(Value::as_str).map(Into::into),
            Kind::Bearer | Kind::Basic => Some("Authorization".into()),
            _ => None,
        }
    }
}

/// `bigcapital` -> `OUTTURN_EGRESS_BIGCAPITAL_API_KEY`, and `acme-api` the same
/// shape rather than `..._ACME_API_API_KEY`. Inside the namespace a rule may
/// name, or the rule the page suggests would be refused.
fn credential_env(slug: &str) -> String {
    let stem = slug.to_ascii_uppercase().replace('-', "_");
    let stem = stem.strip_suffix("_API").unwrap_or(&stem);
    let prefix = crate::runtime::egress::CREDENTIAL_PREFIX;
    if stem.is_empty() {
        format!("{prefix}API_KEY")
    } else {
        format!("{prefix}{stem}_API_KEY")
    }
}
