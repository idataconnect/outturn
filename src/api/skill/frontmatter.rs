//! The declaration at the top of a skill's operation file.
//!
//! See `docs/approvals.md` for what the keys mean and `docs/skill-bundles.md`
//! for why a skill file is a safe place to put a governance rule -- versioned
//! with its body, immutable once published, written under `SkillsWrite`, and
//! read-only to the agent it governs.
//!
//! Frontmatter is for the *platform*, not the model. The prose below the fences
//! is what an agent is told; a declaration up here is what this tier acts on.
//! Anything a model needs to know belongs in the body where it can see it.
//!
//! Parsed by hand rather than with a YAML crate. The contract is three scalar
//! keys under one heading, and it is deliberately that narrow: a full YAML
//! reader would accept anchors, aliases, multi-document streams and nested
//! collections, none of which mean anything here, and every one of which would
//! be a shape somebody eventually writes and expects to work. What this accepts
//! is what `docs/approvals.md` documents, and it refuses the rest by not
//! understanding it.

use serde::{Deserialize, Serialize};

/// What an operation's file declares about needing approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRule {
    /// The act, in a word. Compared rather than read: two operations declaring
    /// the same act are the same act for the purposes of one grant.
    pub requires: String,
    /// The unit a wider grant may span, when the approver ticks it. Absent
    /// means an approval covers this call and its retries and nothing else,
    /// which is the default because it needs no judgement from the approver.
    pub covers: Option<String>,
    /// Which field of the request names that unit. Required when `covers` is
    /// present and meaningless without it.
    pub identified_by: Option<String>,
}

/// What a file's frontmatter said, and the body below it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed<'a> {
    pub approval: Option<ApprovalRule>,
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
            body: source,
        });
    };

    let Some((yaml, body)) = split_at_close_fence(rest) else {
        return Err(FrontmatterError::Unterminated);
    };

    Ok(Parsed {
        approval: approval_from(yaml)?,
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

/// Reads the `approval:` block, if there is one.
///
/// Deliberately not a general YAML reader: one heading at column zero, and
/// `key: value` lines indented under it. Anything else under the heading is
/// refused rather than ignored, because a rule half-read is a rule half-applied.
fn approval_from(yaml: &str) -> Result<Option<ApprovalRule>, FrontmatterError> {
    let mut inside = false;
    let mut requires = None;
    let mut covers = None;
    let mut identified_by = None;

    for raw in yaml.lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }

        let indented = line.starts_with(' ') || line.starts_with('\t');
        if !indented {
            // A new top-level key ends the block. Others are left alone: a file
            // may carry declarations this version knows nothing about.
            inside = line.trim_end() == "approval:";
            if inside {
                continue;
            }
            // A top-level `approval: something` is a rule written as a scalar,
            // which is not the shape and would otherwise be silently skipped.
            if let Some(value) = line.strip_prefix("approval:")
                && !value.trim().is_empty()
            {
                return Err(FrontmatterError::Invalid(
                    "approval takes a block of keys, not a value".into(),
                ));
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
        let value = unquote(value.trim());
        if value.is_empty() {
            return Err(FrontmatterError::Invalid(format!(
                "{} has no value",
                key.trim()
            )));
        }
        match key.trim() {
            "requires" => requires = Some(value.to_string()),
            "covers" => covers = Some(value.to_string()),
            "identified_by" => identified_by = Some(value.to_string()),
            other => {
                return Err(FrontmatterError::Invalid(format!(
                    "{other} is not a key of approval"
                )));
            }
        }
    }

    let Some(requires) = requires else {
        // The block was opened and said nothing that names the act.
        if covers.is_some() || identified_by.is_some() {
            return Err(FrontmatterError::Invalid(
                "approval needs a requires saying what is being asked".into(),
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

    Ok(Some(ApprovalRule {
        requires,
        covers,
        identified_by,
    }))
}

/// Strips one layer of matching quotes, so `requires: "charge"` reads the same
/// as `requires: charge`.
fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|v| v.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
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
            "---\napproval:\n  requires: charge\n  covers: booking\n  identified_by: booking_id\n---\n\n# charge\n",
        )
        .expect("parse");
        assert_eq!(
            parsed.approval,
            Some(ApprovalRule {
                requires: "charge".into(),
                covers: Some("booking".into()),
                identified_by: Some("booking_id".into()),
            })
        );
        assert_eq!(parsed.body, "\n# charge\n");
    }

    #[test]
    fn the_narrow_form_needs_only_requires() {
        let parsed = parse("---\napproval:\n  requires: charge\n---\nbody").expect("parse");
        let rule = parsed.approval.expect("a rule");
        assert_eq!(rule.requires, "charge");
        assert_eq!(rule.covers, None);
    }

    #[test]
    fn covers_without_identified_by_is_refused() {
        // A unit nobody can identify cannot be keyed on, so the grant would
        // quietly become one for every call.
        let err =
            parse("---\napproval:\n  requires: charge\n  covers: booking\n---\n").unwrap_err();
        assert!(matches!(err, FrontmatterError::Invalid(_)));
    }

    #[test]
    fn identified_by_without_covers_is_refused() {
        let err = parse("---\napproval:\n  requires: charge\n  identified_by: booking_id\n---\n")
            .unwrap_err();
        assert!(matches!(err, FrontmatterError::Invalid(_)));
    }

    #[test]
    fn a_block_with_no_requires_is_refused_rather_than_ignored() {
        let err = parse("---\napproval:\n  covers: booking\n---\n").unwrap_err();
        assert!(matches!(err, FrontmatterError::Invalid(_)));
    }

    #[test]
    fn an_empty_block_declares_nothing() {
        // Not an error: a file may open a block and say nothing in it, and
        // nothing is what it then declares.
        let parsed = parse("---\napproval:\n---\nbody").expect("parse");
        assert_eq!(parsed.approval, None);
    }

    #[test]
    fn an_unknown_key_under_approval_is_refused() {
        // Under a block we act on, a key we do not know may be the difference
        // between gated and not.
        let err = parse("---\napproval:\n  requires: charge\n  unless: friday\n---\n").unwrap_err();
        assert!(matches!(err, FrontmatterError::Invalid(_)));
    }

    #[test]
    fn an_unknown_top_level_key_is_left_alone() {
        // A file written for a later version of this platform still reads here.
        let parsed =
            parse("---\ntitle: Charging\napproval:\n  requires: charge\n---\nbody").expect("parse");
        assert_eq!(parsed.approval.expect("a rule").requires, "charge");
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
            "---\n# what this needs\napproval:\n  requires: \"charge\"\n  covers: 'booking'\n  identified_by: booking_id\n---\n",
        )
        .expect("parse");
        let rule = parsed.approval.expect("a rule");
        assert_eq!(rule.requires, "charge");
        assert_eq!(rule.covers.as_deref(), Some("booking"));
    }

    #[test]
    fn windows_line_endings_parse() {
        let parsed = parse("---\r\napproval:\r\n  requires: charge\r\n---\r\nbody").expect("parse");
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
        let source =
            std::fs::read_to_string("k8s/components/hollowbrook/skill/charge_payment_account.md")
                .expect("the charge operation's file");
        let parsed = parse(&source).expect("its frontmatter parses");
        let rule = parsed.approval.expect("it declares an approval");
        assert_eq!(rule.requires, "charge");
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

    /// And the operations that need no approval declare none.
    ///
    /// The point of the default being absence: a file documenting a read is an
    /// ordinary markdown file with no preamble.
    #[test]
    fn the_reads_declare_nothing() {
        for name in [
            "list_rooms",
            "get_room",
            "check_availability",
            "list_bookings",
            "get_booking",
            "list_payment_accounts",
        ] {
            let path = format!("k8s/components/hollowbrook/skill/{name}.md");
            let source = std::fs::read_to_string(&path).expect(&path);
            let parsed = parse(&source).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(parsed.approval, None, "{name} should need no approval");
            assert_eq!(parsed.body, source, "{name} has no frontmatter to strip");
        }
    }
}
