mod parse;
mod render;

use std::collections::BTreeMap;

pub use parse::ParseError;

pub struct WizardInput {
    pub spec_json: Vec<u8>,
    pub slug: String,
    pub base_url: String,
}

pub struct WizardOutput {
    pub body: String,
    pub files: BTreeMap<String, String>,
    pub hosts: Vec<String>,
}

const MAX_SPEC_BYTES: usize = 16 * 1024 * 1024;
const MANIFEST_THRESHOLD: usize = 30;

pub fn generate(input: &WizardInput) -> Result<WizardOutput, ParseError> {
    if input.spec_json.len() > MAX_SPEC_BYTES {
        return Err(ParseError::TooLarge(input.spec_json.len()));
    }

    let api = parse::parse(&input.spec_json)?;
    let host = extract_host(&input.base_url);

    let categories = categorise(&api.operations);
    let needs_categories = api.operations.len() > MANIFEST_THRESHOLD;

    let mut files = BTreeMap::new();

    for op in &api.operations {
        let detail = render::detail(op, &input.base_url, &api.common_auth);
        files.insert(format!("{}.md", op.name), detail);
    }

    let body = if needs_categories {
        for (cat_slug, cat) in &categories {
            let manifest = render::category_manifest_with_ops(cat, &api.operations, &input.slug);
            files.insert(format!("{cat_slug}.md"), manifest);
        }
        render::category_body(&input.slug, &categories, &api.common_auth)
    } else {
        render::flat_body(&input.slug, &api.operations, &api.common_auth)
    };

    Ok(WizardOutput {
        body,
        files,
        hosts: vec![host],
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
