//! Tells cargo what this crate is built from besides its own source.
//!
//! `wasmtime::component::bindgen!` reads `wit/agent.wit` while the macro
//! expands, and `sqlx::migrate!` reads `migrations/`. Cargo knows about
//! neither: it decides whether to rebuild from the mtimes of the files it
//! tracks, which are the ones under `src/`. So changing the interface and
//! nothing else leaves a cached build looking current, and the binary that
//! comes out implements the interface as it was.
//!
//! That is not theoretical. A turn then fails with "component imports instance
//! `outturn:agent/host`, but a matching implementation was not found in the
//! linker" -- a host built before the import existed, linking a component
//! built after. It survives a rebuild, a redeploy and a restart, because every
//! one of them asks cargo, and cargo says there is nothing to do. The image
//! build makes it worse by keeping `target/` in a cache mount, so the stale
//! artifact outlives the container that produced it.
//!
//! `artifact_guard` cannot catch this. It compares two committed files and they
//! agree; what disagrees is a binary neither of them can see.

fn main() {
    // The interface the host implements and the guest imports. Both halves are
    // generated from this at compile time, so a change to it is a change to
    // every binary that links either one.
    println!("cargo:rerun-if-changed=wit/agent.wit");

    // Whole directories, because a file added to one is as much a change as a
    // file edited in it -- and a new migration that did not trigger a rebuild
    // is the same failure wearing different clothes.
    println!("cargo:rerun-if-changed=wit");
    println!("cargo:rerun-if-changed=migrations");
}
