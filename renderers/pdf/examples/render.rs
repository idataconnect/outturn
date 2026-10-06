//! Markdown on stdin, PDF on stdout. For looking at the output.
use std::io::{Read, Write};
fn main() {
    let mut md = String::new();
    std::io::stdin().read_to_string(&mut md).unwrap();
    let pdf = outturn_renderer_pdf::render(&md).unwrap();
    std::io::stdout().write_all(&pdf).unwrap();
}
