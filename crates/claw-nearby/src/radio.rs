//! Dedicated Owly radio process. No file-transfer parser runs as root.

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    if std::env::args_os().len() != 1 {
        return Err("the radio service accepts no runtime selectors".into());
    }
    let backend = owly_radio::injection::Injection::from_config(owly_radio::rundir::RunDir::new(
        owly_radio::rundir::RUN_DIR,
    ))?;
    let probe = backend.prober();
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    owlyd::service::serve(
        backend,
        probe,
        owlyd::service::Options {
            bus: None,
            polkit: true,
        },
        async move {
            terminate.recv().await;
        },
    )
    .await
    .map_err(Into::into)
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("Nearby radio requires Linux and a compatible physical Wi-Fi card.");
    std::process::exit(1);
}
