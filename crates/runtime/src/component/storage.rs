//! Object storage, as a guest reaches it: paths resolved and scoped, reads
//! clamped, and every byte written counted against the turn and the
//! workspace before it is sent.

use std::sync::Arc;

use outturn_core::utf8::describe_binary;

use super::{AgentHost, MAX_LISTS_PER_TURN, ObjectInfo, Stored, outturn};

/// An object a guest is writing, as the host holds it.
pub struct WriterState {
    inner: Box<dyn outturn_core::storage::ObjectWriter>,
    /// As the guest named it, for what it is told back.
    path: String,
    resolved: String,
    storage: Arc<dyn outturn_core::storage::StorageBackend>,
    /// The part this writer may hold, charged to admission while it is open.
    _charge: Option<crate::admission::Charge>,
}

/// An object a guest is reading, as the host holds it.
pub struct ReaderState {
    inner: Box<dyn outturn_core::storage::ObjectReader>,
}

/// Writers one turn may hold open at once. Each is charged a part to
/// admission, and a few is what any sensible piece of work needs.
pub const MAX_OPEN_WRITERS: usize = 4;

/// The most a single read from a reader returns. A guest asking for more is
/// given this, so one call cannot fill its memory.
pub const MAX_READ_CHUNK: u32 = 4 * 1024 * 1024;

/// What a guest's `len` is served as: never more than `MAX_READ_CHUNK`.
/// A short read is what a guest already handles; a four-gigabyte buffer
/// allocated by the host, outside admission and the guest's own cap, is not.
pub fn clamp_read(len: u32) -> u32 {
    len.min(MAX_READ_CHUNK)
}

impl AgentHost {
    /// Turns a storage failure into what the guest is told, and decides who
    /// else hears about it.
    ///
    /// A platform fault -- no bucket, no connection, bad credentials -- is
    /// logged at warn, because an operator has to fix it and nothing on the
    /// transcript reaches one. Everything else is the workspace's outcome: the
    /// transcript already records it as a failed tool, and putting it in the
    /// process log would page the operator for somebody else's typo.
    pub(super) fn storage_failed(
        &self,
        what: &str,
        e: outturn_core::storage::StorageError,
    ) -> String {
        if e.is_platform_fault() {
            tracing::warn!(
                session_id = %self.session_id,
                workspace_id = %self.workspace_id,
                error = %e,
                "object storage failed during {what}"
            );
        } else {
            tracing::debug!(session_id = %self.session_id, error = %e, "{what} refused");
        }
        e.to_string()
    }

    /// Resolves a guest path, or refuses it.
    ///
    /// The check is here rather than in the guest for the same reason the
    /// credential is: a component is deployed by a workspace, and a boundary it
    /// enforces on itself is not a boundary.
    pub(super) fn object_at(
        &self,
        path: &str,
    ) -> Result<(Arc<dyn outturn_core::storage::StorageBackend>, String), String> {
        use outturn_core::storage::StorageError;
        let Some(storage) = self.storage.clone() else {
            return Err("no object storage is configured".to_string());
        };
        if let Some(tidied) = outturn_core::storage::scope::skill_path(path) {
            let tidied = tidied.map_err(|_| format!("path is not allowed: {path}"))?;
            let file = self.skill_file(&tidied)?;
            let key = outturn_core::storage::scope::skill_blob_key(file.workspace_id, &file.sha256);
            return Ok((storage, key));
        }
        // Here rather than in each caller: every way the guest reaches a file
        // resolves its path through this, including the ones that hand the
        // bytes to something else -- `describe_image` would otherwise let a
        // vision model read what the agent may not.
        self.may_read(path)?;
        let resolved =
            outturn_core::storage::scope::resolve(&self.space, path).map_err(|e| match e {
                // Said in full: this is the one a model will hit, and the message
                // tells it how to correct itself.
                StorageError::Refused(m) => m,
                _ => format!("path is not allowed: {path}"),
            })?;
        Ok((storage, resolved))
    }

    /// A bound skill's file, or an answer that says what is there instead.
    pub(super) fn skill_file(
        &self,
        path: &str,
    ) -> Result<&outturn_core::storage::scope::SkillObject, String> {
        if let Some(f) = self.skill_files.iter().find(|f| f.path == path) {
            return Ok(f);
        }
        use outturn_core::storage::scope;
        let slug = scope::skill_slug(path).unwrap_or("");
        let dir = scope::skill_object_path(slug, "");
        let siblings: Vec<&str> = self
            .skill_files
            .iter()
            .filter(|f| f.path.starts_with(&dir))
            .map(|f| f.path.as_str())
            .collect();
        Err(if siblings.is_empty() {
            format!("no skill with files is called {slug:?} here")
        } else {
            const SHOWN: usize = 10;
            let listed: Vec<&str> = siblings.iter().copied().take(SHOWN).collect();
            let tail = if siblings.len() > SHOWN {
                format!(" (and {} more)", siblings.len() - SHOWN)
            } else {
                String::new()
            };
            format!(
                "there is no {path}; {dir} holds {}{}",
                listed.join(", "),
                tail,
            )
        })
    }

    /// Whether this turn may read at `path`, by the scope it names.
    pub(super) fn may_read(&self, path: &str) -> Result<(), String> {
        let (scope, _) = outturn_core::storage::scope::split(path).map_err(|e| e.to_string())?;
        if self.read_scopes.contains(&scope) {
            Ok(())
        } else {
            Err(format!(
                "this agent may not read {0}/. Ask whoever runs the workspace to allow it \
                 access to {0}/.",
                scope.as_str()
            ))
        }
    }

    /// What every write does once the bytes are stored, however they got
    /// there: whatever was read out of an older object at the path is wrong
    /// now, and whoever is watching for writes is told -- after the bytes are
    /// there, so whatever acts on it finds them.
    pub(super) async fn stored(
        &mut self,
        storage: &dyn outturn_core::storage::StorageBackend,
        path: &str,
        resolved: &str,
        what: Stored,
    ) {
        outturn_core::extract::invalidate(storage, resolved).await;
        if let Some(sink) = &self.on_write {
            sink(path, resolved, what);
        }
    }

    /// Whether this turn may write at `path`, by the scope it names.
    pub(super) fn may_write(&self, path: &str) -> Result<(), String> {
        if outturn_core::storage::scope::skill_path(path).is_some() {
            return Err(
                "skill/ holds the files of this agent's skills and is read-only. Write under \
                 session/ or agent/ instead."
                    .to_string(),
            );
        }
        let (scope, _) = outturn_core::storage::scope::split(path).map_err(|e| e.to_string())?;
        if self.write_scopes.contains(&scope) {
            Ok(())
        } else {
            Err(format!(
                "this agent may not write to {0}/. Write under agent/ or session/ instead \
                depending on scoping requirements, or inform the user that you need additional \
                permission to write to {0}/.",
                scope.as_str()
            ))
        }
    }

    /// Counts bytes about to be written against the turn's limit, before
    /// they are sent: a write refused afterwards has already filled the store.
    pub(super) fn take_bytes(&mut self, bytes: u64) -> Result<(), String> {
        let after = self.bytes_written.saturating_add(bytes);
        if self.max_written_bytes > 0 && after > self.max_written_bytes {
            tracing::warn!(
                session_id = %self.session_id,
                limit = self.max_written_bytes,
                "guest reached its limit on bytes written"
            );
            return Err(format!(
                "this turn may write {} bytes to files, and this would take it past that",
                self.max_written_bytes
            ));
        }
        if let Some(left) = self.remaining_stored_bytes
            && after > left
        {
            tracing::warn!(
                session_id = %self.session_id,
                "guest reached its workspace's storage quota"
            );
            return Err(
                "this workspace's storage is full: files can still be read and \
                        deleted, but not written. Deleting files that are no longer \
                        needed makes room."
                    .to_string(),
            );
        }
        self.bytes_written = after;
        Ok(())
    }

    pub(super) async fn read_object(
        &mut self,
        path: String,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>, String> {
        let len = clamp_read(len);
        let (storage, resolved) = self.object_at(&path)?;

        // A document is handed over as the words in it, not as the bytes a
        // model can do nothing with. The guest is never told this happened --
        // it asked for `session/report.pdf` and gets prose -- for the same
        // reason settings are resolved above it: what it needs is the content,
        // and where the content came from is the host's business.
        //
        // Only where something is reading documents at all. Without it a PDF
        // is bytes like any other, and is said to be, rather than "not yet"
        // for a job nothing will ever run.
        if outturn_core::extract::tika_url().is_some()
            && outturn_core::extract::is_extractable(&resolved)
        {
            use outturn_core::storage::StorageError;
            let text = outturn_core::extract::text_key(&resolved);
            return match storage.read(&text, offset, len).await {
                // Extracted, and there was nothing in it. Said outright: an
                // empty read is indistinguishable from an empty document, and
                // a model told a report is blank will report that it is.
                Ok(bytes) if bytes.is_empty() && offset == 0 => Err(format!(
                    "no text could be read from {path}; it may be a scan or an image"
                )),
                Ok(bytes) => Ok(bytes),
                Err(StorageError::NotFound) => {
                    // "Never" and "not yet" are different answers, and only
                    // one of them should have a reader come back later.
                    let failed = outturn_core::extract::failed_key(&resolved);
                    match storage.read(&failed, 0, 4096).await {
                        Ok(why) => Err(format!(
                            "{path} could not be read: {}",
                            String::from_utf8_lossy(&why).trim()
                        )),
                        Err(_) => Err(format!("{path} is still being read; ask again shortly")),
                    }
                }
                Err(e) => Err(self.storage_failed("read", e)),
            };
        }

        let bytes = storage
            .read(&resolved, offset, len)
            .await
            .map_err(|e| self.storage_failed("read", e))?;

        // Bytes that are not text are named rather than handed over. A model
        // given the first kilobyte of a PNG spends the tokens working out that
        // it is a PNG; told so, it can say so, or reach for the tool that
        // deals with it.
        if offset == 0
            && let Some(what) = describe_binary(&bytes)
        {
            return Err(format!("{path} is not text: it is {what}"));
        }
        Ok(bytes)
    }

    pub(super) async fn read_bytes(
        &mut self,
        path: String,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>, String> {
        let len = clamp_read(len);
        // The object as stored: no extraction, no redirect. What read_object
        // does for a document is a convenience for reading; this is for the
        // callers that need the thing itself.
        let (storage, resolved) = self.object_at(&path)?;
        storage
            .read(&resolved, offset, len)
            .await
            .map_err(|e| self.storage_failed("read", e))
    }

    pub(super) async fn stat_object(&mut self, path: String) -> Result<ObjectInfo, String> {
        let (storage, resolved) = self.object_at(&path)?;

        // Size means "how much you get if you read this", because that is what
        // it is used for: deciding whether to page through something. A
        // document read as text is the text's length, not the document's --
        // reporting the bytes of a PDF whose words are a tenth of that would
        // have a model page through a file it could have read whole.
        let extracting = outturn_core::extract::tika_url().is_some()
            && outturn_core::extract::is_extractable(&resolved);
        let stat_at = if extracting {
            outturn_core::extract::text_key(&resolved)
        } else {
            resolved.clone()
        };

        let found = match storage.stat(&stat_at).await {
            Ok(f) => f,
            Err(outturn_core::storage::StorageError::NotFound) if extracting => {
                let failed = outturn_core::extract::failed_key(&resolved);
                return Err(match storage.read(&failed, 0, 4096).await {
                    Ok(why) => format!(
                        "{path} could not be read: {}",
                        String::from_utf8_lossy(&why).trim()
                    ),
                    Err(_) => format!("{path} is still being read; ask again shortly"),
                });
            }
            Err(e) => return Err(self.storage_failed("stat", e)),
        };
        Ok(ObjectInfo {
            // Handed back as the guest named it, not as it is stored.
            path,
            size: found.size,
        })
    }

    pub(super) async fn write_object(
        &mut self,
        path: String,
        data: Vec<u8>,
    ) -> Result<u64, String> {
        self.may_write(&path)?;
        let (storage, resolved) = self.object_at(&path)?;
        // The bytes are already here; held against admission while they are
        // sent on, as a writer's part is while it is open.
        let _charge = self.charge(data.len() as u64, "write a file")?;
        self.take_bytes(data.len() as u64)?;
        let written = storage
            .write(&resolved, &data)
            .await
            .map_err(|e| self.storage_failed("write", e))?;
        self.stored(storage.as_ref(), &path, &resolved, Stored::Wrote(written))
            .await;
        Ok(written)
    }

    pub(super) async fn open_writer(
        &mut self,
        path: String,
    ) -> Result<wasmtime::component::Resource<WriterState>, String> {
        // Checked now rather than at finish, so a guest is refused before it
        // spends a turn producing something it may not keep.
        self.may_write(&path)?;
        let (storage, resolved) = self.object_at(&path)?;
        if self.open_writers >= MAX_OPEN_WRITERS {
            return Err(format!(
                "{MAX_OPEN_WRITERS} files are already being written; finish one first"
            ));
        }
        let charge = self.charge(
            outturn_core::storage::s3::PART_BYTES as u64,
            "open a file for writing",
        )?;
        let inner = storage
            .open_writer(&resolved)
            .await
            .map_err(|e| self.storage_failed("write", e))?;
        let writer = self
            .table
            .push(WriterState {
                inner,
                path,
                resolved,
                storage,
                _charge: charge,
            })
            .map_err(|e| e.to_string())?;
        self.open_writers += 1;
        Ok(writer)
    }

    pub(super) async fn open_reader(
        &mut self,
        path: String,
    ) -> Result<wasmtime::component::Resource<ReaderState>, String> {
        let (storage, resolved) = self.object_at(&path)?;
        let inner = storage
            .open_reader(&resolved)
            .await
            .map_err(|e| self.storage_failed("read", e))?;
        self.table
            .push(ReaderState { inner })
            .map_err(|e| e.to_string())
    }

    pub(super) async fn render_pdf(
        &mut self,
        markdown: String,
        destination: String,
    ) -> Result<ObjectInfo, String> {
        self.may_write(&destination)?;
        let (storage, resolved) = self.object_at(&destination)?;
        // The document and the PDF it becomes are both held at once; the
        // PDF's share is its most, as the renderer's answer is not known yet.
        let _charge = self.charge(
            markdown.len() as u64 + crate::render::MAX_PDF_BYTES as u64,
            "render a PDF",
        )?;
        let pdf = crate::render::render(&self.http, self.renderer_url.as_deref(), markdown).await?;
        self.take_bytes(pdf.len() as u64)?;
        let size = storage
            .write(&resolved, &pdf)
            .await
            .map_err(|e| self.storage_failed("write", e))?;
        self.stored(
            storage.as_ref(),
            &destination,
            &resolved,
            Stored::Wrote(size),
        )
        .await;
        Ok(ObjectInfo {
            path: destination,
            size,
        })
    }

    pub(super) async fn delete_object(&mut self, path: String) -> Result<(), String> {
        // Emptying a scope is writing to it, so it asks the same permission.
        self.may_write(&path)?;
        let (storage, resolved) = self.object_at(&path)?;
        storage.delete(&resolved).await.map_err(|e| match e {
            // Named, because "not found" alone leaves a model guessing which
            // of the paths it just listed it got wrong.
            outturn_core::storage::StorageError::NotFound => {
                format!("there is no file at {path}")
            }
            other => self.storage_failed("delete", other),
        })?;
        // Or the words of a deleted document stay readable to anyone who
        // names it, which is the deletion not having happened.
        self.stored(storage.as_ref(), &path, &resolved, Stored::Deleted)
            .await;
        Ok(())
    }

    pub(super) async fn list_objects(&mut self, prefix: String) -> Result<Vec<ObjectInfo>, String> {
        use outturn_core::storage::scope::{self, Scope};
        Self::count(
            &mut self.lists_used,
            MAX_LISTS_PER_TURN,
            "listings of files",
        )?;
        let Some(storage) = self.storage.clone() else {
            return Err("no object storage is configured".to_string());
        };
        // Skill files are listed from the table the turn arrived with, not
        // from the bucket: the prefix they are stored under is by hash, and
        // shared with versions this turn was not bound to.
        let skill_entries = |under: &str| -> Vec<ObjectInfo> {
            self.skill_files
                .iter()
                .filter(|f| f.path.starts_with(under))
                .map(|f| ObjectInfo {
                    path: f.path.clone(),
                    size: f.bytes,
                })
                .collect()
        };
        if let Some(under) = scope::skill_path(&prefix) {
            let under = under.map_err(|e| e.to_string())?;
            return Ok(skill_entries(&under));
        }
        // An empty prefix means "everything I have": the three scopes, each
        // listed under its own name.
        let prefixes: Vec<String> = if prefix.trim().is_empty() {
            // Only the scopes it may read. A filename is often the sensitive
            // part -- "everything I have" must not be a way to enumerate a
            // scope the guest cannot open.
            Scope::ALL
                .iter()
                .filter(|s| self.read_scopes.contains(s))
                .map(|s| scope::root_for(&self.space, *s))
                .collect()
        } else {
            // Named outright rather than reached for with an empty prefix, and
            // gated the same: filtering only "everything I have" would have
            // left asking for the scope by name as the way round it.
            self.may_read(&prefix)?;
            vec![
                scope::resolve_prefix(&self.space, &prefix).map_err(|e| match e {
                    outturn_core::storage::StorageError::Refused(m) => m,
                    other => other.to_string(),
                })?,
            ]
        };

        let mut out = Vec::new();
        for resolved in prefixes {
            let found = storage
                .list(&resolved)
                .await
                .map_err(|e| self.storage_failed("list", e))?;

            // The text of any documents under here, listed once rather than
            // stat'd one by one, so a listing costs two calls however many
            // files it holds.
            let text: std::collections::HashMap<String, u64> = if outturn_core::extract::tika_url()
                .is_none()
            {
                Default::default()
            } else {
                storage
                .list(&outturn_core::extract::text_key(&resolved))
                .await
                .map(|f| {
                    f.into_iter()
                        .filter(|t| !t.is_dir)
                        .filter_map(|t| {
                            let key = t.path.strip_prefix("extracted/")?.to_string();
                            Some((key, t.size))
                        })
                        .collect()
                })
                .unwrap_or_else(|e| {
                    // Sizes fall back to the documents' own, which is wrong in
                    // the safe direction -- but not silently.
                    tracing::warn!(error = %e, "could not list extracted text; sizes are of the originals");
                    Default::default()
                })
            };

            out.extend(found.iter().filter(|f| !f.is_dir).filter_map(|f| {
                Some(ObjectInfo {
                    path: scope::strip_root(&self.space, &f.path)?,
                    // Same rule as stat: what a read would hand over.
                    size: text.get(&f.path).copied().unwrap_or(f.size),
                })
            }));
        }
        if prefix.trim().is_empty() {
            out.extend(skill_entries(scope::SKILL_ROOT));
        }
        Ok(out)
    }
}

impl outturn::agent::host::HostObjectWriter for AgentHost {
    async fn write(
        &mut self,
        writer: wasmtime::component::Resource<WriterState>,
        chunk: Vec<u8>,
    ) -> Result<(), String> {
        self.take_bytes(chunk.len() as u64)?;
        let state = self.table.get_mut(&writer).map_err(|e| e.to_string())?;
        match state.inner.write(&chunk).await {
            Ok(()) => Ok(()),
            Err(e) => Err(self.storage_failed("write", e)),
        }
    }

    async fn finish(
        &mut self,
        writer: wasmtime::component::Resource<WriterState>,
    ) -> Result<ObjectInfo, String> {
        let state = self.table.delete(writer).map_err(|e| e.to_string())?;
        self.open_writers = self.open_writers.saturating_sub(1);
        let WriterState {
            inner,
            path,
            resolved,
            storage,
            _charge,
        } = state;
        let size = inner
            .finish()
            .await
            .map_err(|e| self.storage_failed("write", e))?;
        self.stored(storage.as_ref(), &path, &resolved, Stored::Wrote(size))
            .await;
        Ok(ObjectInfo { path, size })
    }

    /// A writer the guest let go of unfinished. Dropping it abandons the
    /// upload, and whatever was at the path stays.
    async fn drop(
        &mut self,
        writer: wasmtime::component::Resource<WriterState>,
    ) -> wasmtime::Result<()> {
        self.table.delete(writer)?;
        self.open_writers = self.open_writers.saturating_sub(1);
        Ok(())
    }
}

impl outturn::agent::host::HostObjectReader for AgentHost {
    async fn read(
        &mut self,
        reader: wasmtime::component::Resource<ReaderState>,
        len: u32,
    ) -> Result<Vec<u8>, String> {
        let state = self.table.get_mut(&reader).map_err(|e| e.to_string())?;
        match state.inner.read(clamp_read(len) as usize).await {
            Ok(bytes) => Ok(bytes),
            Err(e) => Err(self.storage_failed("read", e)),
        }
    }

    async fn drop(
        &mut self,
        reader: wasmtime::component::Resource<ReaderState>,
    ) -> wasmtime::Result<()> {
        self.table.delete(reader)?;
        Ok(())
    }
}

#[cfg(test)]
mod read_clamp {
    use super::*;

    #[test]
    fn a_guest_cannot_ask_for_more_than_a_chunk() {
        assert_eq!(clamp_read(u32::MAX), MAX_READ_CHUNK);
        assert_eq!(clamp_read(MAX_READ_CHUNK + 1), MAX_READ_CHUNK);
    }

    #[test]
    fn a_smaller_read_is_left_alone() {
        assert_eq!(clamp_read(0), 0);
        assert_eq!(clamp_read(4096), 4096);
        assert_eq!(clamp_read(MAX_READ_CHUNK), MAX_READ_CHUNK);
    }
}
