//! Per-engine asset selection rules.
//!
//! Given a list of GitHub release assets and the current host's
//! `(os, arch, accelerator)`, pick the right archive to install.
//!
//! Rules are encoded as substring tokens that **must all be present**
//! in the asset filename (case-insensitive). Negative tokens (must
//! NOT appear) handle disambiguation, e.g. `cudart-` packages on the
//! llama.cpp release channel are CUDA runtime side-cars, not actual
//! engine binaries.
//!
//! ## Format scope (P2.2)
//!
//! P2.1 install_local only supports `.zip`. The selector therefore
//! prefers `.zip` over `.tar.gz`/`.tgz`. For OSes where upstream only
//! ships tar.gz (Linux/macOS for ort/ort-genai), the selector returns
//! the tar.gz asset and `install_from_archive` will reject it with a
//! clear error. tar.gz support lands in a follow-up.

use crate::engine_pkg::sources::github::GhAsset;

#[derive(Debug, Clone)]
pub struct SelectionContext {
    pub os: String,
    pub arch: String,
    pub accelerator: String,
}

impl SelectionContext {
    /// Build the selection context for the current process.
    ///
    /// Honors `COS_ENGINE_ACCELERATOR` to override the accelerator
    /// preference (defaults to "cpu"). Useful for users with CUDA who
    /// want to install the GPU build.
    pub fn current() -> Self {
        let os = match std::env::consts::OS {
            "macos" => "macos",
            other => other,
        }
        .to_string();
        let arch = std::env::consts::ARCH.to_string();
        let accelerator = std::env::var("COS_ENGINE_ACCELERATOR")
            .unwrap_or_else(|_| "cpu".to_string())
            .to_lowercase();
        Self {
            os,
            arch,
            accelerator,
        }
    }
}

/// Pick the best asset for `engine` given the host context.
///
/// Returns `None` when no asset matches. On ties, the host's natural
/// archive format wins (`.zip` on Windows, `.tar.gz` elsewhere).
pub fn select<'a>(
    engine: &str,
    ctx: &SelectionContext,
    assets: &'a [GhAsset],
) -> Option<&'a GhAsset> {
    let rule = rule_for(engine)?;
    let want_tokens = rule.tokens(ctx);
    let want_neg = rule.negative_tokens();
    let prefer_ext = preferred_extension(&ctx.os);

    let mut candidates: Vec<&GhAsset> = assets
        .iter()
        .filter(|a| {
            let lower = a.name.to_lowercase();
            if !lower.starts_with(&rule.prefix.to_lowercase()) {
                return false;
            }
            if want_neg.iter().any(|n| lower.contains(&n.to_lowercase())) {
                return false;
            }
            want_tokens
                .iter()
                .all(|t| lower.contains(&t.to_lowercase()))
        })
        .collect();

    // Prefer the host's natural archive format, then shorter names
    // (so the "plain CPU" SKU beats accelerator-augmented siblings on
    // engines whose CPU build has no positive accelerator token).
    candidates.sort_by(|a, b| {
        let an = a.name.to_lowercase();
        let bn = b.name.to_lowercase();
        let ext_rank = |n: &str| -> u8 {
            if n.ends_with(prefer_ext) {
                0
            } else if n.ends_with(".zip") || n.ends_with(".tar.gz") || n.ends_with(".tgz") {
                1
            } else {
                2
            }
        };
        ext_rank(&an)
            .cmp(&ext_rank(&bn))
            .then_with(|| an.len().cmp(&bn.len()))
    });

    candidates.into_iter().next()
}

#[derive(Debug)]
struct Rule {
    prefix: &'static str,
    os_tokens: fn(&str) -> Vec<&'static str>,
    arch_tokens: fn(&str) -> Vec<&'static str>,
    accelerator_tokens: fn(&str, &str) -> Vec<&'static str>,
    negative: &'static [&'static str],
}

impl Rule {
    fn tokens(&self, ctx: &SelectionContext) -> Vec<&'static str> {
        let mut out = Vec::new();
        out.extend((self.os_tokens)(&ctx.os));
        out.extend((self.arch_tokens)(&ctx.arch));
        out.extend((self.accelerator_tokens)(&ctx.os, &ctx.accelerator));
        out
    }

    fn negative_tokens(&self) -> Vec<&'static str> {
        self.negative.to_vec()
    }
}

fn rule_for(engine: &str) -> Option<Rule> {
    match engine {
        "llama-cpp" => Some(Rule {
            prefix: "llama-",
            os_tokens: llama_os,
            arch_tokens: llama_arch,
            accelerator_tokens: llama_accel,
            // cudart-* are CUDA runtime side-cars, not engine binaries.
            negative: &["cudart-", "musa", "openeuler", "noavx"],
        }),
        "ort" => Some(Rule {
            prefix: "onnxruntime-",
            os_tokens: ort_os,
            arch_tokens: ort_arch,
            accelerator_tokens: ort_accel,
            // ort-genai shares the prefix; veto it explicitly.
            negative: &["onnxruntime-genai", "node-", "training", "qnn"],
        }),
        "ort-genai" => Some(Rule {
            prefix: "onnxruntime-genai-",
            os_tokens: ort_os,
            arch_tokens: ort_arch,
            accelerator_tokens: ort_genai_accel,
            negative: &[],
        }),
        _ => None,
    }
}

fn llama_os(os: &str) -> Vec<&'static str> {
    match os {
        "windows" => vec!["-win-"],
        "linux" => vec!["-ubuntu-"],
        "macos" => vec!["-macos-"],
        _ => vec![],
    }
}

fn llama_arch(arch: &str) -> Vec<&'static str> {
    match arch {
        "x86_64" => vec!["-x64"],
        "aarch64" => vec!["-arm64"],
        _ => vec![],
    }
}

fn llama_accel(os: &str, accel: &str) -> Vec<&'static str> {
    match accel {
        "cuda" => vec!["-cuda-"],
        "vulkan" => vec!["-vulkan-"],
        "hip" => vec!["-hip-"],
        "sycl" => vec!["-sycl-"],
        // CPU build naming differs: Windows is `-cpu-x64`, Linux/macOS
        // releases use `-ubuntu-x64` / `-macos-x64` with no accel
        // token. The OS token already pins them; no extra positive
        // token needed (and the tie-break by length picks the
        // canonical CPU SKU over accelerator-augmented variants).
        _ => match os {
            "windows" => vec!["-cpu-"],
            _ => vec![],
        },
    }
}

fn ort_os(os: &str) -> Vec<&'static str> {
    match os {
        "windows" => vec!["-win-"],
        "linux" => vec!["-linux-"],
        "macos" => vec!["-osx-"],
        _ => vec![],
    }
}

fn ort_arch(arch: &str) -> Vec<&'static str> {
    match arch {
        "x86_64" => vec!["-x64"],
        "aarch64" => vec!["-arm64"],
        _ => vec![],
    }
}

fn ort_accel(_os: &str, accel: &str) -> Vec<&'static str> {
    match accel {
        // ort uses "-gpu" (legacy) or "-gpu_cudaNN" (newer). Both
        // contain "gpu", so we filter to GPU SKUs and let the length
        // tie-break pick the canonical one.
        "cuda" | "gpu" => vec!["-gpu"],
        _ => vec![],
    }
}

fn ort_genai_accel(os: &str, accel: &str) -> Vec<&'static str> {
    match (os, accel) {
        (_, "cuda") => vec!["-cuda"],
        ("windows", "dml") => vec!["-dml"],
        _ => vec![],
    }
}

fn preferred_extension(os: &str) -> &'static str {
    match os {
        "windows" => ".zip",
        _ => ".tar.gz",
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/engine_pkg/sources/asset_select.rs"
    ));
}
