//! Seals a credential to a gateway, for storing with `POST /v1/credentials`.
//!
//! ```text
//! printf %s "$KEY" | outturn-seal --public-key <hex> --workspace <id> \
//!     --host books.example.com --header authorization --name "Bigcapital" > body.json
//! ```
//!
//! The secret is read from stdin, never an argument, so it is not left in a
//! shell's history or a process listing. The public key is given here rather
//! than fetched from the API: a CLI that asked the API for the key would seal
//! to whatever a compromised API served, which could be its own -- so the key
//! comes from the operator's own copy, and that pin is the whole of what this
//! path protects. See docs/sealed-credentials.md.
//!
//! What it prints is the request body, with the secret sealed: the API never
//! sees it, and neither does anything that logs the request.

use std::io::Read;

use base64::Engine as _;
use outturn::egress::seal;

fn usage() -> ! {
    eprintln!(
        "usage: outturn-seal --public-key <hex> --workspace <id> --host <name> [--host <name>...] \
         --header <name> --name <label> [--id <credential id, to rotate one>]   (the secret on stdin)"
    );
    std::process::exit(2)
}

fn main() {
    let mut public = None;
    let mut workspace = None;
    let mut hosts = Vec::new();
    let mut header = None;
    let mut name = None;
    // Given when rotating: the binding names the credential, so a new seal for
    // an existing one has to carry its id.
    let mut id = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--public-key" => public = Some(value),
            "--workspace" => workspace = Some(value),
            "--host" => hosts.push(value.to_ascii_lowercase()),
            "--header" => header = Some(value.to_ascii_lowercase()),
            "--name" => name = Some(value),
            "--id" => {
                id = Some(uuid::Uuid::parse_str(&value).unwrap_or_else(|_| {
                    eprintln!("--id is not a credential id");
                    std::process::exit(2)
                }))
            }
            _ => usage(),
        }
    }
    let (Some(public), Some(workspace), Some(header), Some(name)) =
        (public, workspace, header, name)
    else {
        usage()
    };
    if hosts.is_empty() {
        usage()
    }

    let public = hex::decode(public.trim()).unwrap_or_else(|_| {
        eprintln!("--public-key is not hex");
        std::process::exit(2)
    });

    let mut secret = String::new();
    std::io::stdin()
        .read_to_string(&mut secret)
        .expect("reading the secret from stdin");
    let secret = zeroize::Zeroizing::new(secret.trim_end_matches(['\n', '\r']).to_string());
    if secret.is_empty() {
        eprintln!("no secret on stdin");
        std::process::exit(2)
    }

    let id = id.unwrap_or_else(uuid::Uuid::now_v7);
    let binding = serde_json::to_vec(&serde_json::json!({
        "credential": id,
        "kind": "static",
        "workspaces": [workspace],
        "hosts": hosts,
        "header": header,
    }))
    .expect("a binding serializes");
    // Checked before sealing, so a mistake is said here rather than by the API.
    if let Err(why) = seal::Binding::parse(&binding) {
        eprintln!("{why}");
        std::process::exit(2)
    }
    let sealed = seal::seal(&public, &binding, secret.as_bytes()).unwrap_or_else(|why| {
        eprintln!("{why}");
        std::process::exit(1)
    });

    let b64 = base64::engine::general_purpose::STANDARD;
    let body = serde_json::json!({
        "id": id,
        "name": name,
        "binding": b64.encode(&binding),
        "sealed": b64.encode(&sealed),
        "key_id": seal::key_id(&public),
    });
    println!("{body}");
}
