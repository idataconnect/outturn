//! The renderer service. See `outturn_renderer_pdf::service`.

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Fonts for what the built-in faces lack -- CJK, in the image that
    // carries them. A path given and not loaded is a broken deployment, said
    // at startup rather than as boxes in somebody's document.
    if let Some(regular) = font_file("FALLBACK_FONT") {
        let bold = font_file("FALLBACK_FONT_BOLD");
        let with_bold = bold.is_some();
        outturn_renderer_pdf::set_fallback_fonts(regular, bold)
            .unwrap_or_else(|e| panic!("the fallback fonts cannot be used: {e}"));
        tracing::info!(
            with_bold,
            "drawing what the built-in fonts lack from a fallback"
        );
    }

    let addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8090".into());
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("could not listen on {addr}: {e}"));
    tracing::info!(%addr, "rendering PDFs");
    axum::serve(listener, outturn_renderer_pdf::service::app())
        // SIGTERM is how Kubernetes asks; a render in flight finishes first.
        .with_graceful_shutdown(async {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("SIGTERM handler");
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        })
        .await
        .unwrap();
}

/// The font a variable names, and the face in it that `<variable>_INDEX`
/// names: a collection holds several, and the first is not always wanted.
fn font_file(variable: &str) -> Option<outturn_renderer_pdf::FontFile> {
    let path = std::env::var(variable).ok()?;
    let index = std::env::var(format!("{variable}_INDEX"))
        .ok()
        .map(|i| {
            i.parse()
                .unwrap_or_else(|_| panic!("{variable}_INDEX is not a number"))
        })
        .unwrap_or(0);
    let data = std::fs::read(&path).unwrap_or_else(|e| panic!("could not read {path}: {e}"));
    Some(outturn_renderer_pdf::FontFile { data, index })
}
