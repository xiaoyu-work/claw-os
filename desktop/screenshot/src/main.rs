use ashpd::desktop::screenshot::Screenshot;
use clap::{ArgAction, Parser};
use std::{collections::HashMap, fs, os::unix::fs::MetadataExt, path::PathBuf};
use zbus::{proxy, zvariant::Value, Connection};

mod localize;
mod mcp;

#[derive(Parser, Default, Debug, Clone, PartialEq, Eq)]
#[command(version, about, long_about = None)]
struct Args {
    /// Enable interactive mode in the portal
    #[clap(long,
        default_missing_value("true"),
        default_value("true"),
        num_args(0..=1),
        require_equals(true),
        action = ArgAction::Set)]
    interactive: bool,
    /// Enable modal mode in the portal
    #[clap(long,
        default_missing_value("true"),
        default_value("true"),
        num_args(0..=1),
        require_equals(true),
        action = ArgAction::Set,)]
    modal: bool,
    /// Send a notification with the path to the saved screenshot
    #[clap(long,
        default_missing_value("true"),
        default_value("true"),
        num_args(0..=1),
        require_equals(true),
        action = ArgAction::Set)]
    notify: bool,
    /// The directory to save the screenshot to, if not performing an interactive screenshot
    #[clap(short, long)]
    save_dir: Option<PathBuf>,
}

#[proxy(assume_defaults = true)]
trait Notifications {
    /// Call the org.freedesktop.Notifications.Notify D-Bus method
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, &Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;
}

/// Options that drive a single screenshot capture, shared by CLI and
/// MCP entry points so both flows take the same code path.
#[derive(Debug, Clone)]
pub(crate) struct CaptureOptions {
    pub interactive: bool,
    pub modal: bool,
    pub save_dir: Option<PathBuf>,
}

/// Outcome of a successful capture. `path` is empty when the portal
/// chose to put the image on the clipboard instead of writing a file.
#[derive(Debug, Clone)]
pub(crate) struct CaptureOutcome {
    pub path: String,
    pub cancelled: bool,
}

#[derive(Debug)]
pub(crate) enum CaptureError {
    /// The portal returned an error other than "user cancelled".
    Portal(String),
    /// We couldn't move/rename the temp file to `save_dir`.
    Io(String),
    /// Anything else (URI scheme we don't model, etc.).
    Other(String),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::Portal(s) | CaptureError::Io(s) | CaptureError::Other(s) => {
                f.write_str(s)
            }
        }
    }
}

pub(crate) async fn capture(opts: CaptureOptions) -> Result<CaptureOutcome, CaptureError> {
    let picture_dir = if opts.interactive {
        None
    } else {
        let dir = opts
            .save_dir
            .clone()
            .or_else(dirs::picture_dir)
            .ok_or_else(|| CaptureError::Io("failed to locate picture directory".into()))?;
        if !dir.is_dir() {
            return Err(CaptureError::Io(format!(
                "screenshot destination is not an existing directory: {}",
                dir.display()
            )));
        }
        Some(dir)
    };

    let response = Screenshot::request()
        .interactive(opts.interactive)
        .modal(opts.modal)
        .send()
        .await
        .map_err(|e| CaptureError::Portal(format!("failed to send screenshot request: {e}")))?
        .response();

    let response = match response {
        Err(err) => {
            if err.to_string().contains("Cancelled") {
                return Ok(CaptureOutcome {
                    path: String::new(),
                    cancelled: true,
                });
            }
            return Err(CaptureError::Portal(format!(
                "error taking screenshot: {err}"
            )));
        }
        Ok(response) => response,
    };

    let uri = response.uri();
    let path = match uri.scheme() {
        "file" => {
            let response_path = uri
                .to_file_path()
                .map_err(|_| CaptureError::Other(format!("unsupported response URI '{uri}'")))?;
            if let Some(picture_dir) = picture_dir {
                let date = jiff::Zoned::now();
                let filename = format!("Screenshot_{}.png", date.strftime("%Y-%m-%d_%H-%M-%S"));
                let path = picture_dir.join(filename);
                let dst_meta = fs::metadata(&picture_dir)
                    .map_err(|e| CaptureError::Io(format!("stat dest: {e}")))?;
                let src_meta = fs::metadata(&response_path)
                    .map_err(|e| CaptureError::Io(format!("stat src: {e}")))?;
                let src = response_path
                    .to_str()
                    .ok_or_else(|| CaptureError::Io("source path not valid UTF-8".into()))?;
                let dst = path
                    .to_str()
                    .ok_or_else(|| CaptureError::Io("destination path not valid UTF-8".into()))?;
                if dst_meta.dev() != src_meta.dev() {
                    cos_runtime::fs::copy(src, dst)
                        .map_err(|e| CaptureError::Io(format!("copy: {e}")))?;
                    cos_runtime::fs::rm(src)
                        .map_err(|e| CaptureError::Io(format!("rm temp: {e}")))?;
                } else {
                    cos_runtime::fs::rename(src, dst)
                        .map_err(|e| CaptureError::Io(format!("rename: {e}")))?;
                }
                path.to_string_lossy().to_string()
            } else {
                response_path.to_string_lossy().to_string()
            }
        }
        "clipboard" => String::new(),
        scheme => {
            return Err(CaptureError::Other(format!(
                "unsupported scheme '{scheme}'"
            )));
        }
    };

    Ok(CaptureOutcome {
        path,
        cancelled: false,
    })
}

async fn send_notification(path: &str) {
    let connection = Connection::session()
        .await
        .expect("failed to connect to session bus");

    let message = if path.is_empty() {
        fl!("screenshot-saved-to-clipboard")
    } else {
        fl!("screenshot-saved-to")
    };
    let proxy = NotificationsProxy::new(&connection)
        .await
        .expect("failed to create proxy");
    _ = proxy
        .notify(
            &fl!("cosmic-screenshot"),
            0,
            "com.clawos.Screenshot",
            &message,
            path,
            &[],
            HashMap::from([("transient", &Value::Bool(true))]),
            5000,
        )
        .await
        .expect("failed to send notification");
}

//TODO: better error handling
#[tokio::main(flavor = "current_thread")]
async fn main() {
    crate::localize::localize();

    if std::env::var("COS_MCP_SERVER").as_deref() == Ok("1") {
        if let Err(e) = mcp::run().await {
            eprintln!("cosmic-screenshot MCP server exited: {e}");
            std::process::exit(1);
        }
        return;
    }

    let args = Args::parse();

    let outcome = match capture(CaptureOptions {
        interactive: args.interactive,
        modal: args.modal,
        save_dir: args.save_dir.clone(),
    })
    .await
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    if outcome.cancelled {
        println!("Screenshot cancelled by user");
        std::process::exit(0);
    }

    println!("{}", outcome.path);

    if args.notify {
        send_notification(&outcome.path).await;
    }
}
