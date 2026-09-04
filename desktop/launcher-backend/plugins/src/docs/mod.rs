// SPDX-License-Identifier: GPL-3.0-only
// claw-os: Recoll-backed document full-text search plugin.
//
// Shells out to `cos app docs search --query <q> --max-results 10`
// — the same backend that powers the AI Assist drawer in the Files
// app — so launcher and Files share one index, one capability gate
// and one audit trail. Matches are limited to the top 10 to keep
// the launcher snappy; refine the query for narrower results.

use futures::*;
use pop_launcher::*;
use serde::Deserialize;
use std::cell::Cell;
use std::path::PathBuf;
use std::process::Stdio;
use std::rc::Rc;
use tokio::process::Command;

const MAX_RESULTS: u8 = 10;
const QUERY_TIMEOUT_SECS: u64 = 5;

#[derive(Debug, Deserialize)]
struct SearchEnvelope {
    #[serde(default)]
    hint: Option<String>,
    #[serde(default)]
    results: Vec<SearchHit>,
}

#[derive(Debug, Deserialize)]
struct SearchHit {
    #[serde(default)]
    path: PathBuf,
    #[serde(default)]
    snippet: String,
}

#[derive(Debug)]
enum Event {
    Activate(u32),
    Search(String),
}

pub async fn main() {
    let (event_tx, event_rx) = flume::bounded::<Event>(20);
    let (interrupt_tx, interrupt_rx) = flume::bounded::<()>(1);
    let active = Rc::new(Cell::new(false));

    let mut app = SearchContext {
        search_results: Vec::with_capacity(16),
        active: active.clone(),
        interrupt_rx,
        out: async_stdout(),
    };

    let search_handler = async move {
        while let Ok(event) = event_rx.recv_async().await {
            match event {
                Event::Activate(id) => {
                    if let Some(path) = app.search_results.get(id as usize) {
                        let path = path.clone();
                        tokio::spawn(async move {
                            crate::xdg_open(&path);
                        });
                        crate::send(&mut app.out, PluginResponse::Close).await;
                    }
                }
                Event::Search(query) => {
                    app.search(query).await;
                    app.active.set(false);
                    crate::send(&mut app.out, PluginResponse::Finished).await;
                }
            }
        }
    };

    let request_handler = async move {
        let interrupt = || {
            let active = active.clone();
            let tx = interrupt_tx.clone();
            async move {
                if active.get() {
                    let _ = tx.try_send(());
                }
            }
        };

        let mut requests = json_input_stream(async_stdin());

        while let Some(result) = requests.next().await {
            match result {
                Ok(request) => match request {
                    Request::Activate(id) => {
                        event_tx.send_async(Event::Activate(id)).await?;
                    }
                    Request::Interrupt => interrupt().await,
                    Request::Search(query) => {
                        interrupt().await;

                        // Strip the `docs ` prefix the launcher leaves on
                        // the front of the query (matches the regex in
                        // plugin.ron).
                        let query = match query.find(' ') {
                            Some(pos) => query[pos..].trim().to_string(),
                            None => String::new(),
                        };

                        if !query.is_empty() {
                            event_tx.send_async(Event::Search(query)).await?;
                            active.set(true);
                        }
                    }
                    _ => (),
                },
                Err(why) => {
                    tracing::error!("malformed JSON input: {}", why);
                }
            }
        }

        Ok::<(), flume::SendError<Event>>(())
    };

    let _ = futures::future::join(request_handler, search_handler).await;
}

struct SearchContext {
    pub active: Rc<Cell<bool>>,
    pub interrupt_rx: flume::Receiver<()>,
    pub out: tokio::io::Stdout,
    pub search_results: Vec<PathBuf>,
}

impl SearchContext {
    async fn append(&mut self, id: u32, hit: &SearchHit) {
        let name = hit
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| hit.path.display().to_string());

        // Prefer the Recoll snippet (already centred on the matched
        // terms) as the description; fall back to the path so the user
        // can still tell where the file lives.
        let mut description = hit.snippet.replace('\n', " ");
        description = description.split_whitespace().collect::<Vec<_>>().join(" ");
        if description.is_empty() {
            description = hit.path.display().to_string();
        }

        let response = PluginResponse::Append(PluginSearchResult {
            id,
            description,
            name,
            icon: Some(IconSource::Mime(crate::mime_from_path(&hit.path))),
            ..Default::default()
        });

        crate::send(&mut self.out, response).await;
        self.search_results.push(hit.path.clone());
    }

    async fn append_message(&mut self, id: u32, name: String, description: String) {
        let response = PluginResponse::Append(PluginSearchResult {
            id,
            name,
            description,
            icon: Some(IconSource::Name("dialog-information".into())),
            ..Default::default()
        });
        crate::send(&mut self.out, response).await;
    }

    async fn search(&mut self, query: String) {
        self.search_results.clear();

        // Wrap the race in a block so the interrupt future's borrow of
        // `self.interrupt_rx` ends before we reach for `&mut self` to
        // append results below.
        let outcome = {
            let interrupt = async {
                let _ = self.interrupt_rx.recv_async().await;
            };
            let work = run_query(&query);

            futures::pin_mut!(interrupt);
            futures::pin_mut!(work);

            match futures::future::select(interrupt, work).await {
                futures::future::Either::Left(_) => return,
                futures::future::Either::Right((outcome, _)) => outcome,
            }
        };

        match outcome {
            Ok(envelope) => {
                if envelope.results.is_empty() {
                    let msg = envelope.hint.unwrap_or_else(|| String::from("No matches"));
                    self.append_message(0, String::from("docs: no results"), msg)
                        .await;
                    return;
                }
                for (idx, hit) in envelope.results.iter().enumerate() {
                    self.append(idx as u32, hit).await;
                    if idx + 1 == MAX_RESULTS as usize {
                        break;
                    }
                }
            }
            Err(why) => {
                self.append_message(0, String::from("docs: error"), why)
                    .await;
            }
        }
    }
}

async fn run_query(query: &str) -> Result<SearchEnvelope, String> {
    let bin = std::env::var("CLAW_COS_BIN").unwrap_or_else(|_| "cos".into());
    let mut cmd = Command::new(&bin);
    cmd.args([
        "app",
        "docs",
        "search",
        "--query",
        query,
        "--max-results",
        &MAX_RESULTS.to_string(),
    ]);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let timeout = std::time::Duration::from_secs(QUERY_TIMEOUT_SECS);
    let output = match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(why)) => {
            return Err(format!(
                "failed to spawn `{} app docs search`: {}",
                bin, why
            ));
        }
        Err(_) => {
            return Err(format!(
                "`{} app docs search` timed out after {}s",
                bin, QUERY_TIMEOUT_SECS
            ));
        }
    };

    let status = output.status.to_string();
    decode_search_output(
        output.status.success(),
        &status,
        &output.stdout,
        &output.stderr,
    )
}

fn decode_search_output(
    success: bool,
    status: &str,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<SearchEnvelope, String> {
    let stdout = String::from_utf8_lossy(stdout);
    let trimmed = stdout.trim();
    if !success {
        let stderr = String::from_utf8_lossy(stderr);
        let detail = if stderr.trim().is_empty() {
            trimmed
        } else {
            stderr.trim()
        };
        return Err(if detail.is_empty() {
            format!("cos failed without a diagnostic ({status})")
        } else {
            format!("cos failed ({status}): {detail}")
        });
    }
    if trimmed.is_empty() {
        return Err("cos produced no output".to_string());
    }
    serde_json::from_str::<SearchEnvelope>(trimmed).map_err(|e| format!("bad JSON from cos: {}", e))
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/docs.rs"));
}
