//! Markdown on stdin, PDF on stdout. For looking at the output.
//!
//! Each argument is an image to send with it, as `name=file`: the name the
//! markdown draws it by, and the file to read it from. `THEME` and `ACCENT`
//! in the environment choose the look.
use std::io::{Read, Write};
fn main() {
    let mut md = String::new();
    std::io::stdin().read_to_string(&mut md).unwrap();
    let mut images = outturn_renderer_pdf::Images::new();
    for arg in std::env::args().skip(1) {
        let (name, file) = arg.split_once('=').expect("name=file");
        images.insert(name.to_string(), std::fs::read(file).unwrap());
    }
    let theme = outturn_renderer_pdf::Theme::named(
        std::env::var("THEME").ok().as_deref(),
        std::env::var("ACCENT").ok().as_deref(),
    )
    .unwrap();
    let pdf = outturn_renderer_pdf::render(&md, &images, &theme).unwrap();
    std::io::stdout().write_all(&pdf).unwrap();
}
