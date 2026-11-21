//! One disposable Helium process per render. Only clawd can use its CDP pipe
//! or reach the network; Helium retains its own renderer sandbox.

use std::io;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use url::Url;

use crate::caps::{Cap, Scope, Verb};
use crate::clawd::authority::Decision;
use crate::clawd::protocol::BrokerError;
use crate::clawd::state::DaemonState;
use crate::clawd::wire::requests::{BrowserRender, BrowserRenderOutput};
use crate::extension_host::spawn::{attach_current_process, ResourceGroup};
use crate::worker::net_broker::is_globally_routable;

const HELIUM: &str = "/opt/helium/helium";
const BWRAP: &str = "/usr/bin/bwrap";
const MAX_FRAME: usize = 8 * 1024 * 1024;
const MAX_PNG: usize = 4 * 1024 * 1024;
const MAX_RESOURCE: usize = 4 * 1024 * 1024;
const MAX_DOCUMENT_BYTES: usize = 24 * 1024 * 1024;
const MAX_REQUESTS: usize = 96;
const MAX_RESULT: usize = 256 * 1024;
const START: &str = r#"
import os
os.dup2(0, 3)
os.dup2(1, 4)
null = os.open("/dev/null", os.O_RDWR | os.O_CLOEXEC)
os.dup2(null, 0)
os.dup2(null, 1)
binary = "/opt/helium/helium"
os.execv(binary, [
    binary, "--headless=new", "--remote-debugging-pipe",
    "--user-data-dir=/tmp/helium", "--no-first-run",
    "--no-default-browser-check", "--disable-background-networking",
    "--disable-gpu", "about:blank",
])
"#;

pub(super) async fn render(
    params: Value,
    authority: &Decision,
    state: &DaemonState,
) -> Result<Value, BrokerError> {
    let request: BrowserRender = serde_json::from_value(params).map_err(|error| {
        BrokerError::execution(format!("invalid browser render request: {error}"))
    })?;
    let (canonical, scope) =
        super::browser_url(request.url.as_str()).map_err(BrokerError::execution)?;
    let url = Url::parse(&canonical).map_err(|_| BrokerError::execution("invalid render URL"))?;
    let timeout = request.timeout_secs.unwrap_or(30);
    if !(1..=55).contains(&timeout) {
        return Err(BrokerError::execution("invalid browser render deadline"));
    }
    if let BrowserRenderOutput::Screenshot { width, height, .. } = &request.output {
        if !(1..=3840).contains(width) || !(1..=2160).contains(height) {
            return Err(BrokerError::execution(
                "screenshot dimensions exceed 3840 by 2160",
            ));
        }
    }
    if authority.owner_uid() == 0 || unsafe { libc::geteuid() } != 0 {
        return Err(BrokerError::authorization(
            "headless rendering requires a non-root session on clawd",
        ));
    }
    let _authorized = authority
        .require(Cap::new(Verb::NET_DIAL, Scope::host(scope)))
        .map_err(BrokerError::authorization)?;

    tokio::time::timeout(Duration::from_secs(timeout.into()), async {
        let network = Network::new(
            url.clone(),
            authority,
            request.user_agent.as_ref().map(|value| value.as_str()),
        )
        .await?;
        let group = state
            .app_service_manager()
            .ok_or("browser process containment is unavailable")?
            .browser_render_group()?;
        let browser = Cdp::start(authority.owner_uid(), group)?;
        browser.render(&request, &url, network).await
    })
    .await
    .map_err(|_| BrokerError::execution("Helium render timed out"))?
    .map_err(BrokerError::execution)
}

fn start_browser(owner: u32, group: &ResourceGroup) -> Result<Child, String> {
    use std::os::unix::process::CommandExt;

    for path in [HELIUM, BWRAP, "/usr/bin/python3"] {
        let path = Path::new(path);
        let canonical = path.canonicalize().map_err(|_| {
            "Helium renderer prerequisites are missing; install helium-bin and bubblewrap"
                .to_string()
        })?;
        let metadata = crate::provenance::fsec::require_secure_location(&canonical, &[0])
            .map_err(|error| format!("untrusted browser renderer prerequisite: {error}"))?;
        if !metadata.is_file || metadata.mode & 0o111 == 0 {
            return Err("browser renderer prerequisite is not an executable file".into());
        }
    }
    let identity = crate::agentd::spawn::resolve_identity(owner)?;
    let mut command = std::process::Command::new(BWRAP);
    let cgroup = group.procs_fd();
    command
        .args([
            "--unshare-user",
            "--unshare-pid",
            "--unshare-ipc",
            "--unshare-uts",
            "--unshare-net",
            "--die-with-parent",
            "--new-session",
            "--clearenv",
            "--ro-bind",
            "/usr",
            "/usr",
            "--ro-bind",
            "/lib",
            "/lib",
            "--ro-bind-try",
            "/lib64",
            "/lib64",
            "--ro-bind",
            "/opt/helium",
            "/opt/helium",
            "--ro-bind-try",
            "/etc/fonts",
            "/etc/fonts",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--dir",
            "/home/browser",
            "--chdir",
            "/home/browser",
            "--setenv",
            "HOME",
            "/home/browser",
            "--setenv",
            "PATH",
            "/usr/bin",
            "--",
            "/usr/bin/python3",
            "-I",
            "-S",
            "-c",
            START,
        ])
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(move || {
            attach_current_process(cgroup)?;
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setresgid(identity.gid, identity.gid, identity.gid) != 0
                || libc::setresuid(owner, owner, owner) != 0
                || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Command::from(command)
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("cannot start isolated Helium: {error}"))
}

struct Network<'a> {
    origin: url::Origin,
    client: reqwest::Client,
    authority: &'a Decision,
    requests: usize,
    remaining: usize,
}

impl<'a> Network<'a> {
    async fn new(url: Url, authority: &'a Decision, agent: Option<&str>) -> Result<Self, String> {
        let host = url
            .host_str()
            .ok_or("browser URL has no host")?
            .trim_matches(['[', ']']);
        let port = url
            .port_or_known_default()
            .ok_or("browser URL has no port")?;
        let addresses = tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| "browser host did not resolve")?
            .take(65)
            .collect::<Vec<_>>();
        if addresses.is_empty()
            || addresses.len() > 64
            || addresses
                .iter()
                .any(|address| !is_globally_routable(address.ip()))
        {
            return Err("browser host did not resolve only to permitted public addresses".into());
        }
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(host, &addresses);
        if let Some(agent) = agent {
            builder = builder.user_agent(agent);
        }
        Ok(Self {
            origin: url.origin(),
            client: builder
                .build()
                .map_err(|_| "invalid browser HTTP settings")?,
            authority,
            requests: MAX_REQUESTS,
            remaining: MAX_DOCUMENT_BYTES,
        })
    }

    async fn resource(&mut self, request: &Value) -> Result<Value, String> {
        let (url, method) = read_request(request, &self.origin)?;
        self.requests = self
            .requests
            .checked_sub(1)
            .ok_or("browser request limit exceeded")?;
        self.authority.ensure_current()?;
        let mut response = self
            .client
            .request(method, url)
            .send()
            .await
            .map_err(|_| "browser HTTP request failed")?;
        let code = response.status().as_u16();
        let mut headers = Vec::new();
        let mut header_bytes = 0;
        for (name, value) in response.headers() {
            if matches!(
                name.as_str(),
                "connection" | "transfer-encoding" | "content-length" | "set-cookie"
            ) {
                continue;
            }
            if let Ok(value) = value.to_str() {
                header_bytes += name.as_str().len() + value.len();
                if header_bytes > 32 * 1024 {
                    return Err("browser response headers exceed their bound".into());
                }
                headers.push(json!({"name": name.as_str(), "value": value}));
            }
        }
        let limit = MAX_RESOURCE.min(self.remaining);
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "browser HTTP body failed")?
        {
            if chunk.len() > limit.saturating_sub(bytes.len()) {
                return Err("browser response exceeds its byte budget".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        self.remaining -= bytes.len();
        self.authority.ensure_current()?;
        Ok(
            json!({"responseCode": code, "responseHeaders": headers, "body": STANDARD.encode(bytes)}),
        )
    }
}

pub(super) fn read_request(
    request: &Value,
    origin: &url::Origin,
) -> Result<(Url, reqwest::Method), String> {
    let url = Url::parse(field(request, "url")?).map_err(|_| "invalid resource URL")?;
    if &url.origin() != origin || !url.username().is_empty() || url.password().is_some() {
        return Err("browser resource is outside the authorized origin".into());
    }
    let method = match field(request, "method")? {
        "GET" => reqwest::Method::GET,
        "HEAD" => reqwest::Method::HEAD,
        _ => return Err("browser rendering accepts read-only HTTP requests".into()),
    };
    Ok((url, method))
}

struct Cdp {
    _child: Child,
    _group: ResourceGroup,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    sequence: u32,
    session: Option<String>,
}

impl Cdp {
    fn start(owner: u32, group: ResourceGroup) -> Result<Self, String> {
        let mut child = start_browser(owner, &group)?;
        Ok(Self {
            input: child.stdin.take().ok_or("missing Helium request pipe")?,
            output: BufReader::new(child.stdout.take().ok_or("missing Helium response pipe")?),
            _child: child,
            _group: group,
            sequence: 0,
            session: None,
        })
    }

    async fn send(&mut self, method: &str, params: Value) -> Result<u32, String> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("Helium request limit exceeded")?;
        let mut frame = json!({"id": self.sequence, "method": method, "params": params});
        if let Some(session) = &self.session {
            frame["sessionId"] = json!(session);
        }
        let mut data = serde_json::to_vec(&frame).map_err(|error| error.to_string())?;
        data.push(0);
        self.input.write_all(&data).await.map_err(pipe_failure)?;
        Ok(self.sequence)
    }

    async fn call(
        &mut self,
        method: &str,
        params: Value,
        net: &mut Network<'_>,
    ) -> Result<Value, String> {
        let id = self.send(method, params).await?;
        loop {
            let mut frame = Vec::new();
            (&mut self.output)
                .take((MAX_FRAME + 1) as u64)
                .read_until(0, &mut frame)
                .await
                .map_err(pipe_failure)?;
            if frame.len() > MAX_FRAME || frame.pop() != Some(0) {
                return Err("Helium closed its CDP pipe or returned an oversized frame".into());
            }
            let event: Value =
                serde_json::from_slice(&frame).map_err(|_| "invalid Helium CDP frame")?;
            if event["id"].as_u64() == Some(id.into()) {
                if !event["error"].is_null() {
                    return Err(format!("Helium refused {method}"));
                }
                return Ok(event["result"].clone());
            }
            if self.session.as_deref() == event["sessionId"].as_str()
                && event["method"].as_str() == Some("Fetch.requestPaused")
            {
                let paused = &event["params"];
                let request = field(paused, "requestId")?;
                match net.resource(&paused["request"]).await {
                    Ok(mut result) => {
                        result["requestId"] = json!(request);
                        self.send("Fetch.fulfillRequest", result).await?;
                    }
                    Err(_) => {
                        self.send(
                            "Fetch.failRequest",
                            json!({
                                "requestId": request, "errorReason": "BlockedByClient",
                            }),
                        )
                        .await?;
                    }
                }
            }
        }
    }

    async fn evaluate(&mut self, expression: &str, net: &mut Network<'_>) -> Result<Value, String> {
        let result = self
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": expression, "returnByValue": true, "awaitPromise": true,
                }),
                net,
            )
            .await?;
        if result.get("exceptionDetails").is_some() {
            return Err("the page expression failed".into());
        }
        Ok(result["result"]["value"].clone())
    }

    async fn render(
        mut self,
        request: &BrowserRender,
        url: &Url,
        mut net: Network<'_>,
    ) -> Result<Value, String> {
        let target = self
            .call(
                "Target.createTarget",
                json!({"url": "about:blank"}),
                &mut net,
            )
            .await?;
        let session = self
            .call(
                "Target.attachToTarget",
                json!({
                    "targetId": field(&target, "targetId")?, "flatten": true,
                }),
                &mut net,
            )
            .await?;
        self.session = Some(field(&session, "sessionId")?.into());
        self.call("Fetch.enable", json!({}), &mut net).await?;
        if let BrowserRenderOutput::Screenshot { width, height, .. } = &request.output {
            self.call(
                "Emulation.setDeviceMetricsOverride",
                json!({
                    "width": width, "height": height, "deviceScaleFactor": 1, "mobile": false,
                }),
                &mut net,
            )
            .await?;
        }
        let navigation = self
            .call("Page.navigate", json!({"url": url.as_str()}), &mut net)
            .await?;
        if navigation.get("errorText").is_some() {
            return Err("Helium could not navigate within the authorized origin".into());
        }
        let title: String = loop {
            let page = self
                .evaluate(
                    "[document.readyState, location.href, document.title]",
                    &mut net,
                )
                .await?;
            if page[0] == "complete" && page[1] != "about:blank" {
                let current = Url::parse(page[1].as_str().ok_or("Helium omitted its final URL")?)
                    .map_err(|_| "Helium returned an invalid final URL")?;
                if current.origin() != net.origin {
                    return Err("Helium did not finish in the authorized origin".into());
                }
                break page[2].as_str().unwrap_or("").chars().take(512).collect();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        if let Some(selector) = &request.selector {
            let expression = format!("Boolean(document.querySelector({}))", json!(selector));
            let deadline = tokio::time::Instant::now()
                + Duration::from_secs(
                    request
                        .selector_wait_secs
                        .unwrap_or(5)
                        .min(request.timeout_secs.unwrap_or(30))
                        .into(),
                );
            while self.evaluate(&expression, &mut net).await? != true {
                if tokio::time::Instant::now() >= deadline {
                    return Err("the requested selector did not appear".into());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        let result = match &request.output {
            BrowserRenderOutput::Eval { expression } => {
                self.evaluate(expression.as_str(), &mut net).await?
            }
            BrowserRenderOutput::Html => {
                self.evaluate(
                    "'<!doctype html>\\n' + document.documentElement.outerHTML",
                    &mut net,
                )
                .await?
            }
            BrowserRenderOutput::Screenshot {
                width,
                height,
                full_page,
            } => {
                let mut options = json!({"format": "png", "captureBeyondViewport": full_page});
                if *full_page {
                    let metrics = self
                        .call("Page.getLayoutMetrics", json!({}), &mut net)
                        .await?;
                    let content = &metrics["cssContentSize"];
                    let width = content["width"]
                        .as_f64()
                        .unwrap_or(f64::from(*width))
                        .ceil();
                    let height = content["height"]
                        .as_f64()
                        .unwrap_or(f64::from(*height))
                        .ceil();
                    if !(1.0..=3840.0).contains(&width) || !(1.0..=8192.0).contains(&height) {
                        return Err("full-page screenshot exceeds its dimension limit".into());
                    }
                    options["clip"] =
                        json!({"x": 0, "y": 0, "width": width, "height": height, "scale": 1});
                }
                let capture = self
                    .call("Page.captureScreenshot", options, &mut net)
                    .await?;
                let data = field(&capture, "data")?;
                if data.len() > MAX_PNG.div_ceil(3) * 4 {
                    return Err("Helium screenshot exceeds its byte budget".into());
                }
                net.authority.ensure_current()?;
                return Ok(json!({"data": data}));
            }
        };
        if serde_json::to_vec(&result)
            .map_err(|error| error.to_string())?
            .len()
            > MAX_RESULT
        {
            return Err("Helium page output exceeds its byte budget".into());
        }
        net.authority.ensure_current()?;
        Ok(json!({"title": title, "result": result}))
    }
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value[key]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Helium response omitted {key}"))
}

fn pipe_failure(error: io::Error) -> String {
    format!("isolated Helium pipe: {error}")
}
