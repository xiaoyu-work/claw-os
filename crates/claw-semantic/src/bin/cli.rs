//! Human file-index diagnostics and real local semantic search.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use claw_embed::{local::LocalEmbedder, EmbedRequest, Embedder, SemanticStore};
use claw_semantic::{indexer, Config};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "claw-semantic", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    Status,
    Search {
        query: String,
        #[arg(short = 'k', long, default_value_t = 10)]
        k: usize,
    },
    Reindex,
    AddRoot {
        path: PathBuf,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut config = Config::load_or_default()?;
    config.validate()?;
    if let Action::AddRoot { path } = cli.command {
        let path = path
            .canonicalize()
            .context("resolve index source directory")?;
        anyhow::ensure!(path.is_dir(), "index source must be a directory");
        if !config.topdirs.contains(&path) {
            config.topdirs.push(path);
        }
        config.validate()?;
        config.save()?;
        println!(
            "{}",
            serde_json::json!({"source_directories":config.topdirs,"configuration_saved":true})
        );
        return Ok(());
    }
    let path = indexer::database_path()?;
    let embedder = LocalEmbedder::default();
    if matches!(cli.command, Action::Status) {
        let status = match embedder.status().await {
            Ok(status) => serde_json::to_value(status)?,
            Err(error) => {
                serde_json::json!({"configured":false,"detail":error,"model":claw_embed::local::LOCAL_MODEL})
            }
        };
        let corpus = if path.exists() {
            indexer::corpus_stats(&SemanticStore::open_read_only(&path)?)?
        } else {
            serde_json::json!({"n_paths":0,"n_chunks":0,"model":null,"state":"not_indexed","legacy_stub_index_used":false})
        };
        println!(
            "{}",
            serde_json::json!({
                "schema":1,"store_path":path,"topdirs":config.topdirs,"embedding":status,"corpus":corpus,
                "requested_chunk_chars":config.chunk_chars,
                "effective_chunk_chars":indexer::chunk_parameters(&config).0,
                "effective_chunk_overlap_chars":indexer::chunk_parameters(&config).1,
                "embedding_input_token_limit":claw_embed::local::MAX_INPUT_TOKENS,
                "coverage":"Configured personal-file directories and supported UTF-8 text, not every system file or format",
                "source_revisions":"sha256 of indexed file bytes; query snapshots are not current-file attestation",
            })
        );
        return Ok(());
    }
    let status = embedder.status().await.map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        status.configured,
        "local embedding is not configured: {}",
        status
            .detail
            .as_deref()
            .unwrap_or("model/runtime unavailable")
    );
    match cli.command {
        Action::Search { query, k } => {
            anyhow::ensure!(
                !query.trim().is_empty() && query.len() <= 8192 && (1..=100).contains(&k),
                "search requires a bounded query and k between 1 and 100"
            );
            let store = SemanticStore::open_read_only(&path)
                .context("file index is absent; run claw-semantic reindex")?;
            let response = embedder
                .embed(EmbedRequest {
                    inputs: vec![query.clone()],
                })
                .await?;
            let vector = response
                .embeddings
                .first()
                .context("local model returned no query vector")?;
            let hits =
                store.search_scoped_with_vector(vector, k, &response.model, &|namespace, _| {
                    namespace.strip_prefix("file/").is_some_and(|path| {
                        let path = std::path::Path::new(path);
                        !config.is_skipped(path)
                            && config.topdirs.iter().any(|root| path.starts_with(root))
                    })
                })?;
            let hits: Vec<_> = hits.into_iter().map(|hit|serde_json::json!({
                "path":hit.namespace.trim_start_matches("file/"),"key":hit.key,
                "snippet":hit.text,"score":hit.score,"model":hit.model,"indexed_at_ms":hit.ts_ms,
                "source":"indexed_file_snapshot",
            })).collect();
            println!(
                "{}",
                serde_json::json!({"query":query,"hits":hits,"model":response.model,"legacy_stub_index_used":false})
            );
        }
        Action::Reindex => {
            let store = indexer::open_store(&path)?;
            let files = claw_embed::walk::walk_filtered(&config.topdirs, 100_000, &|path| {
                config.is_skipped(path)
            })?;
            let mut indexed = 0;
            let mut current = 0;
            let mut unsupported = 0;
            let mut excluded = 0;
            let mut errors = Vec::new();
            for file in &files {
                match indexer::index_file(file, &config, &embedder, &store).await {
                    Ok(indexer::IndexOutcome::Indexed) => indexed += 1,
                    Ok(indexer::IndexOutcome::Current) => current += 1,
                    Ok(indexer::IndexOutcome::Unsupported) => unsupported += 1,
                    Ok(indexer::IndexOutcome::Excluded) => excluded += 1,
                    Err(error) => {
                        eprintln!("index {} failed: {error:#}", file.display());
                        errors.push(file.display().to_string());
                    }
                }
            }
            indexer::reconcile_files(&files, &store)?;
            println!(
                "{}",
                serde_json::json!({
                    "files_scanned":files.len(),"files_indexed":indexed,"files_current":current,
                    "files_unsupported":unsupported,"files_excluded":excluded,
                    "file_errors":errors.iter().take(20).collect::<Vec<_>>(),"error_count":errors.len(),
                    "corpus":indexer::corpus_stats(&store)?,"complete":errors.is_empty(),
                })
            );
            anyhow::ensure!(
                errors.is_empty(),
                "{} files failed indexing; inspect the reported paths",
                errors.len()
            );
        }
        Action::Status | Action::AddRoot { .. } => {
            unreachable!("handled before opening the embedder")
        }
    }
    Ok(())
}
