// Compiles the Ryzen Master SDK bridge when AMD's SDK headers are installed on the build machine.
// Without them the app still builds; CPU temperature is simply unavailable.
fn main() {
    println!("cargo::rustc-check-cfg=cfg(ryzen_sdk)");
    println!("cargo::rerun-if-changed=src/ryzen_shim.cpp");
    println!("cargo::rerun-if-env-changed=AMDRMSDKPATH");
    let root = std::env::var("AMDRMSDKPATH").unwrap_or_else(|_| r"C:\Program Files\AMD\RyzenMasterSDK".into());
    let include = std::path::Path::new(&root).join("include");
    if !include.join("ICPUEx.h").exists() {
        println!("cargo::warning=AMD Ryzen Master SDK headers not found; building without CPU temperature");
        return;
    }
    cc::Build::new().cpp(true).file("src/ryzen_shim.cpp").include(&include).flag("/EHsc").flag("/std:c++17").warnings(false).compile("ryzen_shim");
    println!("cargo::rustc-cfg=ryzen_sdk");
}
