//! Tells cargo that the component bindings are built from `wit/`, which it
//! does not track: `wasmtime::component::bindgen!` reads it while the macro
//! expands. A change to the interface and nothing else would otherwise leave
//! a cached host implementing the interface as it was -- see the core crate's
//! build script for the failure that caused.

fn main() {
    // The interface the host implements and the guest imports. Both halves are
    // generated from this at compile time, so a change to it is a change to
    // every binary that links either one.
    println!("cargo:rerun-if-changed=../../wit/agent.wit");
    println!("cargo:rerun-if-changed=../../wit");
}
