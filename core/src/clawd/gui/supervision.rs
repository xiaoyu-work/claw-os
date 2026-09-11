use std::io::Write;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use claw_display_control::{
    wire::monotonic_ms, CompositorCommand, CompositorReply, Epoch, ProcessIdentity,
};

use crate::caps::{Cap, Scope, Verb};
use crate::clawd::authority::GrantView;
use crate::display_session::creator::{Creator, Lease};
use crate::display_session::runtime::{Runtime, Socket};
use crate::worker::{
    linux::{GuiPreparation, LinuxSandbox},
    LaunchResources,
};

use super::broker::Broker;
use super::inputs::LaunchRequest;
use super::manager::{Job, Manager, Outcome};
use super::output::{Captured, Output, Report};
use super::proxy::Proxy;
use super::transport::{Binding, Bootstrap, Guard, Targets};

const CONTROL: Duration = Duration::from_secs(2);
const RETIREMENT: Duration = Duration::from_secs(5);

struct Supervised {
    job: Arc<Job>,
    runtime: Runtime,
    broker: Broker,
    resources: Option<LaunchResources>,
    command: Option<Command>,
    child: Option<Child>,
    process: Option<ProcessIdentity>,
    status: Option<ExitStatus>,
    stdout: Option<Output>,
    stderr: Option<Output>,
    output: Option<(Captured, Captured)>,
    output_error: Option<String>,
    listener: Option<std::os::unix::net::UnixListener>,
    creator: Option<Creator>,
    creator_attempted: bool,
    creator_retired: bool,
    display_listener: Option<std::os::unix::net::UnixListener>,
    egress_listener: Option<std::os::unix::net::UnixListener>,
    display_proxy: Option<Proxy>,
    egress_proxy: Option<Proxy>,
    transport: Option<Guard>,
    binding: crate::bridge::LaunchBindingRef,
    digest: String,
}

impl Supervised {
    fn prepare(job: Arc<Job>, manager: &Manager, request: &LaunchRequest) -> Result<Self, String> {
        if job.stopping() {
            return Err("GUI launch was cancelled before preparation".to_string());
        }
        let mut runtime = Runtime::create(job.instance).map_err(|error| error.to_string())?;
        let listener = runtime
            .bind(Socket::Wayland, job.registration.owner())
            .map_err(|error| error.to_string())?;
        let broker_listener = runtime
            .bind(Socket::Broker, job.registration.owner())
            .map_err(|error| error.to_string())?;
        let display_listener = runtime
            .bind(Socket::WaylandTransport, job.registration.owner())
            .map_err(|error| error.to_string())?;
        let plan = crate::bridge::gui::plan(
            &job.registration.launch,
            &job.registration.selector,
            request.args.as_slice(),
            &request.presentation,
            &job.registration.session_id,
            &job.registration.caps,
            job.instance,
            &runtime.socket(Socket::WaylandTransport),
        )?;
        let egress_listener = if matches!(
            plan.launch.policy.network,
            crate::worker::NetworkPolicy::Brokered { .. }
        ) {
            Some(
                runtime
                    .bind(Socket::EgressTransport, job.registration.owner())
                    .map_err(|error| error.to_string())?,
            )
        } else {
            None
        };
        let broker = Broker::start(
            broker_listener,
            job.registration.clone(),
            manager.daemon.clone(),
            manager.admission.clone(),
            &manager.executor,
        )?;
        let prepared = LinuxSandbox.prepare_gui(
            &plan.launch,
            GuiPreparation {
                workload: &job.registration.control.workload,
                instance: job.instance,
                runtime: &mut runtime,
            },
        )?;
        let digest = prepared.facts["policy"]
            .as_str()
            .ok_or("GUI policy has no audit digest")?
            .to_string();
        crate::worker::audit::launched(&prepared.facts, Some(&job.registration.session_id));
        Ok(Self {
            job,
            runtime,
            broker,
            resources: Some(prepared.resources),
            command: Some(prepared.command),
            child: None,
            process: None,
            status: None,
            stdout: None,
            stderr: None,
            output: None,
            output_error: None,
            listener: Some(listener),
            creator: None,
            creator_attempted: false,
            creator_retired: false,
            display_listener: Some(display_listener),
            egress_listener,
            display_proxy: None,
            egress_proxy: None,
            transport: None,
            binding: plan.binding,
            digest,
        })
    }

    fn start(&mut self, manager: &Manager, request: &LaunchRequest) -> Result<GrantView, String> {
        if self.job.stopping() {
            return Err("GUI launch was cancelled before spawn".to_string());
        }
        let mut command = self
            .command
            .take()
            .ok_or("GUI command was already started")?;
        let (bootstrap, child_input) = Bootstrap::new()?;
        command
            .stdin(Stdio::from(child_input))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.child = Some(
            command
                .spawn()
                .map_err(|error| format!("spawn supervised GUI: {error}"))?,
        );
        let child = self.child.as_mut().ok_or("GUI child is unavailable")?;
        self.stdout = Some(Output::start(
            child
                .stdout
                .take()
                .ok_or("GUI stdout pipe is unavailable")?
                .into(),
        )?);
        self.stderr = Some(Output::start(
            child
                .stderr
                .take()
                .ok_or("GUI stderr pipe is unavailable")?
                .into(),
        )?);
        let resources = self
            .resources
            .as_ref()
            .ok_or("GUI containment is unavailable")?;
        let process = manager
            .executor
            .block_on(
                self.job
                    .registration
                    .bind_owned(request.handle.as_str(), child, resources),
            )
            .map_err(|error| format!("bind supervised GUI process: {error}"))?;
        self.process = Some(process.clone());
        self.broker
            .bind(process.clone(), resources)
            .map_err(|error| format!("bind GUI SDK provider: {error}"))?;
        let binding = Binding::new(&process, resources)
            .map_err(|error| format!("bind GUI transport process: {error}"))?;
        self.display_proxy = Some(Proxy::start(
            self.display_listener
                .take()
                .ok_or("GUI display transport listener is unavailable")?,
            self.runtime.socket(Socket::Wayland),
            binding.clone(),
            true,
        )?);
        if let Some(listener) = self.egress_listener.take() {
            self.egress_proxy = Some(Proxy::start(
                listener,
                self.runtime.socket(Socket::Egress),
                binding.clone(),
                false,
            )?);
        }
        let (guard, mut input) = bootstrap.accept(
            binding,
            Targets {
                wayland: self.runtime.socket(Socket::WaylandTransport),
                broker: self.runtime.socket(Socket::Broker),
                egress: self
                    .egress_proxy
                    .as_ref()
                    .map(|_| self.runtime.socket(Socket::EgressTransport)),
            },
            Instant::now() + CONTROL,
        )?;
        self.transport = Some(guard);
        let view = self.job.registration.snapshot(&process)?;
        let expires = lease_expiry(&view)?;
        if self.job.stopping() {
            return Err("GUI authority was retired before creator installation".to_string());
        }
        self.creator_attempted = true;
        self.creator = Some(
            Creator::install(
                &self.job.registration.control,
                Lease {
                    instance: self.job.instance,
                    authority: self.job.registration.authority_alias,
                    revision: view.generation,
                    expires_monotonic_ms: expires,
                    selection_read: view
                        .caps
                        .covers(&Cap::new(Verb::CLIPBOARD_READ, Scope::name("selection"))),
                    selection_write: view
                        .caps
                        .covers(&Cap::new(Verb::CLIPBOARD_WRITE, Scope::name("selection"))),
                    layer_shell: false,
                },
                process.pidfd().map_err(|error| error.to_string())?,
                resources.gui_cgroup()?,
                self.listener
                    .as_ref()
                    .ok_or("GUI listener is unavailable")?,
                self.job.registration.launch.app_id(),
                Instant::now() + CONTROL,
            )
            .map_err(|error| format!("install authenticated GUI creator: {error}"))?,
        );
        let current = self.job.registration.snapshot(&process)?;
        if current.id != view.id || current.generation != view.generation || self.job.stopping() {
            return Err("GUI authority changed before execution was released".to_string());
        }
        input
            .write_all(Epoch(self.job.instance.0).label().as_bytes())
            .map_err(|error| format!("release exact GUI execution gate: {error}"))?;
        drop(input);
        self.job.running()?;
        crate::provenance::audit(
            "gui.instance.bound",
            serde_json::json!({
                "session_id": self.job.registration.session_id,
                "app_id": self.job.registration.launch.app_id(),
                "instance": Epoch(self.job.instance.0).label(),
                "grant": view.id.audit_ref(),
                "generation": view.generation,
                "layer_shell": false,
            }),
        );
        Ok(view)
    }

    fn poll_exit(&mut self) -> Result<bool, String> {
        if self.status.is_none() {
            self.status = self
                .child
                .as_mut()
                .ok_or("GUI child is unavailable")?
                .try_wait()
                .map_err(|error| format!("observe GUI child: {error}"))?;
        }
        Ok(self.status.is_some())
    }

    fn watch(&mut self, initial: &GrantView) -> Result<(), String> {
        let mut refresh = Instant::now();
        loop {
            if self.poll_exit()? {
                return Ok(());
            }
            if self.job.stopping() {
                return Err("GUI instance authority was retired".to_string());
            }
            self.broker.health()?;
            self.transport
                .as_ref()
                .ok_or("GUI syscall guard is unavailable")?
                .health()?;
            self.display_proxy
                .as_ref()
                .ok_or("GUI display transport is unavailable")?
                .health()?;
            if let Some(proxy) = &self.egress_proxy {
                proxy.health()?;
            }
            if Instant::now() >= refresh {
                let view = self.job.registration.snapshot(
                    self.process
                        .as_ref()
                        .ok_or("GUI process binding is unavailable")?,
                )?;
                if view.id != initial.id
                    || view.generation != initial.generation
                    || view.caps != initial.caps
                {
                    return Err("GUI permissions changed".to_string());
                }
                let reply = self.job.registration.control.exchange(
                    CompositorCommand::Refresh {
                        epoch: self.job.registration.control.epoch,
                        instance: self.job.instance,
                        authority: self.job.registration.authority_alias,
                        revision: view.generation,
                        expires_monotonic_ms: lease_expiry(&view)?,
                    },
                    Vec::new(),
                    Instant::now() + CONTROL,
                );
                if self.poll_exit()? {
                    return Ok(());
                }
                let reply = reply.map_err(|error| error.to_string())?;
                if !matches!(reply, CompositorReply::Refreshed { epoch, instance }
                    if epoch == self.job.registration.control.epoch && instance == self.job.instance)
                {
                    return Err("compositor refused the live GUI authority renewal".to_string());
                }
                refresh = Instant::now() + Duration::from_millis(400);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn retire(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + RETIREMENT;
        self.broker.stop();
        if let Some(guard) = &self.transport {
            guard.stop();
        }
        for proxy in [&self.display_proxy, &self.egress_proxy]
            .into_iter()
            .flatten()
        {
            proxy.stop();
        }
        self.display_listener.take();
        self.egress_listener.take();
        let service_error = self
            .resources
            .as_ref()
            .and_then(|resources| resources.stop_gui_services().err());
        let mut control_error = None;
        if self.creator_attempted && !self.creator_retired {
            match self
                .job
                .registration
                .control
                .retire(self.job.instance, Instant::now() + CONTROL)
            {
                Ok(()) => self.creator_retired = true,
                Err(error) => control_error = Some(error.to_string()),
            }
        }
        self.creator.take();
        self.listener.take();
        if let Some(resources) = &self.resources {
            resources.retire_gui(deadline)?;
        }
        if let Some(guard) = &mut self.transport {
            guard.finish(deadline)?;
        }
        for proxy in [&mut self.display_proxy, &mut self.egress_proxy]
            .into_iter()
            .flatten()
        {
            proxy.finish(deadline)?;
        }
        if let Some(resources) = &mut self.resources {
            resources.retire_gui_services(deadline)?;
        }
        if let Some(child) = &mut self.child {
            if self.status.is_none() {
                loop {
                    if let Some(status) = child
                        .try_wait()
                        .map_err(|error| format!("reap GUI child: {error}"))?
                    {
                        self.status = Some(status);
                        break;
                    }
                    if Instant::now() >= deadline {
                        return Err(
                            "GUI child has not exited after checked cgroup retirement".to_string()
                        );
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
        if self.output.is_none() {
            let stdout = match &mut self.stdout {
                Some(output) => output.finish(deadline)?,
                None => Report {
                    captured: Captured {
                        text: String::new(),
                        truncated: false,
                    },
                    error: None,
                },
            };
            let stderr = match &mut self.stderr {
                Some(output) => output.finish(deadline)?,
                None => Report {
                    captured: Captured {
                        text: String::new(),
                        truncated: false,
                    },
                    error: None,
                },
            };
            self.output_error = match (stdout.error, stderr.error) {
                (Some(first), Some(second)) => Some(format!("{first}; {second}")),
                (Some(error), None) | (None, Some(error)) => Some(error),
                (None, None) => None,
            };
            self.output = Some((stdout.captured, stderr.captured));
        }
        while !self.broker.quiescent() {
            if Instant::now() >= deadline {
                return Err("GUI provider calls have not quiesced".to_string());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if let Some(resources) = &mut self.resources {
            resources.remove_gui_cgroup()?;
        }
        self.resources.take();
        self.runtime.remove().map_err(|error| error.to_string())?;
        if let Some(error) = control_error {
            return Err(error);
        }
        if let Some(error) = service_error {
            return Err(error);
        }
        Ok(())
    }
}

impl Drop for Supervised {
    fn drop(&mut self) {
        if self.resources.is_some() || (self.creator_attempted && !self.creator_retired) {
            if let Err(error) = self.retire() {
                tracing::error!(%error, "GUI emergency retirement could not be verified");
            }
        }
    }
}

fn lease_expiry(view: &GrantView) -> Result<u64, String> {
    let lifetime = view.expires_in.min(Duration::from_secs(2));
    if lifetime < Duration::from_millis(250) {
        return Err("GUI authority is expiring".to_string());
    }
    monotonic_ms()
        .map_err(|error| error.to_string())?
        .checked_add(lifetime.as_millis() as u64)
        .ok_or_else(|| "GUI lease clock overflow".to_string())
}

pub(super) fn run(job: Arc<Job>, manager: Arc<Manager>, request: LaunchRequest) {
    let mut supervised = match Supervised::prepare(job.clone(), &manager, &request) {
        Ok(supervised) => supervised,
        Err(error) => {
            job.registration.clear_after_retirement();
            job.complete(Outcome::failed(error));
            return;
        }
    };
    let started = supervised.start(&manager, &request);
    drop(request);
    let result = started.and_then(|view| supervised.watch(&view));
    if let Err(error) = &result {
        tracing::warn!(%error, "supervised GUI did not complete successfully");
    }
    job.retiring(None);
    loop {
        match supervised.retire() {
            Ok(()) => break,
            Err(error) => {
                tracing::error!(%error, "GUI retirement remains incomplete");
                job.retiring(Some(error));
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    }
    let (stdout, stderr) = match supervised.output.take() {
        Some(output) => output,
        None => {
            job.complete(Outcome::failed(
                "GUI retired without its output state".to_string(),
            ));
            return;
        }
    };
    let code = supervised.status.and_then(|status| status.code());
    let error = match (result.err(), supervised.output_error.take()) {
        (Some(first), Some(second)) => Some(format!("{first}; {second}")),
        (Some(error), None) | (None, Some(error)) => Some(error),
        (None, None) => None,
    };
    crate::worker::audit::outcome(
        &supervised.digest,
        &format!(
            "app:{}/{}",
            job.registration.launch.app_id(),
            job.registration.selector
        ),
        serde_json::json!({ "exit_code": code, "retired": true, "failed": error.is_some() }),
    );
    job.registration.clear_after_retirement();
    job.complete(Outcome {
        exit_code: code,
        error,
        stdout,
        stderr,
    });
}
