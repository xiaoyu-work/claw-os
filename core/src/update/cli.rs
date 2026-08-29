//! `claw-security-floor` — the helper every enforcement point calls.
//!
//! Maintainer scripts, the APT pre-install hook and operators all go
//! through this one binary instead of re-deriving version comparisons
//! in shell. That matters for correctness (`dpkg` version ordering is
//! not string ordering) and for review: there is a single place where
//! "may this release be installed?" is answered.
//!
//! Exit codes are part of the contract:
//!
//! | Code | Meaning |
//! | --- | --- |
//! | 0 | allowed |
//! | 10 | refused by policy — the caller must abort |
//! | 2 | usage error |
//! | 1 | internal error — treated as a refusal by every caller |
//!
//! `--root` operates on an alternate filesystem root. It is `dpkg`'s
//! own `DPKG_ROOT` convention, is always reported in the output, and
//! is never consulted by the runtime gates, which use the compiled-in
//! system path only.

use std::collections::BTreeMap;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::json;

use super::decide::{self, Candidate, Decision, Operation};
use super::floor::{ComponentFloor, Floor, FloorState, FloorStore};
use super::manifest::{require_digest, Manifest};
use super::recovery::{self, Authorization, RecoveryStore};
use super::signature::{self, Signature};

pub const EXIT_ALLOWED: i32 = 0;
pub const EXIT_INTERNAL: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_REFUSED: i32 = 10;

/// Entry point for the `claw-security-floor` binary.
pub fn main(args: &[String]) -> i32 {
    match run(args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("claw-security-floor: {error}");
            EXIT_INTERNAL
        }
    }
}

fn run(args: &[String]) -> Result<i32, String> {
    let Some(command) = args.first().map(String::as_str) else {
        print_help();
        return Ok(EXIT_USAGE);
    };
    let rest = &args[1..];
    match command {
        "-h" | "--help" | "help" => {
            print_help();
            Ok(EXIT_ALLOWED)
        }
        "policy" => policy_command(),
        "show" => show_command(rest),
        "check-candidate" => check_candidate_command(rest),
        "check-incoming" => check_incoming_command(rest),
        "commit" => commit_command(rest),
        "project" => project_command(rest),
        "runtime-check" => runtime_check_command(rest),
        "verify-installed" => verify_installed_command(rest),
        "service-gate" => service_gate_command(rest),
        "apt-hook" => apt_hook_command(rest),
        "recover" => recover_command(rest),
        other => {
            eprintln!("claw-security-floor: unknown command `{other}`");
            print_help();
            Ok(EXIT_USAGE)
        }
    }
}

fn print_help() {
    println!(
        "\
claw-security-floor — Claw OS update downgrade protection

Usage:
  claw-security-floor policy
  claw-security-floor show [--root DIR]
  claw-security-floor check-candidate --package NAME --version VERSION
                                      --manifest FILE [--signature FILE]
                                      [--operation install|upgrade|configure|plan]
                                      [--installed NAME=VERSION]... [--root DIR]
  claw-security-floor check-incoming --package NAME --version VERSION [--root DIR]
  claw-security-floor commit --package NAME --version VERSION --manifest FILE
                             [--signature FILE] [--installed NAME=VERSION]...
                             [--reason TEXT] [--root DIR]
  claw-security-floor project [--root DIR]
  claw-security-floor runtime-check [--scope critical|epoch] [--root DIR]
  claw-security-floor verify-installed [--scope critical|epoch] [--root DIR]
  claw-security-floor service-gate --package NAME --manifest FILE
                                   [--installed NAME=VERSION]... [--root DIR]
  claw-security-floor apt-hook [--root DIR]
  claw-security-floor recover authorize --package NAME --version VERSION
                                        --epoch N --manifest-sha256 SHA256
                                        --reason TEXT --expires-in HOURS [--root DIR]
  claw-security-floor recover list [--root DIR]
  claw-security-floor recover revoke --id ID [--root DIR]

Exit codes: 0 allowed, 10 refused, 2 usage, 1 internal error.
"
    );
}

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Options {
    package: Option<String>,
    version: Option<String>,
    manifest: Option<PathBuf>,
    signature: Option<PathBuf>,
    root: PathBuf,
    installed: BTreeMap<String, String>,
    installed_given: bool,
    operation: Option<String>,
    reason: Option<String>,
    scope: Option<String>,
    epoch: Option<u64>,
    manifest_sha256: Option<String>,
    expires_in: Option<i64>,
    id: Option<String>,
    json: bool,
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        root: PathBuf::from("/"),
        ..Options::default()
    };
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].clone();
        let take_value = |index: &mut usize| -> Result<String, String> {
            *index += 1;
            args.get(*index)
                .cloned()
                .ok_or_else(|| format!("`{flag}` requires a value"))
        };
        match args[index].as_str() {
            "--package" => options.package = Some(take_value(&mut index)?),
            "--version" => options.version = Some(take_value(&mut index)?),
            "--manifest" => options.manifest = Some(PathBuf::from(take_value(&mut index)?)),
            "--signature" => options.signature = Some(PathBuf::from(take_value(&mut index)?)),
            "--reason" => options.reason = Some(take_value(&mut index)?),
            "--operation" => options.operation = Some(take_value(&mut index)?),
            "--scope" => options.scope = Some(take_value(&mut index)?),
            "--id" => options.id = Some(take_value(&mut index)?),
            "--manifest-sha256" => options.manifest_sha256 = Some(take_value(&mut index)?),
            "--epoch" => {
                let raw = take_value(&mut index)?;
                options.epoch = Some(
                    raw.parse::<u64>()
                        .map_err(|_| format!("`{raw}` is not a security epoch"))?,
                );
            }
            "--expires-in" => {
                let raw = take_value(&mut index)?;
                options.expires_in = Some(
                    raw.parse::<i64>()
                        .map_err(|_| format!("`{raw}` is not a number of hours"))?,
                );
            }
            "--root" => {
                let raw = take_value(&mut index)?;
                if raw.is_empty() {
                    return Err("`--root` requires a directory".to_string());
                }
                options.root = PathBuf::from(raw);
            }
            "--installed" => {
                let raw = take_value(&mut index)?;
                let (name, version) = raw
                    .split_once('=')
                    .ok_or_else(|| format!("`--installed {raw}` must be NAME=VERSION"))?;
                options.installed_given = true;
                if !version.is_empty() {
                    options
                        .installed
                        .insert(name.to_string(), version.to_string());
                }
            }
            "--json" => options.json = true,
            other => return Err(format!("unknown option `{other}`")),
        }
        index += 1;
    }
    Ok(options)
}

fn require<'a>(value: &'a Option<String>, flag: &str) -> Result<&'a str, String> {
    value
        .as_deref()
        .ok_or_else(|| format!("`{flag}` is required"))
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn policy_command() -> Result<i32, String> {
    let document = json!({
        "abi": super::ABI,
        "components": super::COMPONENTS
            .iter()
            .map(|component| json!({
                "critical": component.critical,
                "name": component.name,
                "package": component.package,
                "path": component.path,
            }))
            .collect::<Vec<_>>(),
        "protocols": super::compiled_protocols(),
        "security_epoch": super::SECURITY_EPOCH,
    });
    println!("{}", super::canonical::to_string(&document)?);
    Ok(EXIT_ALLOWED)
}

fn show_command(args: &[String]) -> Result<i32, String> {
    let options = parse_options(args)?;
    let store = store_for(&options);
    match store.load() {
        Ok(FloorState::Uninitialized) => {
            println!(
                "{}",
                super::canonical::to_string(&json!({"state": "uninitialized"}))?
            );
            Ok(EXIT_ALLOWED)
        }
        Ok(FloorState::Present {
            floor,
            history_repair_needed,
        }) => {
            let document = json!({
                "abi": floor.abi,
                "generation": floor.generation,
                "history_repair_needed": history_repair_needed,
                "packages": floor
                    .packages
                    .iter()
                    .map(|(name, entry)| (name.clone(), json!({
                        "abi": entry.abi,
                        "manifest_sha256": entry.manifest_sha256,
                        "security_epoch": entry.security_epoch,
                        "version": entry.version,
                    })))
                    .collect::<serde_json::Map<_, _>>(),
                "security_epoch": floor.security_epoch,
                "state": "present",
                "suite": floor.suite,
                "trusted_keys": floor.trusted_keys.iter().cloned().collect::<Vec<_>>(),
            });
            println!("{}", super::canonical::to_string(&document)?);
            Ok(EXIT_ALLOWED)
        }
        Err(error) => {
            eprintln!("claw-security-floor: {error}");
            Ok(EXIT_REFUSED)
        }
    }
}

fn check_candidate_command(args: &[String]) -> Result<i32, String> {
    let options = parse_options(args)?;
    let package = require(&options.package, "--package")?.to_string();
    let version = require(&options.version, "--version")?.to_string();
    let operation = match options.operation.as_deref() {
        None | Some("install") => Operation::Install,
        Some("upgrade") => Operation::Upgrade,
        Some("configure") => Operation::Configure,
        Some("plan") => Operation::Plan,
        Some(other) => return Err(format!("unknown operation `{other}`")),
    };
    let (candidate, store) = load_candidate(&options, package, version, operation)?;
    let state = load_state(&store)?;
    let recovery = RecoveryStore::new(&store);
    let decision = decide::evaluate(&candidate, &state, Some(&recovery), Utc::now());
    report(
        &options,
        "check",
        &candidate.package,
        &candidate.version,
        &decision,
    );
    Ok(exit_for(&decision))
}

fn check_incoming_command(args: &[String]) -> Result<i32, String> {
    let options = parse_options(args)?;
    let package = require(&options.package, "--package")?;
    let version = require(&options.version, "--version")?;
    let store = store_for(&options);
    let state = load_state(&store)?;
    let recovery = RecoveryStore::new(&store);
    let decision =
        decide::evaluate_incoming_version(package, version, &state, Some(&recovery), Utc::now());
    report(&options, "pre-unpack", package, version, &decision);
    Ok(exit_for(&decision))
}

fn commit_command(args: &[String]) -> Result<i32, String> {
    let options = parse_options(args)?;
    let package = require(&options.package, "--package")?.to_string();
    let version = require(&options.version, "--version")?.to_string();
    let (candidate, store) = load_candidate(&options, package, version, Operation::Configure)?;
    let state = load_state(&store)?;
    let recovery = RecoveryStore::new(&store);
    let decision = decide::evaluate(&candidate, &state, Some(&recovery), Utc::now());
    if !decision.allowed {
        report(
            &options,
            "commit",
            &candidate.package,
            &candidate.version,
            &decision,
        );
        return Ok(EXIT_REFUSED);
    }

    // The floor is only advanced once the files this release claims to
    // have installed are actually on disk with the content the signed
    // manifest names. A partial or failed unpack therefore cannot
    // record a success.
    let measured = measure_declared_components(&options.root, &candidate.manifest)?;

    let next = match &state {
        FloorState::Uninitialized => Floor::bootstrap(
            &candidate.manifest,
            candidate
                .signature
                .key_id()
                .map(|key| std::iter::once(key.to_string()).collect())
                .unwrap_or_default(),
            measured,
            Utc::now(),
        ),
        FloorState::Present {
            floor,
            history_repair_needed,
        } => {
            if *history_repair_needed {
                store
                    .repair_history(floor, "repaired after an interrupted commit")
                    .map_err(|error| error.to_string())?;
            }
            floor
                .advanced(
                    &candidate.manifest,
                    candidate.signature.key_id(),
                    measured,
                    Utc::now(),
                    if decision.class == decide::class::ALLOWED_RECOVERY {
                        super::floor::Advance::AuthorizedRecovery
                    } else {
                        super::floor::Advance::Forward
                    },
                )
                .map_err(|error| error.to_string())?
        }
    };
    store
        .commit(
            &next,
            options.reason.as_deref().unwrap_or("package configure"),
        )
        .map_err(|error| error.to_string())?;

    // Only now, with the authority already durable, publish the
    // unprivileged view. A failure here is INDETERMINATE, never a
    // success: the private floor has moved forward monotonically but
    // the machine's runtime view has not, so the caller must retry or
    // repair rather than continue.
    let projection = projection_for(&options);
    if let Err(error) = projection.publish(&next) {
        mark_projection_pending(&store);
        report(
            &options,
            "commit",
            &candidate.package,
            &candidate.version,
            &Decision {
                allowed: false,
                class: decide::class::FLOOR_UNAVAILABLE,
                message: error.to_string(),
                recovery: None,
                signature_verified: decision.signature_verified,
            },
        );
        return Err(error.to_string());
    }
    clear_projection_pending(&store);

    if let Some(authorization) = &decision.recovery {
        consume_recovery(&recovery, authorization)?;
    }
    report(
        &options,
        "commit",
        &candidate.package,
        &candidate.version,
        &decision,
    );
    Ok(EXIT_ALLOWED)
}

/// Republish the unprivileged runtime view from the authoritative
/// floor. Root-only in practice, and idempotent.
fn project_command(args: &[String]) -> Result<i32, String> {
    let options = parse_options(args)?;
    let store = store_for(&options);
    let state = load_state(&store)?;
    let FloorState::Present { floor, .. } = &state else {
        println!("no authoritative security floor to project");
        return Ok(EXIT_ALLOWED);
    };
    let projection = projection_for(&options);
    projection
        .publish(floor)
        .map_err(|error| error.to_string())?;
    clear_projection_pending(&store);
    println!(
        "published runtime security floor generation {} to {}",
        floor.generation,
        projection.path().display()
    );
    Ok(EXIT_ALLOWED)
}

/// What an unprivileged Claw OS binary sees, and nothing else.
///
/// This is the exact code path `cos`, `claw-agentd`, the approval
/// helper and the App runner take at startup, so it never repairs
/// anything: an operator running it is asking what those processes
/// would decide.
fn runtime_check_command(args: &[String]) -> Result<i32, String> {
    let options = parse_options(args)?;
    let scope = match options.scope.as_deref() {
        None | Some("epoch") => super::runtime::Scope::CompiledEpoch,
        Some("critical") => super::runtime::Scope::CriticalComponents,
        Some(other) => {
            return Err(format!(
                "unknown scope `{other}`; expected `critical` or `epoch`"
            ))
        }
    };
    let projection = projection_for(&options);
    match super::runtime::enforce_startup_in(&projection, &options.root, scope) {
        Ok(()) => {
            println!("runtime security floor satisfied");
            Ok(EXIT_ALLOWED)
        }
        Err(refusal) => {
            eprintln!("claw-security-floor: {}", refusal.message);
            Ok(EXIT_REFUSED)
        }
    }
}

fn verify_installed_command(args: &[String]) -> Result<i32, String> {
    let options = parse_options(args)?;
    let scope = match options.scope.as_deref() {
        None | Some("critical") => super::runtime::Scope::CriticalComponents,
        Some("epoch") => super::runtime::Scope::CompiledEpoch,
        Some(other) => {
            return Err(format!(
                "unknown scope `{other}`; expected `critical` or `epoch`"
            ))
        }
    };
    let store = store_for(&options);
    let projection = projection_for(&options);
    // Privileged view first: it is the authority, and it repairs the
    // unprivileged projection when they disagree.
    if let Err(refusal) =
        super::runtime::enforce_broker_startup_in(&store, &projection, &options.root)
    {
        eprintln!("claw-security-floor: {}", refusal.message);
        return Ok(EXIT_REFUSED);
    }
    match super::runtime::enforce_startup_in(&projection, &options.root, scope) {
        Ok(()) => {
            println!("security floor satisfied");
            Ok(EXIT_ALLOWED)
        }
        Err(refusal) => {
            eprintln!("claw-security-floor: {}", refusal.message);
            Ok(EXIT_REFUSED)
        }
    }
}

fn service_gate_command(args: &[String]) -> Result<i32, String> {
    let options = parse_options(args)?;
    let package = require(&options.package, "--package")?;
    let manifest_path = options
        .manifest
        .as_ref()
        .ok_or_else(|| "`--manifest` is required".to_string())?;
    let manifest = read_manifest(manifest_path)?;
    if manifest.package != package {
        return Err(format!(
            "release manifest describes `{}`, not `{package}`",
            manifest.package
        ));
    }
    let installed = installed_versions(&options)?;
    match decide::installed_set_is_compatible(&manifest, &installed) {
        Ok(()) => Ok(EXIT_ALLOWED),
        Err(reason) => {
            eprintln!("claw-security-floor: {reason}");
            Ok(EXIT_REFUSED)
        }
    }
}

/// Validate a complete APT transaction before a single file is
/// unpacked.
///
/// This is the one check that sees the *whole* candidate set, so it
/// catches the case no maintainer script can: `apt install
/// claw-os-agent=<old>` where the old package's own scripts predate
/// this protection, and a reinstall after the package was removed.
fn apt_hook_command(args: &[String]) -> Result<i32, String> {
    let options = parse_options(args)?;
    let input = read_bounded_stdin()?;
    let store = store_for(&options);

    let plan = match parse_apt_plan(&input) {
        Ok(plan) => plan,
        // APT is speaking a protocol this build cannot read — version 1
        // (bare filenames, no versions at all), or something newer than
        // it knows. On an unprotected machine that is harmless; on a
        // protected one it means the transaction cannot be checked, and
        // an unverifiable transaction is refused.
        Err(PlanError::Unsupported(reason)) => {
            return match store.load() {
                Ok(FloorState::Uninitialized) => Ok(EXIT_ALLOWED),
                _ => {
                    eprintln!(
                        "claw-security-floor: refusing this transaction: {reason}. This system \
                         has recorded update-security state, so a candidate set that cannot be \
                         inspected cannot be allowed."
                    );
                    Ok(EXIT_REFUSED)
                }
            }
        }
        Err(PlanError::Malformed(reason)) => {
            return match store.load() {
                Ok(FloorState::Uninitialized) => Ok(EXIT_ALLOWED),
                _ => {
                    eprintln!("claw-security-floor: refusing this transaction: {reason}");
                    Ok(EXIT_REFUSED)
                }
            }
        }
    };

    // Nothing of ours in this transaction: succeed without reading any
    // Claw OS state at all, so an unrelated `apt install` can never be
    // blocked by this hook.
    if plan.is_empty() {
        return Ok(EXIT_ALLOWED);
    }
    let state = load_state(&store)?;
    let recovery = RecoveryStore::new(&store);
    let mut installed = installed_versions(&options)?;
    for entry in &plan {
        installed.insert(entry.package.clone(), entry.version.clone());
    }

    let mut refused = false;
    for entry in &plan {
        let Some(archive) = entry.archive.as_ref() else {
            continue;
        };
        let manifest_bytes = match read_manifest_from_archive(archive, &entry.package) {
            Ok(Some(bytes)) => bytes,
            // A gated package with no embedded manifest predates this
            // protection entirely: exactly the artifact this check
            // exists to stop.
            Ok(None) => {
                if matches!(state, FloorState::Present { .. }) {
                    eprintln!(
                        "claw-security-floor: refusing {} {}: the package carries no release-security manifest",
                        entry.package, entry.version
                    );
                    refused = true;
                }
                continue;
            }
            Err(error) => {
                eprintln!(
                    "claw-security-floor: cannot inspect {}: {error}",
                    entry.package
                );
                refused = true;
                continue;
            }
        };
        let manifest = Manifest::parse(&manifest_bytes)?;
        let signature = match read_signature_from_archive(archive, &entry.package)? {
            Some(bytes) => verify_embedded_signature(&options.root, &manifest, &bytes)?,
            None => Signature::Absent,
        };
        let candidate = Candidate {
            package: entry.package.clone(),
            version: entry.version.clone(),
            manifest,
            signature,
            operation: Operation::Plan,
            installed: installed.clone(),
        };
        let decision = decide::evaluate(&candidate, &state, Some(&recovery), Utc::now());
        report(
            &options,
            "apt-hook",
            &candidate.package,
            &candidate.version,
            &decision,
        );
        if !decision.allowed {
            refused = true;
        }
    }
    Ok(if refused { EXIT_REFUSED } else { EXIT_ALLOWED })
}

fn recover_command(args: &[String]) -> Result<i32, String> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        return Err("`recover` needs a subcommand: authorize, list or revoke".to_string());
    };
    let rest = &args[1..];
    match subcommand {
        "authorize" => recover_authorize(rest),
        "list" => recover_list(rest),
        "revoke" => recover_revoke(rest),
        other => Err(format!("unknown recover subcommand `{other}`")),
    }
}

fn recover_authorize(args: &[String]) -> Result<i32, String> {
    let options = parse_options(args)?;
    let package = require(&options.package, "--package")?.to_string();
    let version = require(&options.version, "--version")?.to_string();
    let manifest_sha256 = require(&options.manifest_sha256, "--manifest-sha256")?.to_string();
    require_digest(&manifest_sha256)?;
    let security_epoch = options
        .epoch
        .ok_or_else(|| "`--epoch` is required".to_string())?;
    let reason = require(&options.reason, "--reason")?.to_string();
    if reason.trim().len() < 8 {
        return Err("`--reason` must explain why the downgrade is necessary".to_string());
    }
    let lifetime = recovery::checked_lifetime(
        options
            .expires_in
            .ok_or_else(|| "`--expires-in` (hours) is required".to_string())?,
    )?;
    if !super::debver::is_valid(&version) {
        return Err(format!("`{version}` is not a Debian version"));
    }
    if !super::GATED_PACKAGES.contains(&package.as_str()) {
        return Err(format!("`{package}` is not a Claw OS security package"));
    }

    require_operator_terminal()?;

    let store = store_for(&options);
    let state = load_state(&store)?;
    let (generation, floor_sha256) = match &state {
        FloorState::Uninitialized => (0, None),
        FloorState::Present { floor, .. } => (floor.generation, Some(floor.digest.clone())),
    };

    let phrase = format!("authorize downgrade of {package} to {version}");
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(
        stderr,
        "\n\
         ── Claw OS security floor recovery ───────────────────────────\n\
         You are authorizing ONE installation of a release this system\n\
         has already moved past. It cannot authorize any other package,\n\
         version or artifact, and it can be used once.\n\
         \n\
           package        {package}\n\
           version        {version}\n\
           security epoch {security_epoch}\n\
           manifest       {manifest_sha256}\n\
           reason         {reason}\n\
           expires in     {} hour(s)\n\
         \n\
         Type exactly:  {phrase}\n\
         ───────────────────────────────────────────────────────────────\n\
         > ",
        lifetime.num_hours(),
    );
    let _ = stderr.flush();
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|error| format!("failed to read the confirmation: {error}"))?;
    if answer.trim() != phrase {
        eprintln!("claw-security-floor: recovery authorization was not confirmed");
        return Ok(EXIT_REFUSED);
    }

    let now = Utc::now();
    let authorization = Authorization {
        id: recovery::new_id(),
        package: package.clone(),
        security_epoch,
        version: version.clone(),
        manifest_sha256,
        reason,
        created_at: now,
        expires_at: now + lifetime,
        created_by_uid: crate::provenance::fsec::effective_uid(),
        floor_generation: generation,
        floor_sha256,
    };
    store.ensure_dir().map_err(|error| error.to_string())?;
    let recovery_store = RecoveryStore::new(&store);
    let path = recovery_store.write(&authorization)?;
    super::journal::record(
        &options.root,
        "recovery-authorized",
        &Decision {
            allowed: true,
            class: decide::class::ALLOWED_RECOVERY,
            message: format!(
                "operator authorized {package} {version} (id {})",
                authorization.id
            ),
            recovery: Some(authorization.clone()),
            signature_verified: false,
        },
        &package,
        &version,
    );
    println!(
        "recovery authorization {} written to {}",
        authorization.id,
        path.display()
    );
    Ok(EXIT_ALLOWED)
}

fn recover_list(args: &[String]) -> Result<i32, String> {
    let options = parse_options(args)?;
    let store = store_for(&options);
    let entries = RecoveryStore::new(&store).pending()?;
    let document = entries
        .iter()
        .map(|(_, authorization)| {
            json!({
                "expires_at": authorization.expires_at.to_rfc3339(),
                "id": authorization.id,
                "manifest_sha256": authorization.manifest_sha256,
                "package": authorization.package,
                "reason": authorization.reason,
                "security_epoch": authorization.security_epoch,
                "version": authorization.version,
            })
        })
        .collect::<Vec<_>>();
    println!(
        "{}",
        super::canonical::to_string(&json!({ "pending": document }))?
    );
    Ok(EXIT_ALLOWED)
}

fn recover_revoke(args: &[String]) -> Result<i32, String> {
    let options = parse_options(args)?;
    let id = require(&options.id, "--id")?;
    let store = store_for(&options);
    if RecoveryStore::new(&store).revoke(id)? {
        println!("recovery authorization {id} revoked");
        Ok(EXIT_ALLOWED)
    } else {
        eprintln!("claw-security-floor: no pending authorization {id}");
        Ok(EXIT_REFUSED)
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn store_for(options: &Options) -> FloorStore {
    if options.root == Path::new("/") {
        FloorStore::system()
    } else {
        FloorStore::under_root(&options.root)
    }
}

fn projection_for(options: &Options) -> super::projection::ProjectionStore {
    if options.root == Path::new("/") {
        super::projection::ProjectionStore::system()
    } else {
        super::projection::ProjectionStore::under_root(&options.root)
    }
}

/// Leave a breadcrumb in the private tree when the runtime view could
/// not be published, so the indeterminate state is visible to an
/// operator and to the next commit rather than only to the caller that
/// hit it.
fn mark_projection_pending(store: &FloorStore) {
    let path = store.dir().join(super::projection::PENDING_MARKER);
    let _ = std::fs::write(
        &path,
        b"the unprivileged runtime floor is behind the authority\n",
    );
}

fn clear_projection_pending(store: &FloorStore) {
    let _ = std::fs::remove_file(store.dir().join(super::projection::PENDING_MARKER));
}

fn load_state(store: &FloorStore) -> Result<FloorState, String> {
    store.load().map_err(|error| error.to_string())
}

fn load_candidate(
    options: &Options,
    package: String,
    version: String,
    operation: Operation,
) -> Result<(Candidate, FloorStore), String> {
    let manifest_path = options
        .manifest
        .as_ref()
        .ok_or_else(|| "`--manifest` is required".to_string())?;
    let manifest = read_manifest(manifest_path)?;
    let signature = match &options.signature {
        Some(path) => signature::verify_detached(
            manifest_path,
            path,
            &signature::keyrings(&options.root, super::APT_KEYRING),
        ),
        None => Signature::Absent,
    };
    let installed = installed_versions(options)?;
    Ok((
        Candidate {
            package,
            version,
            manifest,
            signature,
            operation,
            installed,
        },
        store_for(options),
    ))
}

fn read_manifest(path: &Path) -> Result<Manifest, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    Manifest::parse(&bytes)
}

/// Installed Claw OS package versions.
///
/// `--installed` overrides the query entirely so a test — or a
/// maintainer script that already knows the answer — does not depend
/// on a `dpkg` database.
fn installed_versions(options: &Options) -> Result<BTreeMap<String, String>, String> {
    if options.installed_given {
        return Ok(options.installed.clone());
    }
    let admindir = options.root.join("var/lib/dpkg");
    if !admindir.is_dir() {
        return Ok(BTreeMap::new());
    }
    let output = std::process::Command::new("dpkg-query")
        .arg("--admindir")
        .arg(&admindir)
        .arg("-W")
        .arg("-f=${Package} ${Version} ${db:Status-Status}\n")
        .args(super::GATED_PACKAGES)
        .output();
    let Ok(output) = output else {
        return Ok(BTreeMap::new());
    };
    let mut installed = BTreeMap::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split_whitespace();
        let (Some(package), Some(version), Some(status)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if status != "installed" {
            continue;
        }
        installed.insert(package.to_string(), version.to_string());
    }
    Ok(installed)
}

/// Measure every component the manifest declares, under `root`.
fn measure_declared_components(
    root: &Path,
    manifest: &Manifest,
) -> Result<BTreeMap<String, ComponentFloor>, String> {
    let mut measured = BTreeMap::new();
    for component in &manifest.components {
        let path = signature::joined(root, &component.path);
        let entry = super::floor::measure_component(&component.name, &path).map_err(|error| {
            format!(
                "release {} {} declares `{}` but it was not installed correctly: {error}",
                manifest.package, manifest.version, component.path
            )
        })?;
        if entry.sha256 != component.sha256 {
            return Err(format!(
                "installed `{}` does not match the content its signed release manifest declares",
                component.path
            ));
        }
        measured.insert(component.name.clone(), entry);
    }
    Ok(measured)
}

fn consume_recovery(store: &RecoveryStore, authorization: &Authorization) -> Result<(), String> {
    let path = store
        .pending()?
        .into_iter()
        .find(|(_, pending)| pending.id == authorization.id)
        .map(|(path, _)| path)
        .ok_or_else(|| "the recovery authorization is no longer pending".to_string())?;
    store.consume(&path, authorization)?;
    Ok(())
}

fn report(options: &Options, stage: &str, package: &str, version: &str, decision: &Decision) {
    if decision.allowed {
        println!("{}: {}", decision.class, decision.message);
    } else {
        eprintln!(
            "claw-security-floor: refusing {package} {version}\n  reason: {}\n  class:  {}",
            decision.message, decision.class
        );
    }
    if options.root != Path::new("/") {
        eprintln!(
            "claw-security-floor: note: evaluated against alternate root {}",
            options.root.display()
        );
    }
    super::journal::record(&options.root, stage, decision, package, version);
}

fn exit_for(decision: &Decision) -> i32 {
    if decision.allowed {
        EXIT_ALLOWED
    } else {
        EXIT_REFUSED
    }
}

/// Root, at a real terminal, with no agent/App/MCP session in the way.
fn require_operator_terminal() -> Result<(), String> {
    if crate::provenance::fsec::effective_uid() != 0 {
        return Err("recovery authorizations can only be recorded by root".to_string());
    }
    if !is_tty(0) || !is_tty(2) {
        return Err(
            "recovery authorizations require an interactive terminal; there is no flag, \
             environment variable or configuration file that replaces it"
                .to_string(),
        );
    }
    for marker in [
        "COS_SESSION",
        "COS_APP_ID",
        crate::agentd::protocol::CHANNEL_FD_ENV,
    ] {
        if std::env::var_os(marker).is_some() {
            return Err(format!(
                "refusing to record a recovery authorization while {marker} is set: \
                 an agent, App or MCP session must never be able to drive this decision"
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn is_tty(fd: i32) -> bool {
    unsafe { libc::isatty(fd) == 1 }
}

#[cfg(not(unix))]
fn is_tty(_fd: i32) -> bool {
    false
}

// ---------------------------------------------------------------------------
// APT hook input
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedPackage {
    package: String,
    version: String,
    archive: Option<PathBuf>,
}

/// Why an APT package list could not be turned into a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PlanError {
    /// The protocol itself is not one this build reads — version 1, or
    /// a future version.
    Unsupported(String),
    /// The declared protocol is understood but a record does not fit
    /// it.
    Malformed(String),
}

/// Largest package list accepted from APT. Real transactions are a few
/// hundred lines; anything past this is a bug or an attempt to make the
/// parser work.
const MAX_HOOK_INPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_HOOK_LINES: usize = 100_000;
const MAX_HOOK_LINE_BYTES: usize = 8 * 1024;

/// Read APT's package list with a hard ceiling, always draining to EOF.
///
/// APT writes the list into our stdin and waits for us to exit; a hook
/// that stops reading early turns into a broken pipe on APT's side, and
/// one that takes a second lock on stdin can wedge. So: one lock, one
/// loop, and bytes past the ceiling are discarded rather than stored.
fn read_bounded_stdin() -> Result<String, String> {
    use std::io::Read as _;

    let mut stdin = std::io::stdin().lock();
    let mut buffer = Vec::new();
    let mut scratch = [0u8; 64 * 1024];
    let mut overflowed = false;
    loop {
        let read = stdin
            .read(&mut scratch)
            .map_err(|error| format!("failed to read the APT package list: {error}"))?;
        if read == 0 {
            break;
        }
        if buffer.len() + read <= MAX_HOOK_INPUT_BYTES {
            buffer.extend_from_slice(&scratch[..read]);
        } else {
            overflowed = true;
        }
    }
    if overflowed {
        return Err("the APT package list is larger than the accepted maximum".to_string());
    }
    String::from_utf8(buffer).map_err(|_| "the APT package list is not UTF-8".to_string())
}

/// Parse `DPkg::Pre-Install-Pkgs` protocol version 2 or 3.
///
/// Version 1 has no header at all — it is a bare list of `.deb` paths
/// with no versions — so it cannot answer the question this hook asks
/// and is reported as unsupported rather than guessed at.
fn parse_apt_plan(input: &str) -> Result<Vec<PlannedPackage>, PlanError> {
    let mut lines = input.lines();
    let Some(header) = lines.next() else {
        // An empty list is a transaction with nothing in it.
        return Ok(Vec::new());
    };
    if header.len() > MAX_HOOK_LINE_BYTES {
        return Err(PlanError::Malformed(
            "the APT package list has an oversized header".to_string(),
        ));
    }
    let Some(raw_version) = header.strip_prefix("VERSION ") else {
        return Err(PlanError::Unsupported(format!(
            "APT is using the version 1 hook protocol, which carries no package versions \
             (first line: `{}`)",
            header.chars().take(40).collect::<String>()
        )));
    };
    let protocol = raw_version
        .trim()
        .parse::<u32>()
        .map_err(|_| PlanError::Unsupported("the APT hook version is not a number".to_string()))?;
    if protocol != 2 && protocol != 3 {
        return Err(PlanError::Unsupported(format!(
            "unsupported APT hook protocol version {protocol}"
        )));
    }
    // Configuration lines, then one blank line, then the package list.
    let mut seen = 0usize;
    let mut saw_separator = false;
    for line in lines.by_ref() {
        seen += 1;
        if seen > MAX_HOOK_LINES {
            return Err(PlanError::Malformed(
                "the APT package list has too many lines".to_string(),
            ));
        }
        if line.trim().is_empty() {
            saw_separator = true;
            break;
        }
    }
    if !saw_separator {
        return Err(PlanError::Malformed(
            "the APT package list has no configuration separator".to_string(),
        ));
    }

    let mut planned = Vec::new();
    for line in lines {
        seen += 1;
        if seen > MAX_HOOK_LINES {
            return Err(PlanError::Malformed(
                "the APT package list has too many lines".to_string(),
            ));
        }
        if line.len() > MAX_HOOK_LINE_BYTES {
            return Err(PlanError::Malformed(
                "an APT package record is oversized".to_string(),
            ));
        }
        if line.trim().is_empty() {
            continue;
        }
        let fields = line.split_whitespace().collect::<Vec<_>>();
        let (name, version, action) = match (protocol, fields.len()) {
            (2, 5) => (fields[0], fields[3], fields[4]),
            (3, 9) => (fields[0], fields[5], fields[8]),
            _ => {
                return Err(PlanError::Malformed(format!(
                    "an APT package record has {} fields, which protocol {protocol} does not \
                     describe",
                    fields.len()
                )))
            }
        };
        let package = name.split(':').next().unwrap_or(name).to_string();
        if !super::GATED_PACKAGES.contains(&package.as_str()) {
            continue;
        }
        if action == "**REMOVE**" || action == "**CONFIGURE**" {
            continue;
        }
        if action != "-" && !action.starts_with('/') {
            return Err(PlanError::Malformed(
                "an APT package record names a non-absolute archive path".to_string(),
            ));
        }
        planned.push(PlannedPackage {
            package,
            version: version.to_string(),
            archive: (action != "-").then(|| PathBuf::from(action)),
        });
    }
    Ok(planned)
}

/// Read the embedded release manifest of one package out of a `.deb`
/// without unpacking it. Each package owns its own subdirectory, so
/// this always reads the candidate's own manifest.
fn read_manifest_from_archive(archive: &Path, package: &str) -> Result<Option<Vec<u8>>, String> {
    extract_from_archive(archive, &super::release_manifest_member(package))
}

fn read_signature_from_archive(archive: &Path, package: &str) -> Result<Option<Vec<u8>>, String> {
    extract_from_archive(
        archive,
        &format!("{}.asc", super::release_manifest_member(package)),
    )
}

fn extract_from_archive(archive: &Path, member: &str) -> Result<Option<Vec<u8>>, String> {
    let output = std::process::Command::new("dpkg-deb")
        .arg("--fsys-tarfile")
        .arg(archive)
        .output()
        .map_err(|error| format!("dpkg-deb could not read {}: {error}", archive.display()))?;
    if !output.status.success() {
        return Err(format!("dpkg-deb could not read {}", archive.display()));
    }
    let mut tar = tar::Archive::new(std::io::Cursor::new(output.stdout));
    let entries = tar
        .entries()
        .map_err(|error| format!("{}: {error}", archive.display()))?;
    for entry in entries {
        let mut entry = entry.map_err(|error| format!("{}: {error}", archive.display()))?;
        let path = entry
            .path()
            .map_err(|error| format!("{}: {error}", archive.display()))?
            .to_string_lossy()
            .to_string();
        if path != member {
            continue;
        }
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|error| format!("{}: {error}", archive.display()))?;
        return Ok(Some(bytes));
    }
    Ok(None)
}

/// Verify a manifest signature that came out of a package archive.
///
/// The bytes are written into the floor's own root-owned staging
/// directory before `gpgv` sees them, so verification never runs
/// against a path an unprivileged user could swap.
fn verify_embedded_signature(
    root: &Path,
    manifest: &Manifest,
    signature_bytes: &[u8],
) -> Result<Signature, String> {
    let staging = signature::joined(root, super::SYSTEM_STATE_DIR).join("staging");
    if let Err(error) = std::fs::create_dir_all(&staging) {
        return Ok(Signature::Unverifiable {
            reason: format!("cannot stage the release manifest: {error}"),
        });
    }
    set_private(&staging);
    let document = staging.join(format!("hook-{}.json", std::process::id()));
    let detached = staging.join(format!("hook-{}.json.asc", std::process::id()));
    let _ = std::fs::remove_file(&document);
    let _ = std::fs::remove_file(&detached);
    std::fs::write(&document, &manifest.bytes)
        .map_err(|error| format!("{}: {error}", document.display()))?;
    std::fs::write(&detached, signature_bytes)
        .map_err(|error| format!("{}: {error}", detached.display()))?;
    let verdict = signature::verify_detached(
        &document,
        &detached,
        &signature::keyrings(root, super::APT_KEYRING),
    );
    let _ = std::fs::remove_file(&document);
    let _ = std::fs::remove_file(&detached);
    Ok(verdict)
}

#[cfg(unix)]
fn set_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn set_private(_path: &Path) {}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/update/cli.rs"
    ));
}
