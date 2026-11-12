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

#[path = "resources.rs"]
mod resources;

const CONTROL: Duration = Duration::from_secs(2);
const RETIREMENT: Duration = Duration::from_secs(5);

pub(crate) fn validate_optional_mounts(
    app: &crate::apps::App, entry: Option<&str>, home: &std::path::Path, caps: &crate::caps::CapSet,
) -> Result<(), String> {
    resources::validate_optional_mounts(app, entry, home, caps)
}

struct Supervised {
    job: Arc<Job>,
    runtime: Runtime,
    broker: Broker,
    resources: Option<LaunchResources>,
    launch: Option<crate::worker::WorkerLaunch>,
    home: Option<resources::HomeMetadata>,
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
        admit_home_metadata(&job).map_err(|error| error.message)?;
        let consent = resources::consume(&job.registration, job.instance)?;
        let sandbox_caps = resources::sandbox_caps(&job.registration, consent.is_some())?;
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
        let mut plan = crate::bridge::gui::plan(
            &job.registration.launch,
            job.registration.gui_entry.as_deref(),
            &job.registration.optional_needs_ungranted,
            &job.registration.selector,
            request.args.as_slice(),
            &request.presentation,
            &job.registration.session_id,
            &sandbox_caps,
            job.instance,
            &runtime.socket(Socket::WaylandTransport),
        )?;
        plan.launch.policy.env.insert(
            "COS_OWNER_HOME".to_string(),
            job.registration.home.to_str()
                .ok_or("Authenticated GUI owner home is not UTF-8")?.to_string(),
        );
        resources::refuse_raw_home(&plan.launch.policy, &job.registration.home, consent.is_some())?;
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
            consent.is_some(),
        )?;
        let home = consent.map(|consent| resources::HomeMetadata::reserve(consent, &runtime)).transpose()?;
        Ok(Self {
            job,
            runtime,
            broker,
            resources: None,
            launch: Some(plan.launch),
            home,
            command: None,
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
            digest: "unprepared".into(),
        })
    }

    fn prepare_resources(&mut self) -> Result<(), String> {
        let mut launch = self.launch.take().ok_or("GUI sandbox plan is unavailable")?;
        if let Some(home) = &mut self.home {
            home.mount(&self.job.registration)?;
            home.attach(&mut launch.policy, &self.job.registration)?;
        }
        let prepared = LinuxSandbox.prepare_gui(
            &launch,
            GuiPreparation {
                workload: &self.job.registration.control.workload,
                instance: self.job.instance,
                runtime: &mut self.runtime,
                home_metadata: self.home.is_some(),
                retained: Some(&mut self.resources),
            },
        )?;
        self.resources = Some(prepared.resources);
        self.command = Some(prepared.command);
        self.digest = prepared.facts["policy"].as_str().ok_or("GUI policy has no audit digest")?.to_string();
        crate::worker::audit::launched(&prepared.facts, Some(&self.job.registration.session_id));
        Ok(())
    }

    fn start(&mut self, manager: &Manager, request: &LaunchRequest) -> Result<GrantView, String> {
        if self.job.stopping() {
            return Err("GUI launch was cancelled before spawn".to_string());
        }
        self.prepare_resources()?;
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
        if let Some(home) = &self.home {
            let view = self.job.registration.snapshot(&process)?;
            let selected = home.bind(&self.job.registration, self.job.instance, &process, resources, &view)?;
            self.job.selected_reads.set(selected).map_err(|_| "GUI selected read binding was already used")?;
            resources.release_gui_setup_gate()?;
        }
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
        let layer_shell = view.caps.covers(&Cap::new(Verb::UI_PANEL, Scope::Wild));
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
                    layer_shell,
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
        if let Some(home) = &self.home { home.refresh(&self.job.registration, &current)?; }
        input
            .write_all(Epoch(self.job.instance.0).label().as_bytes())
            .map_err(|error| format!("release exact GUI execution gate: {error}"))?;
        drop(input);
        self.job.running(layer_shell)?;
        crate::provenance::audit(
            "gui.instance.bound",
            serde_json::json!({
                "session_id": self.job.registration.session_id,
                "app_id": self.job.registration.launch.app_id(),
                "instance": Epoch(self.job.instance.0).label(),
                "grant": view.id.audit_ref(),
                "generation": view.generation,
                "layer_shell": layer_shell,
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
            if let Some(home) = &self.home { home.health()?; }
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
                if let Some(home) = &self.home { home.refresh(&self.job.registration, &view)?; }
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
        let home_stop_error = self.home.as_ref().and_then(|home| home.stop().err());
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
        if let Some(resources) = &mut self.resources {
            if self.home.is_some() { resources.release_gui_source_pins(deadline)?; }
        }
        if let Some(home) = &mut self.home { home.retire(deadline)?; }
        self.home.take();
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
            self.broker.retire_package_intents_after_death(resources.gui_cgroup()?, deadline)?;
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
        if let Some(error) = home_stop_error { return Err(error); }
        Ok(())
    }
}

impl Drop for Supervised {
    fn drop(&mut self) {
        if self.resources.is_some() || self.home.is_some() || (self.creator_attempted && !self.creator_retired) {
            if let Err(error) = self.retire() {
                tracing::error!(%error, "GUI emergency retirement could not be verified");
            }
        }
    }
}

pub(super) fn admit_home_metadata(job: &Job) -> Result<(), crate::clawd::protocol::BrokerError> {
    crate::paths::RoutedPathContext::for_owner(job.registration.owner(), job.registration.home.clone())
        .scope_sync(|| resources::admit(&job.registration, job.instance))
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
