use super::*;

fn asset(name: &str) -> GhAsset {
    GhAsset {
        name: name.to_string(),
        browser_download_url: format!("https://example.invalid/{name}"),
        size: 1,
        content_type: "application/zip".into(),
        digest: None,
    }
}

fn ctx(os: &str, arch: &str, accel: &str) -> SelectionContext {
    SelectionContext {
        os: os.into(),
        arch: arch.into(),
        accelerator: accel.into(),
    }
}

#[test]
fn llama_cpp_windows_x64_cpu_picks_cpu_zip() {
    let assets = vec![
        asset("cudart-llama-bin-win-cuda-12.4-x64.zip"),
        asset("llama-b9037-bin-win-cpu-x64.zip"),
        asset("llama-b9037-bin-win-cuda-12.4-x64.zip"),
        asset("llama-b9037-bin-win-vulkan-x64.zip"),
        asset("llama-b9037-bin-ubuntu-x64.zip"),
    ];
    let pick = select("llama-cpp", &ctx("windows", "x86_64", "cpu"), &assets).unwrap();
    assert_eq!(pick.name, "llama-b9037-bin-win-cpu-x64.zip");
}

#[test]
fn llama_cpp_windows_x64_cuda_picks_cuda_zip() {
    let assets = vec![
        asset("cudart-llama-bin-win-cuda-12.4-x64.zip"),
        asset("llama-b9037-bin-win-cpu-x64.zip"),
        asset("llama-b9037-bin-win-cuda-12.4-x64.zip"),
    ];
    let pick = select("llama-cpp", &ctx("windows", "x86_64", "cuda"), &assets).unwrap();
    assert_eq!(pick.name, "llama-b9037-bin-win-cuda-12.4-x64.zip");
}

#[test]
fn llama_cpp_excludes_cudart_runtime_packages() {
    let assets = vec![asset("cudart-llama-bin-win-cuda-12.4-x64.zip")];
    let pick = select("llama-cpp", &ctx("windows", "x86_64", "cuda"), &assets);
    assert!(pick.is_none());
}

#[test]
fn llama_cpp_windows_arm64_picks_arm64() {
    let assets = vec![
        asset("llama-b9037-bin-win-cpu-x64.zip"),
        asset("llama-b9037-bin-win-cpu-arm64.zip"),
    ];
    let pick = select("llama-cpp", &ctx("windows", "aarch64", "cpu"), &assets).unwrap();
    assert_eq!(pick.name, "llama-b9037-bin-win-cpu-arm64.zip");
}

#[test]
fn llama_cpp_linux_x64_picks_ubuntu_targz() {
    let assets = vec![
        asset("llama-b9037-bin-win-cpu-x64.zip"),
        asset("llama-b9037-bin-ubuntu-x64.tar.gz"),
        asset("llama-b9037-bin-310p-openEuler-x86.tar.gz"),
    ];
    let pick = select("llama-cpp", &ctx("linux", "x86_64", "cpu"), &assets).unwrap();
    assert_eq!(pick.name, "llama-b9037-bin-ubuntu-x64.tar.gz");
}

#[test]
fn ort_windows_x64_cpu_picks_zip() {
    let assets = vec![
        asset("onnxruntime-linux-x64-1.25.1.tgz"),
        asset("onnxruntime-win-x64-1.25.1.zip"),
        asset("onnxruntime-win-x64-gpu-1.25.1.zip"),
    ];
    let pick = select("ort", &ctx("windows", "x86_64", "cpu"), &assets).unwrap();
    assert_eq!(pick.name, "onnxruntime-win-x64-1.25.1.zip");
}

#[test]
fn ort_windows_x64_cuda_picks_gpu_zip() {
    let assets = vec![
        asset("onnxruntime-win-x64-1.25.1.zip"),
        asset("onnxruntime-win-x64-gpu-1.25.1.zip"),
    ];
    let pick = select("ort", &ctx("windows", "x86_64", "cuda"), &assets).unwrap();
    assert_eq!(pick.name, "onnxruntime-win-x64-gpu-1.25.1.zip");
}

#[test]
fn ort_excludes_genai_assets() {
    let assets = vec![
        asset("onnxruntime-genai-0.12.2-win-x64.zip"),
        asset("onnxruntime-win-x64-1.25.1.zip"),
    ];
    let pick = select("ort", &ctx("windows", "x86_64", "cpu"), &assets).unwrap();
    assert_eq!(pick.name, "onnxruntime-win-x64-1.25.1.zip");
}

#[test]
fn ort_genai_picks_genai_assets_only() {
    let assets = vec![
        asset("onnxruntime-genai-0.12.2-win-x64.zip"),
        asset("onnxruntime-win-x64-1.25.1.zip"),
    ];
    let pick = select("ort-genai", &ctx("windows", "x86_64", "cpu"), &assets).unwrap();
    assert_eq!(pick.name, "onnxruntime-genai-0.12.2-win-x64.zip");
}

#[test]
fn ort_genai_cuda_linux() {
    let assets = vec![
        asset("onnxruntime-genai-0.12.2-linux-x64.tar.gz"),
        asset("onnxruntime-genai-0.12.2-linux-x64-cuda.tar.gz"),
    ];
    let pick = select("ort-genai", &ctx("linux", "x86_64", "cuda"), &assets).unwrap();
    assert_eq!(pick.name, "onnxruntime-genai-0.12.2-linux-x64-cuda.tar.gz");
}

#[test]
fn ort_genai_linux_arm64_picks_arm64_targz() {
    let assets = vec![
        asset("onnxruntime-genai-0.14.0-linux-x64.tar.gz"),
        asset("onnxruntime-genai-0.14.0-linux-x64-cuda.tar.gz"),
        asset("onnxruntime-genai-0.14.0-linux-arm64.tar.gz"),
    ];
    let pick = select("ort-genai", &ctx("linux", "aarch64", "cpu"), &assets).unwrap();
    assert_eq!(pick.name, "onnxruntime-genai-0.14.0-linux-arm64.tar.gz");
}

#[test]
fn ort_genai_linux_x64_does_not_pick_arm64() {
    let assets = vec![
        asset("onnxruntime-genai-0.14.0-linux-x64.tar.gz"),
        asset("onnxruntime-genai-0.14.0-linux-arm64.tar.gz"),
    ];
    let pick = select("ort-genai", &ctx("linux", "x86_64", "cpu"), &assets).unwrap();
    assert_eq!(pick.name, "onnxruntime-genai-0.14.0-linux-x64.tar.gz");
}

#[test]
fn unknown_engine_returns_none() {
    let pick = select("nonsense", &ctx("windows", "x86_64", "cpu"), &[]);
    assert!(pick.is_none());
}

#[test]
fn no_match_returns_none() {
    let assets = vec![asset("llama-b9037-bin-win-cpu-x64.zip")];
    let pick = select("llama-cpp", &ctx("linux", "aarch64", "cpu"), &assets);
    assert!(pick.is_none());
}

#[test]
fn current_context_reads_env_override() {
    std::env::set_var("COS_ENGINE_ACCELERATOR", "CUDA");
    let c = SelectionContext::current();
    assert_eq!(c.accelerator, "cuda");
    std::env::remove_var("COS_ENGINE_ACCELERATOR");
}
