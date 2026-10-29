//! Compiled components, kept by the bytes they came from.

use std::sync::{Arc, Mutex};

use wasmtime::component::Component;

/// Compiled components to keep, keyed by the bytes they came from.
///
/// A cap because the map is keyed by workspace-supplied content: without one,
/// uploading a component is a way to make the runtime allocate, over and over,
/// with nothing to reclaim it. Small, because in practice a node serves a
/// handful of distinct agents and a miss costs one compile, not a failure.
const COMPILED_CACHE_ENTRIES: usize = 32;

/// How long an unused compiled component is kept.
///
/// Long enough that an idle workspace does not pay a compile on every message,
/// short enough that a redeployed agent's previous build is gone within the
/// hour rather than at the next pod recycle.
const COMPILED_CACHE_IDLE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Compiling a component is pure: the same bytes always produce the same
/// module, so the work only has to happen once per distinct guest rather than
/// once per turn.
///
/// Keyed by a hash of the bytes rather than by agent id, because a compiled
/// component is only valid for the bytes it came from -- an agent that is
/// redeployed with changes must not be served its previous build, and one
/// redeployed unchanged should be.
///
/// Most-recently-used last. Thirty-two entries is short enough that scanning
/// costs less than the bookkeeping to avoid it.
pub(super) struct CompiledCache<T = Component> {
    entries: Mutex<Vec<CompiledEntry<T>>>,
    /// One lock per key being compiled, so a miss that arrives while the same
    /// bytes are already compiling waits for that build instead of starting
    /// its own. Without it every concurrent miss compiled in parallel: forty
    /// of them at once took forty-seven seconds apiece in a debug build, where
    /// one took a few, and a node handed a burst of turns for a newly deployed
    /// agent would do the same.
    compiling: Mutex<std::collections::HashMap<[u8; 32], Arc<Mutex<()>>>>,
}

struct CompiledEntry<T> {
    key: [u8; 32],
    component: T,
    last_used: std::time::Instant,
}

impl<T: Clone> CompiledCache<T> {
    pub(super) fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            compiling: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// What is held for `key`, or what `compile` makes of it, compiled once
    /// however many ask at the same time.
    ///
    /// A failed build is not cached and not shared: the next caller in line
    /// tries again, which for a component that does not compile is a fast
    /// failure rather than a slow one.
    pub(super) fn get_or_compile(
        &self,
        key: [u8; 32],
        compile: impl FnOnce() -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        if let Some(found) = self.get(&key) {
            return Ok(found);
        }

        let gate = self
            .compiling
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key)
            .or_default()
            .clone();
        // Poisoning only means an earlier compile panicked; the lock guards
        // nothing but the right to compile, so the next caller may.
        let turn = gate.lock().unwrap_or_else(|e| e.into_inner());

        // Whoever held the lock before us may have left the build behind.
        let result = match self.get(&key) {
            Some(found) => Ok(found),
            None => compile().inspect(|built| self.insert(key, built.clone())),
        };

        // Dropped by the last one out, so a key is not remembered for ever.
        //
        // Our own handles go first, and only then is the count read. A clone
        // is only ever made under `compiling`, and each is released before
        // its holder takes `compiling` again, so whoever takes it last sees
        // the map's handle alone. Counting while still holding ours read "2"
        // as "nobody else": a caller just ahead of us, between releasing its
        // turn and its handle, made it 3 for everyone, and the key stayed.
        drop(turn);
        drop(gate);
        let mut compiling = self.compiling.lock().unwrap_or_else(|e| e.into_inner());
        if compiling
            .get(&key)
            .is_some_and(|g| Arc::strong_count(g) == 1)
        {
            compiling.remove(&key);
        }
        result
    }

    fn get(&self, key: &[u8; 32]) -> Option<T> {
        let mut entries = self.entries.lock().ok()?;
        Self::drop_idle(&mut entries);
        let found = entries.iter().position(|e| &e.key == key)?;
        // Move to the back so eviction takes the least recently used.
        let mut entry = entries.remove(found);
        entry.last_used = std::time::Instant::now();
        let component = entry.component.clone();
        entries.push(entry);
        Some(component)
    }

    fn insert(&self, key: [u8; 32], component: T) {
        let Ok(mut entries) = self.entries.lock() else {
            // A poisoned cache is a lost optimization, not a lost turn.
            return;
        };
        Self::drop_idle(&mut entries);
        if entries.iter().any(|e| e.key == key) {
            return;
        }
        entries.push(CompiledEntry {
            key,
            component,
            last_used: std::time::Instant::now(),
        });
        if entries.len() > COMPILED_CACHE_ENTRIES {
            entries.remove(0);
        }
    }

    /// Forgets what has not been asked for lately.
    ///
    /// The size cap alone would not do this. A node serving one agent holds
    /// one entry, so a version that has been superseded is never pushed out --
    /// it just sits there, resident for the life of the process, holding the
    /// compiled form of code nobody runs any more. Redeploys are exactly when
    /// that happens, and the old bytes are never asked for again.
    ///
    /// Swept on access rather than by a timer: a cache nobody is using is not
    /// costing anything to sweep, and one that is busy sweeps constantly.
    fn drop_idle(entries: &mut Vec<CompiledEntry<T>>) {
        let now = std::time::Instant::now();
        entries.retain(|e| now.duration_since(e.last_used) < COMPILED_CACHE_IDLE);
    }
}

#[cfg(test)]
mod compiled_cache_tests {
    use super::*;

    fn key(n: u8) -> [u8; 32] {
        [n; 32]
    }

    fn cache() -> CompiledCache<u32> {
        CompiledCache::new()
    }

    #[test]
    fn what_was_put_in_comes_back_out() {
        let c = cache();
        assert_eq!(c.get(&key(1)), None);
        c.insert(key(1), 11);
        assert_eq!(c.get(&key(1)), Some(11));
    }

    #[test]
    fn different_bytes_never_share_a_build() {
        // The whole reason for keying on content: a changed component must
        // not be served the build of the one it replaced.
        let c = cache();
        c.insert(key(1), 11);
        c.insert(key(2), 22);
        assert_eq!(c.get(&key(1)), Some(11));
        assert_eq!(c.get(&key(2)), Some(22));
    }

    #[test]
    fn a_full_cache_drops_the_least_recently_used() {
        let c = cache();
        for n in 0..COMPILED_CACHE_ENTRIES as u8 {
            c.insert(key(n), n as u32);
        }
        // Touch the oldest so it is no longer the one to go.
        assert_eq!(c.get(&key(0)), Some(0));
        c.insert(key(200), 200);

        assert_eq!(c.get(&key(0)), Some(0), "a recently used entry was evicted");
        assert_eq!(
            c.get(&key(1)),
            None,
            "the least recently used entry survived"
        );
        assert_eq!(c.get(&key(200)), Some(200));
    }

    #[test]
    fn a_build_nobody_asks_for_does_not_stay_resident() {
        // A node serving one agent never fills the cache, so the size cap
        // would keep a superseded build for the life of the process.
        let c = cache();
        c.insert(key(1), 11);
        {
            let mut entries = c.entries.lock().expect("lock");
            entries[0].last_used -= COMPILED_CACHE_IDLE + std::time::Duration::from_secs(1);
        }
        assert_eq!(c.get(&key(1)), None, "an idle build outlived its welcome");
        assert!(c.entries.lock().expect("lock").is_empty());
    }

    #[test]
    fn concurrent_misses_share_one_build() {
        const CALLERS: usize = 8;
        let c = Arc::new(cache());
        let builds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let threads: Vec<_> = (0..CALLERS)
            .map(|_| {
                let (c, builds) = (c.clone(), builds.clone());
                let waiting = c.clone();
                std::thread::spawn(move || {
                    c.get_or_compile(key(1), || {
                        builds.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        // The build does not finish until every caller is
                        // waiting on it: the map's handle and one per caller.
                        // Waited for rather than slept through. Nothing is
                        // cached until this returns, so every caller misses
                        // and has to arrive here -- the wait always ends, and
                        // the test always exercises the sharing it is about.
                        while waiting
                            .compiling
                            .lock()
                            .expect("lock")
                            .get(&key(1))
                            .map(Arc::strong_count)
                            != Some(CALLERS + 1)
                        {
                            std::thread::yield_now();
                        }
                        Ok(11)
                    })
                    .expect("build")
                })
            })
            .collect();
        for t in threads {
            assert_eq!(t.join().expect("thread"), 11);
        }
        assert_eq!(builds.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(
            c.compiling.lock().expect("lock").is_empty(),
            "a finished key was kept"
        );
    }

    #[test]
    fn a_failed_build_is_not_what_the_next_caller_gets() {
        let c = cache();
        assert!(
            c.get_or_compile(key(1), || anyhow::bail!("broken"))
                .is_err()
        );
        assert_eq!(c.get_or_compile(key(1), || Ok(11)).expect("build"), 11);
    }

    #[test]
    fn staying_in_use_keeps_a_build_alive() {
        let c = cache();
        c.insert(key(1), 11);
        for _ in 0..3 {
            assert_eq!(c.get(&key(1)), Some(11));
        }
        assert_eq!(c.entries.lock().expect("lock").len(), 1);
    }
}
