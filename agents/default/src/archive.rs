//! Archives, expanded and made inside the sandbox.
//!
//! Both directions run in the guest rather than on the host, and that is the
//! point. Inflating happens inside a WASM sandbox with a memory limit, so a
//! zip bomb blows up this component and not the platform; and every entry is
//! written through the same `write-object` the model uses, so an entry named
//! `../../etc/passwd` is refused by the same path check that refuses the model
//! typing it. The host gained nothing it can reach and nothing it must trust.
//!
//! The zip handling takes readers and writers rather than paths, so it can be
//! tested against memory; the host glue at the bottom is the only part that
//! knows where bytes come from.

use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom, Write};

use crate::bindings::outturn::agent::host;

/// More entries than this is not an archive somebody meant to expand.
pub const MAX_ENTRIES: usize = 10_000;
/// One entry, inflated, sits in guest memory before it is written. The
/// sandbox would stop a larger one anyway; this makes the refusal a sentence.
pub const MAX_ENTRY_BYTES: u64 = 32 * 1024 * 1024;
/// Written to storage across one expansion. Entries are written one at a
/// time, so memory does not bound this -- storage does.
pub const MAX_EXPANDED_BYTES: u64 = 1024 * 1024 * 1024;
/// An archive is assembled whole before it is written, since `write-object`
/// takes an object and not a stream. Bounded by what fits beside the inputs.
pub const MAX_ARCHIVE_INPUT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct Expanded {
    pub written: Vec<String>,
    pub bytes: u64,
    /// Entries left out, each with why. Reported rather than failed on: an
    /// archive with one bad name still has everything else in it.
    pub skipped: Vec<String>,
}

/// An entry name that may be written under a destination.
///
/// The resolver on the host refuses traversal regardless; this is so a bad
/// name is reported and skipped rather than ending the expansion at the
/// host's refusal of it.
pub fn safe_name(raw: &str) -> Option<String> {
    let name = raw.replace('\\', "/");
    let parts: Vec<&str> = name
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    if parts.is_empty() || parts.iter().any(|c| *c == "..") {
        return None;
    }
    Some(parts.join("/"))
}

/// Expands an archive, handing each entry to `write` as `(relative name, bytes)`.
pub fn expand_from<R, W>(reader: R, mut write: W) -> Result<Expanded, String>
where
    R: Read + Seek,
    W: FnMut(&str, &[u8]) -> Result<(), String>,
{
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| format!("not a zip archive: {e}"))?;
    if zip.len() > MAX_ENTRIES {
        return Err(format!("archive holds {} entries; at most {MAX_ENTRIES} are expanded", zip.len()));
    }

    let mut out = Expanded::default();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| format!("entry {i}: {e}"))?;
        if entry.is_dir() {
            continue;
        }
        let Some(name) = safe_name(entry.name()) else {
            out.skipped.push(format!("{}: name would escape the destination", entry.name()));
            continue;
        };
        if entry.size() > MAX_ENTRY_BYTES {
            out.skipped.push(format!("{name}: {} bytes, more than an entry may be", entry.size()));
            continue;
        }

        // Read through a cap rather than trusting the declared size: a bomb
        // lies about how big it is, and the lie is the attack.
        let mut data = Vec::with_capacity(entry.size() as usize);
        entry
            .take(MAX_ENTRY_BYTES + 1)
            .read_to_end(&mut data)
            .map_err(|e| format!("{name}: {e}"))?;
        if data.len() as u64 > MAX_ENTRY_BYTES {
            out.skipped.push(format!("{name}: inflated past what an entry may be"));
            continue;
        }

        out.bytes += data.len() as u64;
        if out.bytes > MAX_EXPANDED_BYTES {
            return Err(format!(
                "expansion passed {MAX_EXPANDED_BYTES} bytes after {} entries; stopped",
                out.written.len()
            ));
        }
        // A refusal with nothing yet landed is the whole expansion being
        // refused -- no permission to write here -- and is said so at once.
        // One after others have landed is that one entry, skipped and named.
        if let Err(e) = write(&name, &data) {
            if out.written.is_empty() {
                return Err(e);
            }
            out.skipped.push(format!("{name}: {e}"));
            continue;
        }
        out.written.push(name);
    }
    Ok(out)
}

/// Builds an archive from `(relative name, bytes)` pairs.
pub fn create_into<I>(entries: I) -> Result<Vec<u8>, String>
where
    I: IntoIterator<Item = (String, Vec<u8>)>,
{
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut total = 0u64;
    let mut count = 0usize;
    for (name, data) in entries {
        count += 1;
        if count > MAX_ENTRIES {
            return Err(format!("more than {MAX_ENTRIES} files; not archived"));
        }
        total += data.len() as u64;
        if total > MAX_ARCHIVE_INPUT_BYTES {
            return Err(format!(
                "inputs pass {MAX_ARCHIVE_INPUT_BYTES} bytes; an archive is assembled in memory and this is more than fits"
            ));
        }
        writer.start_file(name.as_str(), options).map_err(|e| format!("{name}: {e}"))?;
        writer.write_all(&data).map_err(|e| format!("{name}: {e}"))?;
    }
    if count == 0 {
        return Err("nothing to archive".to_string());
    }
    Ok(writer
        .finish()
        .map_err(|e| format!("finishing archive: {e}"))?
        .into_inner())
}

// Host glue -----------------------------------------------------------------

/// A stored object presented as something that can be read and seeked.
///
/// The zip format wants seeking: the central directory is at the end, and each
/// entry is reached by offset. Fetching the archive whole would put all of it
/// in guest memory, so this fetches the window it is asked for, and the
/// `BufReader` around it turns the many small reads a directory parse makes
/// into a few large ones.
struct Remote {
    path: String,
    size: u64,
    pos: u64,
}

impl Read for Remote {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.size || buf.is_empty() {
            return Ok(0);
        }
        let want = buf.len().min((self.size - self.pos) as usize).min(u32::MAX as usize);
        let got = host::read_bytes(&self.path, self.pos, want as u32).map_err(io::Error::other)?;
        let n = got.len().min(buf.len());
        buf[..n].copy_from_slice(&got[..n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Remote {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let next = match to {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::End(n) => self.size as i128 + n as i128,
            SeekFrom::Current(n) => self.pos as i128 + n as i128,
        };
        if next < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before start"));
        }
        self.pos = next as u64;
        Ok(self.pos)
    }
}

/// The object as stored, whole. Windowed rather than in one call so the size
/// is not needed up front and a document is not mistaken for its text.
fn read_all_bytes(path: &str) -> Result<Vec<u8>, String> {
    const WINDOW: u32 = 4 * 1024 * 1024;
    let mut out = Vec::new();
    let mut offset = 0u64;
    loop {
        let chunk = host::read_bytes(path, offset, WINDOW)?;
        let n = chunk.len();
        out.extend(chunk);
        offset += n as u64;
        if out.len() as u64 > MAX_ENTRY_BYTES {
            return Err(format!("{path} is larger than an archive entry may be"));
        }
        if n < WINDOW as usize {
            return Ok(out);
        }
    }
}

/// Expands a stored archive into a folder, through the host.
pub fn expand(archive: &str, destination: &str) -> Result<Expanded, String> {
    let info = host::stat_object(archive)?;
    let remote = Remote { path: archive.to_string(), size: info.size, pos: 0 };
    let dest = destination.trim_end_matches('/');
    expand_from(BufReader::with_capacity(64 * 1024, remote), |name, data| {
        let target = format!("{dest}/{name}");
        host::write_object(&target, data).map(|_| ()).map_err(|e| format!("{target}: {e}"))
    })
}

/// What a requested prefix turns out to name: the folder that entry names are
/// taken relative to, and the paths going in.
///
/// Split out from `create` because it is the part that was getting the answer
/// wrong, and the part that can be checked without a host.
pub fn resolve_inputs(asked: &str, listed: &[String], under: &[String]) -> (String, Vec<String>) {
    // A listing of the prefix that holds the prefix itself means the prefix is
    // one file: archive just that, named as it is named inside its folder. An
    // exact match and not a prefix match, or "session/main" would claim
    // "session/main.pdf" and archive it under a name nobody asked for.
    match listed.iter().find(|p| *p == asked) {
        Some(file) => (
            asked.rsplit_once('/').map_or(String::new(), |(dir, _)| format!("{dir}/")),
            vec![file.clone()],
        ),
        None => (format!("{asked}/"), under.to_vec()),
    }
}

/// Archives everything under a prefix into one stored object, through the host.
///
/// A single file is named as readily as a folder, by a model and by a person,
/// so one is archived under its own name rather than refused for not being a
/// folder.
pub fn create(prefix: &str, archive: &str) -> Result<(usize, u64), String> {
    let asked = prefix.trim_end_matches('/');
    let listed: Vec<String> =
        host::list_objects(asked)?.into_iter().map(|f| f.path).collect();
    // Only asked for when the prefix was not itself a file, so the common case
    // of a folder costs the one listing it always did.
    let under: Vec<String> = if listed.iter().any(|p| p == asked) {
        Vec::new()
    } else {
        host::list_objects(&format!("{asked}/"))?.into_iter().map(|f| f.path).collect()
    };
    let (prefix, found) = resolve_inputs(asked, &listed, &under);

    let mut entries = Vec::with_capacity(found.len());
    for path in &found {
        // Not the archive itself, should it be written under its own inputs.
        if path == archive {
            continue;
        }
        let name = path.strip_prefix(&prefix).unwrap_or(path).to_string();
        entries.push((name, read_all_bytes(path)?));
    }
    if entries.is_empty() {
        return Err(format!("nothing is stored under {asked}; nothing to archive"));
    }
    let count = entries.len();
    let bytes = create_into(entries)?;
    let written = host::write_object(archive, &bytes)?;
    Ok((count, written))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn names_that_escape_are_refused_and_the_rest_are_tidied() {
        assert_eq!(safe_name("a/b.txt"), Some("a/b.txt".into()));
        assert_eq!(safe_name("/a/b.txt"), Some("a/b.txt".into()), "leading slash dropped");
        assert_eq!(safe_name("a\\b.txt"), Some("a/b.txt".into()), "backslashes normalised");
        assert_eq!(safe_name("./a/./b.txt"), Some("a/b.txt".into()));
        assert_eq!(safe_name("../evil"), None);
        assert_eq!(safe_name("a/../../evil"), None);
        assert_eq!(safe_name(""), None);
        assert_eq!(safe_name("/"), None);
    }

    /// What goes in comes out, by name and by byte.
    #[test]
    fn an_archive_round_trips() {
        let bytes = create_into(vec![
            ("one.txt".to_string(), b"first".to_vec()),
            ("dir/two.bin".to_string(), vec![0u8, 255, 7, 7, 7]),
        ])
        .expect("create");

        let mut got: HashMap<String, Vec<u8>> = HashMap::new();
        let out = expand_from(Cursor::new(bytes), |name, data| {
            got.insert(name.to_string(), data.to_vec());
            Ok(())
        })
        .expect("expand");

        assert_eq!(out.written.len(), 2);
        assert!(out.skipped.is_empty(), "{:?}", out.skipped);
        assert_eq!(got["one.txt"], b"first");
        assert_eq!(got["dir/two.bin"], vec![0u8, 255, 7, 7, 7]);
    }

    /// An entry that would climb out is left behind, and everything else lands.
    #[test]
    fn a_traversing_entry_is_skipped_not_fatal() {
        // Built by hand, since create_into would not produce such a name.
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default();
        w.start_file("../../etc/passwd", o).unwrap();
        w.write_all(b"root:x").unwrap();
        w.start_file("fine.txt", o).unwrap();
        w.write_all(b"ok").unwrap();
        let bytes = w.finish().unwrap().into_inner();

        let mut got = Vec::new();
        let out = expand_from(Cursor::new(bytes), |name, _| {
            got.push(name.to_string());
            Ok(())
        })
        .expect("expand");

        assert_eq!(got, vec!["fine.txt"]);
        assert_eq!(out.skipped.len(), 1, "{:?}", out.skipped);
        assert!(out.skipped[0].contains("escape"), "{:?}", out.skipped);
    }

    /// A prefix naming one file archives that file, under its bare name --
    /// the case that reported "nothing to archive" and did nothing.
    #[test]
    fn a_prefix_naming_one_file_archives_that_file() {
        let listed = vec!["session/main.pdf".to_string()];
        let (prefix, found) = resolve_inputs("session/main.pdf", &listed, &[]);
        assert_eq!(prefix, "session/");
        assert_eq!(found, vec!["session/main.pdf"]);
        assert_eq!(found[0].strip_prefix(&prefix), Some("main.pdf"));
    }

    /// A folder still archives everything under it, relative to the folder.
    #[test]
    fn a_prefix_naming_a_folder_archives_what_is_under_it() {
        let under = vec!["session/docs/a.txt".to_string(), "session/docs/b/c.txt".to_string()];
        let (prefix, found) = resolve_inputs("session/docs", &[], &under);
        assert_eq!(prefix, "session/docs/");
        assert_eq!(found, under);
        assert_eq!(found[1].strip_prefix(&prefix), Some("b/c.txt"));
    }

    /// A partial name is not a file: it is a folder prefix, matched exactly or
    /// not at all, or "session/main" would archive "session/main.pdf" as a
    /// file the caller never named.
    #[test]
    fn a_partial_name_is_not_taken_for_the_file_it_prefixes() {
        let listed = vec!["session/main.pdf".to_string()];
        let (prefix, found) = resolve_inputs("session/main", &listed, &[]);
        assert_eq!(prefix, "session/main/");
        assert!(found.is_empty());
    }

    /// The declared size is not trusted: an entry is read through a cap.
    #[test]
    fn an_entry_that_inflates_past_the_cap_is_skipped() {
        // Compresses to almost nothing and inflates past the limit.
        let big = vec![0u8; (MAX_ENTRY_BYTES + 1024) as usize];
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        w.start_file("bomb", o).unwrap();
        w.write_all(&big).unwrap();
        let bytes = w.finish().unwrap().into_inner();

        let out = expand_from(Cursor::new(bytes), |_, _| Ok(())).expect("expand");
        assert!(out.written.is_empty());
        assert_eq!(out.skipped.len(), 1);
    }
}
