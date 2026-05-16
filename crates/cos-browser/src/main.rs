use std::sync::Arc;
use std::time::Instant;

use clap::{Parser, Subcommand};
use obscura_browser::{BrowserContext, Page};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::time::{timeout, Duration};
use url::Url;

// Shared SSRF policy. Lives in `url_safety.rs` so the worker binary
// can include it too — cos-browser has two `[[bin]]` targets and no
// library, so we share via `#[path]` instead of `use crate::…`.
#[path = "url_safety.rs"]
mod url_safety;
use url_safety::{recheck_no_rebind, validate_navigable_url};

#[derive(Parser)]
#[command(name = "cos-browser", about = "cos-browser - Agent-first headless browser for Claw OS (vendored from Obscura)")]
struct Args {
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Command>,

    #[arg(short, long, default_value_t = 9222)]
    port: u16,

    #[arg(long)]
    proxy: Option<String>,

    #[arg(long)]
    obey_robots: bool,

    #[arg(long)]
    user_agent: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(short, long, default_value_t = 9222)]
        port: u16,

        #[arg(long)]
        proxy: Option<String>,

        #[arg(long)]
        user_agent: Option<String>,

        #[arg(long)]
        stealth: bool,

        #[arg(long, default_value_t = 1)]
        workers: u16,
    },

    Fetch {
        url: String,

        #[arg(long, default_value = "html")]
        dump: DumpFormat,

        #[arg(long)]
        selector: Option<String>,

        #[arg(long, default_value_t = 5)]
        wait: u64,

        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,

        #[arg(long, default_value = "load")]
        wait_until: String,

        #[arg(long)]
        user_agent: Option<String>,

        #[arg(long)]
        stealth: bool,

        #[arg(long, short)]
        eval: Option<String>,

        #[arg(long, short)]
        quiet: bool,
    },

    Scrape {
        urls: Vec<String>,

        #[arg(long, short)]
        eval: Option<String>,

        #[arg(long, default_value_t = std::num::NonZeroUsize::new(10).unwrap())]
        concurrency: std::num::NonZeroUsize,

        #[arg(long, default_value = "json")]
        format: String,

        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
    },

    /// Capture a page screenshot.
    ///
    /// cos-browser has no rendering engine of its own, so this shells out to
    /// `chromium --headless`. Override the chromium binary with the
    /// $COS_CHROMIUM_BIN environment variable (defaults to `chromium`).
    Screenshot {
        url: String,

        #[arg(long, short)]
        output: std::path::PathBuf,

        #[arg(long, default_value_t = 1280)]
        width: u32,

        #[arg(long, default_value_t = 720)]
        height: u32,

        /// Compute the document scroll height first and capture the full page.
        /// Adds a short pre-fetch through Obscura before invoking chromium.
        #[arg(long)]
        full_page: bool,

        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
    },

}


#[derive(Clone, Debug, clap::ValueEnum)]
enum DumpFormat {
    Html,
    Text,
    Links,
}

fn print_banner(port: u16) {
    println!(r#"
   _____ ____  _____    ____                                   
  / ____/ __ \/ ____|  | __ ) _ __ _____      _____  ___ _ __  
 | |   | |  | (___    |  _ \| '__/ _ \ \ /\ / / __|/ _ \ '__| 
 | |   | |  | \___ \   | |_) | | | (_) \ V  V /\__ \  __/ |    
 | |___| |__| |___) |  |____/|_|  \___/ \_/\_/ |___/\___|_|    
  \_____\____/_____/                                            

  Claw OS browser engine v0.1.0 (vendored from Obscura)
  CDP server: ws://127.0.0.1:{}/devtools/browser
"#, port);
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let filter = if args.verbose { "debug" } else { "warn" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter)),
        )
        .with_writer(std::io::stderr)
        .init();

    match args.command {
        Some(Command::Serve { port, proxy, user_agent, stealth, workers }) => {
            print_banner(port);
            if let Some(ref proxy) = proxy {
                tracing::info!("Using proxy: {}", proxy);
            }
            if let Some(ref ua) = user_agent {
                tracing::info!("User-Agent: {}", ua);
            }
            if stealth {
                #[cfg(feature = "stealth")]
                tracing::info!(
                    "Stealth mode enabled (TLS fingerprint impersonation + tracker blocking)"
                );
                #[cfg(not(feature = "stealth"))]
                tracing::info!("Stealth mode enabled (tracker blocking)");
            }

            if workers > 1 {
                tracing::info!("{} worker processes", workers);
                run_multi_worker_serve(port, workers, proxy, stealth, user_agent).await?;
            } else {
                obscura_cdp::start_with_full_options(port, proxy, stealth, user_agent).await?;
            }
        }
        Some(Command::Fetch { url, dump, selector, wait, timeout, wait_until, user_agent, stealth, eval, quiet }) => {
            run_fetch(&url, dump, selector, wait, timeout, &wait_until, user_agent, stealth, eval, quiet).await?;
        }
        Some(Command::Scrape { urls, eval, concurrency, format, timeout }) => {
            run_parallel_scrape(urls, eval, concurrency.get(), &format, timeout).await?;
        }
        Some(Command::Screenshot { url, output, width, height, full_page, timeout }) => {
            run_screenshot(&url, &output, width, height, full_page, timeout).await?;
        }
        None => {
            print_banner(args.port);
            if let Some(ref proxy) = args.proxy {
                tracing::info!("Using proxy: {}", proxy);
            }
            obscura_cdp::start_with_options(args.port, args.proxy, false).await?;
        }
    }

    Ok(())
}

async fn run_multi_worker_serve(
    port: u16,
    workers: u16,
    proxy: Option<String>,
    stealth: bool,
    user_agent: Option<String>,
) -> anyhow::Result<()> {
    use tokio::net::TcpListener;
    use tokio::io::AsyncWriteExt as _;

    let exe = std::env::current_exe()?;
    let mut children = Vec::new();

    for i in 0..workers {
        // `port + 1 + i` would overflow u16 silently for any
        // `port` close to 65535 (and any non-trivial worker count).
        // Use checked arithmetic so the spawn fails loudly instead
        // of binding workers on wrapped-around port numbers.
        let worker_port = port
            .checked_add(1)
            .and_then(|p| p.checked_add(i))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "worker port {} + 1 + {} overflows u16",
                    port,
                    i
                )
            })?;
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("serve").arg("--port").arg(worker_port.to_string());
        if let Some(ref p) = proxy {
            cmd.arg("--proxy").arg(p);
        }
        if let Some(ref ua) = user_agent {
            cmd.arg("--user-agent").arg(ua);
        }
        if stealth {
            cmd.arg("--stealth");
        }
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());

        let child = cmd.spawn()?;
        tracing::info!("Worker {} on port {}", i + 1, worker_port);
        children.push(child);
    }

    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("Load balancer on port {}, {} workers", port, workers);

    let mut next_worker: u16 = 0;

    loop {
        let (client_stream, peer_addr) = listener.accept().await?;
        // Same overflow guard as the worker-spawn loop: a `port`
        // close to 65535 plus `(next_worker % workers)` would wrap
        // silently and route to the wrong process.
        let worker_offset = next_worker % workers;
        let worker_port = port
            .checked_add(1)
            .and_then(|p| p.checked_add(worker_offset))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "worker port {} + 1 + {} overflows u16",
                    port,
                    worker_offset
                )
            })?;
        next_worker = next_worker.wrapping_add(1);

        tracing::debug!("Routing {} to worker port {}", peer_addr, worker_port);

        // The original code called `peek(&mut buf)` once and treated
        // the result as if it always filled the buffer. peek() is
        // free to return fewer bytes than requested (and routinely
        // does when the TCP segment hasn't been fully assembled yet),
        // so we'd compare a partial prefix against `b"GET "` and
        // route the connection as if it weren't HTTP. Loop until we
        // either see four bytes or the peer half-closes.
        let mut peek_buf = [0u8; 4];
        let mut got = 0usize;
        while got < peek_buf.len() {
            let n = client_stream.peek(&mut peek_buf[got..]).await?;
            if n == 0 {
                break;
            }
            // peek() returns the *cumulative* count including any
            // earlier bytes that were already in the kernel buffer —
            // just take what it gave us and try again if short.
            got = n.max(got);
            if n >= peek_buf.len() {
                break;
            }
        }
        let peek_ready = &peek_buf[..got];

        if peek_ready == b"GET " {
            let mut full_peek = [0u8; 256];
            let n = client_stream.peek(&mut full_peek).await?;
            let request_line = String::from_utf8_lossy(&full_peek[..n]);

            if request_line.contains("/json") {
                let worker_addr = format!("127.0.0.1:{}", worker_port);
                match tokio::net::TcpStream::connect(&worker_addr).await {
                    Ok(mut worker_stream) => {
                        tokio::spawn(async move {
                            let std_stream = match client_stream.into_std() {
                                Ok(s) => s,
                                Err(e) => {
                                    tracing::error!(
                                        "/json: failed to convert client to std stream: {}",
                                        e
                                    );
                                    return;
                                }
                            };
                            let mut client = match tokio::net::TcpStream::from_std(std_stream) {
                                Ok(c) => c,
                                Err(e) => {
                                    tracing::error!(
                                        "/json: failed to recreate tokio TcpStream: {}",
                                        e
                                    );
                                    return;
                                }
                            };
                            let _ = tokio::io::copy_bidirectional(
                                &mut client,
                                &mut worker_stream,
                            )
                            .await;
                        });
                    }
                    Err(e) => {
                        tracing::warn!("/json worker {} unreachable: {}", worker_addr, e);
                        tokio::spawn(async move {
                            let mut s = client_stream;
                            let _ = s
                                .write_all(
                                    b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n",
                                )
                                .await;
                            let _ = s.shutdown().await;
                        });
                    }
                }
                continue;
            }
        }

        let worker_addr = format!("127.0.0.1:{}", worker_port);
        tokio::spawn(async move {
            match tokio::net::TcpStream::connect(&worker_addr).await {
                Ok(mut worker_stream) => {
                    let mut client = client_stream;
                    let _ =
                        tokio::io::copy_bidirectional(&mut client, &mut worker_stream).await;
                }
                Err(e) => {
                    tracing::warn!("worker {} unreachable: {}", worker_addr, e);
                    let mut s = client_stream;
                    let _ = s
                        .write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n")
                        .await;
                    let _ = s.shutdown().await;
                }
            }
        });
    }
}

async fn run_fetch(
    url_str: &str,
    dump: DumpFormat,
    selector: Option<String>,
    wait_secs: u64,
    timeout_secs: u64,
    wait_until: &str,
    user_agent: Option<String>,
    stealth: bool,
    eval: Option<String>,
    quiet: bool,
) -> anyhow::Result<()> {
    // Reject internal, file://, and DNS-resolved private IPs before
    // we hand the URL to obscura. Keeps every SSRF gadget that lives
    // behind a public hostname (e.g. `nip.io` → 127.0.0.1, CNAME to
    // a metadata service IP) from reaching the network stack.
    let (validated_url, resolved_before) = validate_navigable_url(url_str)?;
    let _ = validated_url; // navigation uses the original string

    let context = Arc::new(BrowserContext::with_options("fetch".to_string(), None, stealth));
    let mut page = Page::new("fetch-page".to_string(), context);

    if let Some(ref ua) = user_agent {
        page.http_client.set_user_agent(ua).await;
    }

    let wait_condition = obscura_browser::lifecycle::WaitUntil::from_str(wait_until);

    if !quiet {
        eprintln!("Fetching {}...", url_str);
    }

    match timeout(Duration::from_secs(timeout_secs), page.navigate_with_wait(url_str, wait_condition)).await {
        Ok(result) => result.map_err(|e| anyhow::anyhow!("Failed to navigate to {}: {}", url_str, e))?,
        Err(_) => anyhow::bail!(
            "Timed out navigating to {} after {}s",
            url_str,
            timeout_secs
        ),
    }

    // DNS-rebinding check: after navigation, re-resolve the host and
    // confirm the IP set didn't suddenly include a private address.
    if let Err(e) = recheck_no_rebind(
        &Url::parse(url_str).unwrap_or_else(|_| Url::parse("about:blank").unwrap()),
        &resolved_before,
    ) {
        anyhow::bail!("post-fetch validation failed: {}", e);
    }

    if !quiet {
        eprintln!("Page loaded: {} - \"{}\"", page.url_string(), page.title);
    }

    if let Some(ref sel) = selector {
        let found = wait_for_selector(&mut page, sel, wait_secs).await;
        if !found {
            eprintln!("Warning: selector '{}' not found after {}s", sel, wait_secs);
        }
    }

    if let Some(ref expr) = eval {
        let result = page.evaluate(expr);
        match result {
            serde_json::Value::String(s) => println!("{}", s),
            serde_json::Value::Null => println!("null"),
            other => println!("{}", other),
        }
        return Ok(());
    }

    match dump {
        DumpFormat::Html => {
            dump_html(&page);
        }
        DumpFormat::Text => {
            dump_text(&mut page);
        }
        DumpFormat::Links => {
            dump_links(&page);
        }
    }

    Ok(())
}

async fn wait_for_selector(page: &mut Page, selector: &str, timeout_secs: u64) -> bool {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(timeout_secs);
    loop {
        let found = page.with_dom(|dom| {
            dom.query_selector(selector).ok().flatten().is_some()
        }).unwrap_or(false);

        if found {
            return true;
        }

        if tokio::time::Instant::now() >= deadline {
            return false;
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
}

fn dump_html(page: &Page) {
    page.with_dom(|dom| {
        if let Ok(Some(html_node)) = dom.query_selector("html") {
            let html = dom.outer_html(html_node);
            println!("<!DOCTYPE html>");
            println!("{}", html);
        } else {
            let doc = dom.document();
            let html = dom.inner_html(doc);
            println!("{}", html);
        }
    });
}

fn dump_text(page: &mut Page) {
    page.with_dom(|dom| {
        if let Ok(Some(body)) = dom.query_selector("body") {
            let text = extract_readable_text(dom, body);
            println!("{}", text.trim());
        }
    });
}

fn extract_readable_text(dom: &obscura_dom::DomTree, node_id: obscura_dom::NodeId) -> String {
    use obscura_dom::NodeData;

    let mut result = String::new();
    let node = match dom.get_node(node_id) {
        Some(n) => n,
        None => return result,
    };

    match &node.data {
        NodeData::Text { contents } => {
            let trimmed = contents.trim();
            if !trimmed.is_empty() {
                result.push_str(trimmed);
            }
        }
        NodeData::Element { name, .. } => {
            let tag = name.local.as_ref();
            let is_block = matches!(
                tag,
                "div" | "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                    | "li" | "tr" | "br" | "hr" | "blockquote" | "pre"
                    | "section" | "article" | "header" | "footer" | "nav"
                    | "main" | "aside" | "figure" | "figcaption" | "table"
                    | "thead" | "tbody" | "tfoot" | "dl" | "dt" | "dd"
                    | "ul" | "ol"
            );

            if tag == "script" || tag == "style" {
                return result;
            }

            if is_block {
                result.push('\n');
            }

            for child_id in dom.children(node_id) {
                result.push_str(&extract_readable_text(dom, child_id));
            }

            if is_block {
                result.push('\n');
            }
        }
        _ => {
            for child_id in dom.children(node_id) {
                result.push_str(&extract_readable_text(dom, child_id));
            }
        }
    }

    result
}

async fn run_parallel_scrape(
    urls: Vec<String>,
    eval: Option<String>,
    concurrency: usize,
    format: &str,
    timeout_secs: u64,
) -> anyhow::Result<()> {
    let total = urls.len();
    let start = Instant::now();

    if total == 0 {
        anyhow::bail!("No URLs provided. Pass at least one URL to scrape.");
    }

    // Validate every URL up-front. Catches typos and SSRF gadgets
    // before we burn time spawning workers for them. A single bad
    // URL fails the whole batch — easier than reporting per-URL
    // validation errors mixed in with real fetch failures. The
    // workers re-validate independently (so a rebinding peer can't
    // serve a different IP after this gate); we only do the
    // pre-flight here to fail fast on plainly-internal URLs.
    for u in &urls {
        let _ = validate_navigable_url(u)?;
    }

    eprintln!(
        "Scraping {} URLs with {} concurrent workers (per-worker timeout: {}s)...",
        total, concurrency, timeout_secs
    );

    let worker_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("cos-browser-worker")))
        .unwrap_or_else(|| std::path::PathBuf::from("cos-browser-worker"));

    if !worker_path.exists() {
        anyhow::bail!(
            "Worker binary not found at {}. Build with: cargo build --release",
            worker_path.display()
        );
    }

    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
    let eval = Arc::new(eval);
    let worker_path = Arc::new(worker_path);
    let worker_timeout = Duration::from_secs(timeout_secs);
    let read_timeout = Duration::from_secs(timeout_secs.min(30));
    let shutdown_timeout = Duration::from_secs(5);

    let mut handles = Vec::new();

    for (i, url) in urls.into_iter().enumerate() {
        let sem = semaphore.clone();
        let eval = eval.clone();
        let worker_path = worker_path.clone();

        let handle = tokio::spawn(async move {
            // Semaphore::acquire returns Err only when the Semaphore
            // has been closed — that doesn't happen anywhere in
            // this binary, but unwrap()ing on the Err arm would
            // panic the worker task and leave the parent waiting
            // forever for the JoinHandle. Convert to a graceful
            // failure result instead.
            let _permit = match sem.acquire().await {
                Ok(p) => p,
                Err(_) => {
                    return serde_json::json!({
                        "url": url,
                        "error": "semaphore closed",
                        "time_ms": 0,
                    });
                }
            };
            let task_start = Instant::now();

            let mut child = match TokioCommand::new(worker_path.as_ref())
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    return serde_json::json!({
                        "url": url,
                        "error": format!("Failed to spawn worker: {}", e),
                        "time_ms": task_start.elapsed().as_millis(),
                    });
                }
            };

            let mut stdin = match child.stdin.take() {
                Some(stdin) => stdin,
                None => {
                    let _ = timeout(shutdown_timeout, child.kill()).await;
                    return serde_json::json!({
                        "url": url,
                        "error": "Failed to open worker stdin",
                        "time_ms": task_start.elapsed().as_millis(),
                    });
                }
            };
            let stdout = match child.stdout.take() {
                Some(stdout) => stdout,
                None => {
                    let _ = timeout(shutdown_timeout, child.kill()).await;
                    return serde_json::json!({
                        "url": url,
                        "error": "Failed to open worker stdout",
                        "time_ms": task_start.elapsed().as_millis(),
                    });
                }
            };
            let mut reader = BufReader::new(stdout);

            let worker_result: Result<serde_json::Value, String> = match timeout(worker_timeout, async {
                let nav_cmd = serde_json::json!({"cmd": "navigate", "url": url});
                let mut line = serde_json::to_string(&nav_cmd).unwrap();
                line.push('\n');
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    return Err("Write failed".to_string());
                }
                if stdin.flush().await.is_err() {
                    return Err("Write failed".to_string());
                }

                let mut resp_line = String::new();
                match timeout(read_timeout, reader.read_line(&mut resp_line)).await {
                    Ok(Ok(bytes)) if bytes > 0 => {}
                    Ok(Ok(_)) | Ok(Err(_)) => return Err("Read failed".to_string()),
                    Err(_) => return Err("timeout".to_string()),
                };

                let nav_resp: serde_json::Value = match serde_json::from_str(resp_line.trim()) {
                    Ok(v) => v,
                    Err(e) => {
                        return Err(format!(
                            "worker emitted non-JSON navigate response: {} (raw: {:?})",
                            e,
                            // Truncate to keep this off-log of any
                            // sensitive content the worker echoed.
                            resp_line.chars().take(120).collect::<String>(),
                        ));
                    }
                };

                if !nav_resp["ok"].as_bool().unwrap_or(false) {
                    return Err(
                        nav_resp["error"]
                            .as_str()
                            .unwrap_or("navigate failed")
                            .to_string(),
                    );
                }

                let title = nav_resp["result"]["title"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();

                let eval_result = if let Some(ref expr) = *eval {
                    let eval_cmd = serde_json::json!({"cmd": "evaluate", "expression": expr});
                    let mut line = serde_json::to_string(&eval_cmd).unwrap();
                    line.push('\n');
                    if stdin.write_all(line.as_bytes()).await.is_err() {
                        return Err("Write failed".to_string());
                    }
                    if stdin.flush().await.is_err() {
                        return Err("Write failed".to_string());
                    }

                    let mut resp_line = String::new();
                    match timeout(read_timeout, reader.read_line(&mut resp_line)).await {
                        Ok(Ok(bytes)) if bytes > 0 => {
                            let resp: serde_json::Value = match serde_json::from_str(resp_line.trim()) {
                                Ok(v) => v,
                                Err(e) => {
                                    return Err(format!(
                                        "worker emitted non-JSON eval response: {} (raw: {:?})",
                                        e,
                                        resp_line.chars().take(120).collect::<String>(),
                                    ));
                                }
                            };
                            resp["result"].clone()
                        }
                        Ok(Ok(_)) | Ok(Err(_)) => return Err("Read failed".to_string()),
                        Err(_) => return Err("timeout".to_string()),
                    }
                } else {
                    serde_json::Value::Null
                };

                let shutdown_cmd = serde_json::json!({"cmd": "shutdown"});
                let mut line = serde_json::to_string(&shutdown_cmd).unwrap();
                line.push('\n');
                let _ = stdin.write_all(line.as_bytes()).await;
                let _ = stdin.flush().await;
                let _ = timeout(shutdown_timeout, child.wait()).await;

                Ok(serde_json::json!({
                    "url": url,
                    "title": title,
                    "eval": eval_result,
                    "time_ms": task_start.elapsed().as_millis(),
                    "worker": i,
                }))
            })
            .await
            {
                Ok(result) => result,
                Err(_) => Err("timeout".to_string()),
            };

            match worker_result {
                Ok(result) => result,
                Err(error) => {
                    let _ = timeout(shutdown_timeout, child.kill()).await;
                    serde_json::json!({
                        "url": url,
                        "error": error,
                        "time_ms": task_start.elapsed().as_millis(),
                    })
                }
            }
        });

        handles.push(handle);
    }

    let mut results = Vec::new();
    for handle in handles {
        match handle.await {
            Ok(result) => results.push(result),
            Err(e) => results.push(serde_json::json!({"error": e.to_string()})),
        }
    }

    let total_time = start.elapsed();

    if format == "json" {
        let avg_ms = if total > 0 {
            total_time.as_millis() as f64 / total as f64
        } else {
            // Belt-and-braces: the early bail on `total == 0` should
            // prevent us getting here, but keep the math defensible
            // so a future refactor that drops the bail can't trip
            // a div-by-zero on the metrics path.
            0.0
        };
        let output = serde_json::json!({
            "total_urls": total,
            "concurrency": concurrency,
            "total_time_ms": total_time.as_millis(),
            "avg_time_ms": avg_ms,
            "results": results,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        for r in &results {
            let url = r["url"].as_str().unwrap_or("?");
            let title = r["title"].as_str().unwrap_or("");
            let time = r["time_ms"].as_u64().unwrap_or(0);
            let eval = &r["eval"];
            if eval.is_null() {
                println!("{}ms\t{}\t{}", time, url, title);
            } else {
                println!("{}ms\t{}\t{}", time, url, eval);
            }
        }
        eprintln!(
            "\nTotal: {}ms for {} URLs ({} concurrent)",
            total_time.as_millis(),
            total,
            concurrency
        );
    }

    Ok(())
}

fn dump_links(page: &Page) {
    let base_url = page.url.clone();
    page.with_dom(|dom| {
        let links = dom.query_selector_all("a").unwrap_or_default();
        for link_id in links {
            if let Some(node) = dom.get_node(link_id) {
                let href = node.get_attribute("href").unwrap_or_default().to_string();
                let text = dom.text_content(link_id);
                let text = text.trim();

                let full_url = if href.starts_with("http://") || href.starts_with("https://") {
                    href.clone()
                } else if let Some(ref base) = base_url {
                    base.join(&href).map(|u| u.to_string()).unwrap_or(href.clone())
                } else {
                    href.clone()
                };

                if !full_url.is_empty() {
                    if text.is_empty() {
                        println!("{}", full_url);
                    } else {
                        println!("{}\t{}", full_url, text);
                    }
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// URL policy + screenshot subcommand (Claw OS additions)
// ---------------------------------------------------------------------------

// `validate_navigable_url`, `reject_private_ip`, and `recheck_no_rebind`
// now live in `url_safety.rs` so the worker binary can share them.

async fn run_screenshot(
    url_str: &str,
    output: &std::path::Path,
    width: u32,
    height: u32,
    full_page: bool,
    timeout_secs: u64,
) -> anyhow::Result<()> {
    let (validated_url, resolved_before) = validate_navigable_url(url_str)?;

    // Resolve a height: if --full-page, ask Obscura for document.scrollHeight
    // first so chromium captures the full document instead of just the
    // viewport. Otherwise honor --height as given.
    let final_height = if full_page {
        match probe_scroll_height(url_str, width, timeout_secs).await {
            Ok(h) if h > height => h,
            _ => height,
        }
    } else {
        height
    };

    let chromium_bin = std::env::var("COS_CHROMIUM_BIN")
        .unwrap_or_else(|_| "chromium".to_string());

    if which::which(&chromium_bin).is_err()
        && !std::path::Path::new(&chromium_bin).exists()
    {
        anyhow::bail!(
            "chromium binary '{}' not found on PATH. Set $COS_CHROMIUM_BIN or install chromium.",
            chromium_bin
        );
    }

    let abs_output = std::path::absolute(output).unwrap_or_else(|_| output.to_path_buf());
    if let Some(parent) = abs_output.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }

    let window_size = format!("{},{}", width, final_height);
    let screenshot_arg = format!("--screenshot={}", abs_output.display());
    let window_size_arg = format!("--window-size={}", window_size);

    let args: Vec<&str> = vec![
        "--headless=new",
        "--no-sandbox",
        "--disable-gpu",
        "--hide-scrollbars",
        "--disable-dev-shm-usage",
        &screenshot_arg,
        &window_size_arg,
        // `--` terminates chromium's option parsing so a URL that
        // starts with `--` (e.g. an attacker-crafted argv injection)
        // can't be reinterpreted as a chromium flag.
        "--",
        url_str,
    ];

    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        TokioCommand::new(&chromium_bin).args(&args).output(),
    )
    .await;

    match result {
        Ok(Ok(out)) if out.status.success() => {
            // After the navigation finishes, re-resolve the host so a
            // DNS-rebinding peer (returning a public IP for our
            // pre-flight check, then a private IP for chromium's
            // real fetch) can't slip past us. Failures here are
            // logged but don't tear down a successful screenshot —
            // recheck_no_rebind only bails on a confirmed rebind.
            if let Err(e) = recheck_no_rebind(&validated_url, &resolved_before) {
                anyhow::bail!("post-fetch validation failed: {}", e);
            }
            // Chromium writes screenshot to abs_output; verify and report.
            let exists = std::fs::metadata(&abs_output)
                .map(|m| m.is_file())
                .unwrap_or(false);
            let bytes = if exists {
                std::fs::metadata(&abs_output).map(|m| m.len()).unwrap_or(0)
            } else {
                0
            };
            println!(
                "{}",
                serde_json::json!({
                    "ok": exists,
                    "url": url_str,
                    "output": abs_output.display().to_string(),
                    "bytes": bytes,
                    "width": width,
                    "height": final_height,
                    "full_page": full_page,
                    "time_ms": started.elapsed().as_millis(),
                })
            );
            if !exists {
                anyhow::bail!("chromium exited 0 but no file at {}", abs_output.display());
            }
            Ok(())
        }
        Ok(Ok(out)) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!(
                "chromium failed (status {}): {}",
                out.status,
                stderr.trim()
            )
        }
        Ok(Err(e)) => Err(anyhow::anyhow!("failed to spawn chromium: {}", e)),
        Err(_) => anyhow::bail!(
            "chromium screenshot timed out after {}s",
            timeout_secs
        ),
    }
}

async fn probe_scroll_height(
    url_str: &str,
    width: u32,
    timeout_secs: u64,
) -> anyhow::Result<u32> {
    let context = Arc::new(BrowserContext::with_options(
        "screenshot-probe".to_string(),
        None,
        false,
    ));
    let mut page = Page::new("probe-page".to_string(), context);
    let _ = width; // Obscura has no layout, viewport width is informational only.
    let _ = timeout(
        Duration::from_secs(timeout_secs.min(15)),
        page.navigate_with_wait(url_str, obscura_browser::lifecycle::WaitUntil::Load),
    )
    .await
    .map_err(|_| anyhow::anyhow!("probe navigation timed out"))?
    .map_err(|e| anyhow::anyhow!("probe navigation failed: {}", e))?;
    let h = page.evaluate(
        "(function(){var d=document;var b=d.body||d.documentElement;return b?b.scrollHeight:0;})()",
    );
    let h = h.as_f64().unwrap_or(0.0) as u32;
    Ok(h.max(720))
}
