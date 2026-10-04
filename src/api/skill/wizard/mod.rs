mod parse;
mod preview;
mod render;

use std::collections::BTreeMap;

pub use parse::{ParseError, document};
pub use preview::{Kind, Preview, preview};

pub struct WizardInput {
    pub spec_json: Vec<u8>,
    pub slug: String,
    pub base_url: String,
    /// The header the egress rule will attach the credential in -- the one
    /// the page showed and the operator confirmed, so the skill and the rule
    /// say the same thing. `None` when the API takes no credential.
    pub auth_header: Option<String>,
    /// What people added to the generated skill, rendered into it on every
    /// generation so that an updated specification keeps them. See
    /// docs/openapi-wizard.md, "A derivation, not an output".
    pub annotations: Vec<Annotation>,
}

pub struct WizardOutput {
    pub body: String,
    pub files: BTreeMap<String, String>,
    pub hosts: Vec<String>,
    /// Indexes into `WizardInput::annotations` whose target is not in this
    /// specification: a category or operation renamed or removed. Reported,
    /// never dropped quietly -- a business rule that stopped applying because
    /// somebody renamed an operation is a fact leaving without anyone seeing.
    pub unmatched: Vec<usize>,
}

/// Something a person added to a generated skill, at the level where it has to
/// be seen: the whole skill (the body, which every turn carries), a category
/// (its file), or one operation (its file).
///
/// Keyed by what the specification names -- a tag, an operation's name -- and
/// never by a file path, since files are what generation produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    pub target: Target,
    pub kind: AnnotationKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Skill,
    /// A category by its tag, as the specification writes it: `Sale Invoices`.
    Category(String),
    /// An operation by the name the skill shows it under.
    Operation(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnnotationKind {
    /// Prose for the agent, at any level.
    Note(String),
    /// Use this other operation instead. Operation level.
    Prefer(String),
    /// Leave the operation out of the skill. Advice rather than enforcement:
    /// an agent can still compose the call; a gate is what stops one.
    Hidden,
    /// The operation's approval rule, as the YAML that goes between the file's
    /// `---` fences -- parsed by the same reader as a hand-written file, at
    /// publish. Operation level.
    Approval(String),
}

pub const MAX_SPEC_BYTES: usize = 16 * 1024 * 1024;
const MANIFEST_THRESHOLD: usize = 30;

pub fn generate(input: &WizardInput) -> Result<WizardOutput, ParseError> {
    if input.spec_json.len() > MAX_SPEC_BYTES {
        return Err(ParseError::TooLarge(input.spec_json.len()));
    }

    let mut api = parse::parse(&input.spec_json)?;
    let auth_header = input
        .auth_header
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty());
    // The platform sets this header, so no operation should offer it to the
    // model as a parameter. Header names are case-insensitive.
    if let Some(header) = auth_header {
        for op in &mut api.operations {
            op.parameters
                .retain(|p| !(p.location == "header" && p.name.eq_ignore_ascii_case(header)));
        }
    }
    let host = extract_host(&input.base_url);

    // Which annotations name something this specification has, before
    // anything is hidden: hiding an operation is applying an annotation to it.
    let names: std::collections::HashSet<&str> =
        api.operations.iter().map(|o| o.name.as_str()).collect();
    let tags: std::collections::HashSet<String> = api
        .operations
        .iter()
        .map(|o| {
            o.tags
                .first()
                .cloned()
                .unwrap_or_else(|| "general".to_string())
        })
        .collect();
    let unmatched: Vec<usize> = input
        .annotations
        .iter()
        .enumerate()
        .filter(|(_, a)| match (&a.target, &a.kind) {
            (Target::Operation(_), AnnotationKind::Prefer(other))
                if !names.contains(other.as_str()) =>
            {
                true
            }
            (Target::Operation(name), _) => !names.contains(name.as_str()),
            (Target::Category(tag), _) => !tags.contains(tag),
            (Target::Skill, _) => false,
        })
        .map(|(i, _)| i)
        .collect();
    let notes = render::Notes::from(&input.annotations, &unmatched);

    api.operations.retain(|op| !notes.hidden(&op.name));
    let categories = categorise(&api.operations);
    let needs_categories = api.operations.len() > MANIFEST_THRESHOLD;

    let mut files = BTreeMap::new();

    for op in &api.operations {
        let detail = render::detail(op, &input.base_url, auth_header, &notes);
        files.insert(format!("{}.md", op.name), detail);
    }

    let body = if needs_categories {
        for (cat_slug, cat) in &categories {
            let manifest =
                render::category_manifest_with_ops(cat, &api.operations, &input.slug, &notes);
            files.insert(format!("{cat_slug}.md"), manifest);
        }
        render::category_body(&input.slug, &categories, auth_header, &notes)
    } else {
        render::flat_body(&input.slug, &api.operations, auth_header, &notes)
    };

    Ok(WizardOutput {
        body,
        files,
        hosts: vec![host],
        unmatched,
    })
}

fn extract_host(base_url: &str) -> String {
    if let Some(rest) = base_url.strip_prefix("https://") {
        rest.split('/').next().unwrap_or(rest).to_string()
    } else if let Some(rest) = base_url.strip_prefix("http://") {
        rest.split('/').next().unwrap_or(rest).to_string()
    } else {
        base_url.split('/').next().unwrap_or(base_url).to_string()
    }
}

struct Category {
    label: String,
    slug: String,
    operations: Vec<usize>,
}

fn categorise(operations: &[parse::Operation]) -> BTreeMap<String, Category> {
    let mut cats: BTreeMap<String, Category> = BTreeMap::new();

    for (i, op) in operations.iter().enumerate() {
        let tag = op
            .tags
            .first()
            .cloned()
            .unwrap_or_else(|| "general".to_string());
        let slug = slugify(&tag);
        cats.entry(slug.clone())
            .or_insert_with(|| Category {
                label: tag,
                slug,
                operations: Vec::new(),
            })
            .operations
            .push(i);
    }

    cats
}

fn slugify(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

#[cfg(test)]
mod tests;
