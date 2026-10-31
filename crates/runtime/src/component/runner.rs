//! Running one turn: building the host, instantiating the guest, and
//! reading back what it produced.

use std::sync::Arc;

use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::WasiCtxBuilder;

use super::cache::CompiledCache;
use super::*;

/// What a turn produced, and why it stopped.
///
/// A struct rather than a tuple: the last two are both about a turn ending
/// early and mean opposite things for the session, which is exactly the pair a
/// positional tuple invites somebody to swap.
#[derive(Debug)]
pub struct Finished {
    pub reply: String,
    pub cost: TurnCost,
    /// Why a hold ended it, where one did.
    pub held: Option<String>,
    /// Whether what stopped it is a question somebody will answer, and so
    /// whether the session should latch. See `ExecuteEvent::Done`.
    pub awaiting_approval: bool,
    /// What this turn's replies were signed with, for the turns after it.
    pub signed: Vec<Signed>,
}

pub struct AgentRunner {
    engine: Engine,
    /// Built once. A linker describes what the host offers, which does not
    /// vary by turn, by guest, or by workspace.
    linker: Linker<AgentHost>,
    compiled: CompiledCache,
}

pub struct RunOptions {
    pub session_id: uuid::Uuid,
    pub gateway_url: String,
    pub gateway_token: String,
    pub gateway_token_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Asks the API for a fresh turn token. A turn has no upper bound while it
    /// works, and its token has to outlast it.
    pub refresh_token: Option<TokenRefresher>,
    pub gates: outturn_core::egress::gate::RawGates,
    pub default_model: String,
    pub progress: Option<ProgressSink>,
    pub reasoning: Option<ReasoningSink>,
    pub writing: Option<WritingSink>,
    pub on_tool: Option<ToolSink>,
    pub on_tool_result: Option<ToolResultSink>,
    pub on_usage: Option<UsageSink>,
    pub on_write: Option<WriteSink>,
    pub on_absorbed: Option<AbsorbedSink>,
    /// Called when a request goes out with a person's own key.
    pub on_connection: Option<ConnectionSink>,
    /// Called when the gateway refuses a request for want of an approval.
    pub on_gated: Option<GatedSink>,
    /// Where declared writes are recorded, so one sent twice is one write.
    pub invocations: Option<Arc<dyn InvocationRecord>>,
    /// Relays `sleep` and `set-timer` to the API.
    pub on_wait: Option<WaitSink>,
    /// Approved requests to send before the guest runs. See `send_held`.
    pub held: Option<HeldSends>,
    pub fuel: u64,
    /// IANA zone of the user this turn belongs to, as the client reported it.
    /// Unrecognized or absent means the clock answers in UTC.
    pub timezone: Option<String>,
    /// Passed to providers that support it; ignored by those that do not.
    pub reasoning_effort: Option<String>,
    /// What earlier replies were signed with, from the API.
    pub signed: Vec<Signed>,
    /// Applied when the guest names no temperature of its own.
    pub temperature: Option<f32>,
    /// Names the class of traffic, which the gateway resolves to a route.
    pub traffic_type: String,
    /// Model calls permitted in this turn; zero is unbounded.
    pub max_tool_rounds: u32,
    /// Bytes this turn may write to files; zero for no limit.
    pub max_written_bytes: u64,
    /// What the workspace may still store; `None` for no quota.
    pub remaining_stored_bytes: Option<u64>,
    /// Tools to offer from the first round, by name; empty defers every tool
    /// to an explicit request from the guest. Names the component does not
    /// have are ignored, so a stale policy is harmless.
    pub eager_tools: Vec<String>,
    /// The bound skills' operations, offered to the model as tools.
    pub skill_operations: Vec<outturn_core::protocol::work::SkillOperation>,
    /// The reply being written, so messages absorbed mid-turn can name it.
    pub reply_id: uuid::Uuid,
    /// Object storage the guest may reach, within its workspace's own space.
    pub storage: Option<Arc<dyn outturn_core::storage::StorageBackend>>,
    /// Hosts this turn may reach, from the workspace's own rules.
    pub egress: Vec<outturn_core::egress::rule::EgressRule>,
    /// Whose space that is. The guest is never told.
    pub workspace_id: uuid::Uuid,
    pub agent_id: uuid::Uuid,
    /// Scopes the guest may write: "session", "agent", "workspace".
    pub write_scopes: Vec<String>,
    /// Scopes the guest may read. A superset of `write_scopes`.
    pub read_scopes: Vec<String>,
    /// The bound skills' files, readable as `skill/<slug>/<path>`.
    pub skill_files: Vec<outturn_core::storage::scope::SkillObject>,
    /// How long the gateway's stream may go silent before the turn is
    /// abandoned. A parameter so a test can prove it fires.
    pub idle_timeout: std::time::Duration,
    /// Charged for what the turn holds beyond its flat charge. None charges
    /// nothing.
    pub admission: Option<Arc<crate::admission::Admission>>,
    /// Where the PDF renderer listens. None where none is deployed, and
    /// `render-pdf` says so.
    pub renderer_url: Option<String>,
}

impl AgentRunner {
    pub fn new() -> anyhow::Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        // Fuel bounds a runaway guest; without it a loop in a component would
        // occupy a worker indefinitely.
        config.consume_fuel(true);
        let engine = Engine::new(&config)?;

        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        // The generated linker needs a HasData marker naming which type the
        // host implementations live on.
        AgentWorld::add_to_linker::<_, HostData>(&mut linker, |state: &mut AgentHost| state)?;

        Ok(Self {
            engine,
            linker,
            compiled: CompiledCache::new(),
        })
    }

    /// The compiled form of these bytes, compiling only if it is not held.
    fn component_for(&self, bytes: &[u8]) -> anyhow::Result<Component> {
        use sha2::{Digest, Sha256};

        let key: [u8; 32] = Sha256::digest(bytes).into();
        self.compiled.get_or_compile(key, || {
            let started = std::time::Instant::now();
            let component = Component::new(&self.engine, bytes)?;
            tracing::debug!(
                bytes = bytes.len(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "compiled a guest component"
            );
            Ok(component)
        })
    }

    /// Checks that this host can link this component, before taking any work.
    ///
    /// Every import the guest declares has to have an implementation in the
    /// linker, and the two are generated from `wit/agent.wit` at different
    /// times: the host when this binary was compiled, the guest when
    /// `assets/agent_default.wasm` was built. Ship a pair that disagree and
    /// every turn dies with "component imports instance `outturn:agent/host`,
    /// but a matching implementation was not found in the linker" -- one turn
    /// at a time, in a worker log, with nothing to say which half is wrong or
    /// that anything is wrong with the deployment at all.
    ///
    /// Asked once at startup instead, where it is a pod that fails to become
    /// ready rather than a user whose message failed. `artifact_guard` covers
    /// the same ground in the repository, comparing the committed interface
    /// against the one the component was built from; this covers what it
    /// cannot see, which is the binary doing the linking.
    ///
    /// Compiling is most of the cost and the result is cached, so the first
    /// turn finds the component ready rather than paying for it again.
    pub fn verify(&self, bytes: &[u8]) -> anyhow::Result<()> {
        let component = self.component_for(bytes)?;
        // Type-checks every import against the linker without a store or any
        // host state: this asks whether the two halves fit, not whether a turn
        // would succeed.
        self.linker.instantiate_pre(&component)?;
        Ok(())
    }

    pub async fn run(
        &self,
        component_bytes: &[u8],
        conversation: Vec<Message>,
        system_prompt: String,
        options: RunOptions,
    ) -> Result<Finished, HeldError> {
        let component = self.component_for(component_bytes)?;

        // No preopened directories, no environment, no network: everything the
        // guest can reach is an explicit import.
        // Told apart from what this turn signs, which is all it reports.
        let handed_in: std::collections::HashSet<String> = options
            .signed
            .iter()
            .map(|s| s.first_call.clone())
            .collect();
        let host = AgentHost {
            cancelled: false,
            held: None,
            awaiting_approval: false,
            wasi: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
            gateway_url: options.gateway_url,
            gateway_token: options.gateway_token,
            gateway_token_expires_at: options.gateway_token_expires_at,
            refresh_token: options.refresh_token,
            gates: options.gates,
            default_model: options.default_model,
            http: outturn_core::http_client::streaming_client(options.idle_timeout),
            progress: options.progress,
            reasoning: options.reasoning,
            writing: options.writing,
            on_tool: options.on_tool,
            on_tool_result: options.on_tool_result,
            on_gated: options.on_gated,
            invocations: options.invocations,
            on_wait: options.on_wait,
            on_usage: options.on_usage,
            on_write: options.on_write,
            on_absorbed: options.on_absorbed,
            on_connection: options.on_connection,
            connections_reported: Default::default(),
            session_id: options.session_id,
            // Parsed here so a bad zone from a client degrades to UTC once,
            // rather than on every call the guest makes.
            reasoning_effort: options.reasoning_effort,
            signed: options
                .signed
                .into_iter()
                .map(|s| (s.first_call.clone(), s))
                .collect(),
            temperature: options.temperature,
            traffic_type: options.traffic_type,
            max_tool_rounds: options.max_tool_rounds,
            max_written_bytes: options.max_written_bytes,
            remaining_stored_bytes: options.remaining_stored_bytes,
            fetches_used: 0,
            lists_used: 0,
            bytes_written: 0,
            eager_tools: options.eager_tools,
            skill_operations: options.skill_operations,
            rounds_used: 0,
            arrivals: Vec::new(),
            reply_id: options.reply_id,
            spent: Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
            },
            served_by: None,
            served_model: None,
            streamed: false,
            storage: options.storage,
            workspace_id: options.workspace_id,
            space: outturn_core::storage::scope::Space {
                workspace_id: options.workspace_id,
                agent_id: options.agent_id,
                session_id: options.session_id,
            },
            write_scopes: options
                .write_scopes
                .iter()
                .filter_map(|s| outturn_core::storage::scope::Scope::parse(s))
                .collect(),
            // Unioned with the writes rather than trusted as given. The
            // cascade already resolves reads as a superset, and this is the
            // boundary where being wrong about that would mean a write the
            // guest cannot read back.
            read_scopes: options
                .read_scopes
                .iter()
                .chain(options.write_scopes.iter())
                .filter_map(|s| outturn_core::storage::scope::Scope::parse(s))
                .collect(),
            skill_files: options.skill_files,
            egress: options.egress,
            limits: wasmtime::StoreLimitsBuilder::new()
                .memory_size(GUEST_MEMORY_LIMIT)
                // One instance and a handful of tables is what a component
                // needs; more is a guest doing something it need not.
                .instances(8)
                .tables(64)
                .build(),
            admission: options.admission,
            renderer_url: options.renderer_url,
            open_writers: 0,
            malformed_reply: false,
            refused: None,
            timezone: options.timezone.as_deref().and_then(|tz| {
                tz.parse::<chrono_tz::Tz>()
                    .inspect_err(|_| tracing::warn!(timezone = tz, "unknown timezone, using UTC"))
                    .ok()
            }),
        };

        let mut store = Store::new(&self.engine, host);

        // Before the guest runs, so what it reads is what happened rather than
        // an instruction to make it happen. Through the same path a guest's
        // fetch takes -- keyed, claimed and settled the same way -- under the
        // token minted for these requests alone. The answer replaces the
        // refusal it was the call for, where the API found one.
        let mut conversation = conversation;
        if let Some(held) = options.held {
            for call in &held.calls {
                // Marked as going first, so a pod lost before it reports
                // leaves the request sent rather than still to send: the
                // turn's retry must not send it again. Not marked, not sent --
                // it stays waiting, and a later turn tries.
                if let Some(begin) = &held.begin
                    && !begin(call.id).await
                {
                    tracing::warn!(
                        session_id = %store.data().session_id,
                        held_id = %call.id,
                        "a request somebody approved could not be marked as going, so it was not sent"
                    );
                    continue;
                }
                let outcome = store
                    .data_mut()
                    .send_held(call, held.token.clone(), held.gates.clone())
                    .await;
                let told = match &held.report {
                    Some(report) => report(call.id, outcome.clone()).await,
                    None => None,
                };
                // Unreported, the API will hand this request out again, so the
                // model is told no more than this host knows -- and a declared
                // write sent again is answered from the idempotency record.
                let told = told.unwrap_or_else(|| {
                    outturn_core::egress::held::result_text(
                        &call.method,
                        &call.url,
                        &call.approved_by,
                        &outcome,
                    )
                });
                tracing::info!(
                    session_id = %store.data().session_id,
                    held_id = %call.id,
                    answered = matches!(outcome, outturn_core::egress::held::Outcome::Answered { .. }),
                    "sent a request somebody approved"
                );
                let mut replaced = false;
                if let Some(id) = &call.tool_call_id {
                    for message in conversation
                        .iter_mut()
                        .filter(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some(id))
                    {
                        message.parts = vec![ContentPart::Text(told.clone())];
                        replaced = true;
                    }
                }
                // Sent, and no refusal found to put the answer in -- trimmed
                // away, or not recognized. Said anyway, as a note: a model
                // left with only the stale refusal makes the call itself, and
                // is asked about it a second time. Every later turn is told
                // the same by the API (`worker::answered`).
                if !replaced {
                    let note = Message {
                        role: "user".into(),
                        parts: vec![ContentPart::Text(
                            outturn_core::egress::held::unmatched_note(
                                &call.method,
                                &call.url,
                                &told,
                            ),
                        )],
                        tool_call_id: None,
                    };
                    // Before the prompt where the prompt is last; after
                    // everything otherwise, since a resumed turn can end on a
                    // call and its result, which must not be split.
                    match conversation.last() {
                        Some(last) if last.role == "user" => {
                            let at = conversation.len() - 1;
                            conversation.insert(at, note);
                        }
                        _ => conversation.push(note),
                    }
                }
            }
        }

        store.set_fuel(options.fuel)?;
        // Yield to the executor every so often while fuel is burning. Fuel
        // alone bounds how long a guest may run, not how long it may hold the
        // thread it runs on: without this a busy loop pins a Tokio worker
        // until the whole budget is spent, and every other turn on that
        // thread waits for it.
        store.fuel_async_yield_interval(Some(FUEL_YIELD_INTERVAL))?;
        // The guest's memory is capped here, in the sandbox, not only guessed
        // at by admission. Admission decides whether to start a turn from the
        // memory that is free; nothing about that stops a component from
        // growing once it is running, and a guest that grows without bound
        // takes the pod -- and every other turn on it -- with it.
        store.limiter(|host| &mut host.limits);

        let instance = AgentWorld::instantiate_async(&mut store, &component, &self.linker).await?;

        // Taken apart rather than chained, because a turn that a hold cut and
        // that then failed still has to say what cut it. A `?` here would drop
        // the store, and with it the only record that this was a stop rather
        // than a crash -- so the turn would be retried and, once the hold was
        // released, simply run.
        let outcome = instance
            .outturn_agent_agent()
            .call_run(&mut store, &conversation, &system_prompt)
            .await;

        // Logged however the turn ended, a trap included: what a turn burns
        // is wanted before the ledger records it, to see what turns cost.
        // Excludes host work done for the guest, a render included: the
        // renderer is a service, and logs its own time.
        tracing::info!(
            session_id = %store.data().session_id,
            fuel = options.fuel.saturating_sub(store.get_fuel().unwrap_or(0)),
            ok = matches!(outcome, Ok(Ok(_))),
            "turn finished"
        );

        let reply = match outcome {
            Ok(Ok(reply)) => reply,
            Ok(Err(e)) => {
                return Err(HeldError {
                    malformed_reply: store.data().malformed_reply,
                    refused: store.data().refused.clone(),
                    ..HeldError::new(
                        store.data().held.clone(),
                        // As the guest said it: the guest is passing on
                        // why it stopped, and a prefix saying which side of
                        // the sandbox it came from reads as another failure.
                        anyhow::anyhow!("{e}"),
                    )
                });
            }
            Err(e) => return Err(HeldError::new(store.data().held.clone(), e.into())),
        };

        // Read back from the host rather than returned by the guest: the
        // guest never sees these numbers, which is the point.
        let host = store.data();
        let cost = TurnCost {
            prompt_tokens: host.spent.prompt_tokens,
            completion_tokens: host.spent.completion_tokens,
            cache_read_tokens: host.spent.cache_read_tokens,
            cache_write_tokens: host.spent.cache_write_tokens,
            reasoning_tokens: host.spent.reasoning_tokens,
            provider: host.served_by.clone(),
            model: host.served_model.clone(),
        };

        // Beside the cost rather than on it: `TurnCost` is what a turn spent,
        // and why it stopped is not a number. The tier above needs both and
        // treats them differently -- one goes to the ledger, the other to the
        // latch.
        Ok(Finished {
            reply,
            cost,
            held: host.held.clone(),
            awaiting_approval: host.awaiting_approval,
            // Only this turn's: what the API handed in is already stored.
            signed: host
                .signed
                .values()
                .filter(|s| !handed_in.contains(&s.first_call))
                .cloned()
                .collect(),
        })
    }
}

#[cfg(test)]
mod artifact_guard {
    /// The committed component must have been built against the current
    /// interface.
    ///
    /// `assets/agent_default.wasm` and `agents/default/src/bindings.rs` are both
    /// generated and both committed, and neither regenerates on its own. Change
    /// `wit/agent.wit` without rebuilding and the guest crate still compiles --
    /// against its stale bindings -- while every turn fails at runtime with
    /// "component imports instance `outturn:agent/host`, but a matching
    /// implementation was not found in the linker". That is a long way to travel
    /// for a mismatch a file comparison can catch.
    ///
    /// The interface is copied beside the component when it is built, so this
    /// compares the two and a failure can be read as a diff.
    #[test]
    fn the_committed_component_was_built_against_this_interface() {
        const BUILT_AGAINST: &str = include_str!("../../../../assets/agent_default.wit");
        const CURRENT: &str = include_str!("../../../../wit/agent.wit");

        assert_eq!(
            CURRENT, BUILT_AGAINST,
            "wit/agent.wit has changed since assets/agent_default.wasm was built.\n\
             Rebuild the guest and copy both artifacts:\n\
             \x20 (cd agents/default && cargo component build --release)\n\
             \x20 cp agents/default/target/wasm32-wasip1/release/outturn_agent_default.wasm \
             assets/agent_default.wasm\n\
             \x20 cp wit/agent.wit assets/agent_default.wit\n\
             `cargo component build` regenerates src/bindings.rs on the way, so it \
             covers both."
        );
    }

    /// This host must be able to link the component committed beside it.
    ///
    /// The test above compares two files in the repository. This asks the
    /// question they cannot: whether the host compiled from *this* source
    /// satisfies every import the committed guest declares. They are generated
    /// from one interface at different times, and a pair that disagree fails
    /// every turn in the linker -- which is a long way from here, and looks
    /// like a model problem rather than a build one.
    ///
    /// The runtime asks the same thing at startup, against the component it
    /// was actually given, and refuses to become ready if the answer is no.
    #[test]
    fn this_host_can_link_the_committed_component() {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/agent_default.wasm"
        ))
        .expect("component fixture");
        let runner = super::AgentRunner::new().expect("runner");
        runner
            .verify(&bytes)
            .expect("the committed component must link against this host");
    }

    /// And it must refuse a component it cannot satisfy.
    ///
    /// A check that only ever passes is not a check. Bytes that are not a
    /// component at all stand in for the mismatch: what matters is that
    /// `verify` reports a problem rather than returning cheerfully, since the
    /// startup path treats a pass as permission to take work.
    #[test]
    fn a_component_it_cannot_link_is_refused() {
        let runner = super::AgentRunner::new().expect("runner");
        assert!(
            runner.verify(b"not a component at all").is_err(),
            "verify must reject what it cannot link"
        );
    }
}
