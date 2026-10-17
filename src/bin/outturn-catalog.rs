//! Builds the catalog snapshot a release ships, from a catalog document.
//!
//!     outturn-catalog <catalog.json> <out.json.gz>
//!
//! The reduction is the one the API applies to a catalog it fetches or is
//! given, so what ships and what is fetched later have the same shape and
//! compare by digest. `scripts/refresh-catalog.sh` fetches the document.

use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, input, output] = args.as_slice() else {
        eprintln!("usage: outturn-catalog <catalog.json> <out.json.gz>");
        std::process::exit(2);
    };
    let bytes = std::fs::read(input).unwrap_or_else(|e| fail(&format!("{input}: {e}")));
    let catalog = outturn::catalog::read(&bytes).unwrap_or_else(|e| fail(&e.to_string()));
    let (providers, models) = outturn::catalog::counts(&catalog);

    // Compressed hard and with no timestamp in the header, so the same
    // catalog builds the same bytes and a refresh that changed nothing is not
    // a change in git.
    let mut gz = flate2::GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), flate2::Compression::best());
    gz.write_all(catalog.to_string().as_bytes())
        .and_then(|_| gz.try_finish())
        .unwrap_or_else(|e| fail(&e.to_string()));
    let written = gz.finish().unwrap_or_else(|e| fail(&e.to_string()));
    std::fs::write(output, &written).unwrap_or_else(|e| fail(&format!("{output}: {e}")));
    println!(
        "{providers} providers, {models} models, {} KB",
        written.len() / 1024
    );
}

fn fail(why: &str) -> ! {
    eprintln!("outturn-catalog: {why}");
    std::process::exit(1)
}
