//! The name a skill file's operation has as a tool: `<slug>_<operation>`.
//!
//! One definition for the API, which names the tools a turn offers, and the
//! agent component, which matches a name a model asked for back to its file.
//! The component cannot depend on this crate -- it is built for the sandbox,
//! from its own manifest -- so it includes this file by path, which is why it
//! uses nothing but `core` and `alloc`. Two copies had to be kept in step by
//! hand, and a name either side composed differently would point a model at
//! a file the other could not find.

/// `hollowbrook` and `create_booking.md` are `hollowbrook_create_booking`.
/// Anything a tool name may not hold becomes `_`, and the whole is cut to the
/// 64 characters a provider accepts.
pub fn tool_name(slug: &str, path: &str) -> String {
    let stem = path.strip_suffix(".md").unwrap_or(path);
    let mut name = String::new();
    for c in slug
        .chars()
        .chain(core::iter::once('_'))
        .chain(stem.chars())
        .take(64)
    {
        name.push(if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            c
        } else {
            '_'
        });
    }
    name
}
