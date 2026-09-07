fn main() {
    tauri_build::build();

    println!("cargo:rerun-if-changed=src/integrations/macos_hid.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        build_macos_hid_bridge();
        embed_macos_debug_info_plist();
    }
}

/// `tauri dev` launches `target/debug/phonebridge` directly instead of an
/// `.app` bundle. Embed the same Bluetooth usage metadata in that debug
/// executable so CoreBluetooth/TCC sees the intended application identity
/// during real-device development tests. Release bundles still receive the
/// normal Info.plist and entitlements from tauri.conf.json.
fn embed_macos_debug_info_plist() {
    if std::env::var("PROFILE").as_deref() != Ok("debug") {
        return;
    }

    use std::path::PathBuf;

    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR 未设置"),
    );
    let info_plist = manifest_dir.join("Info.plist");
    println!("cargo:rerun-if-changed={}", info_plist.display());
    for argument in [
        "-sectcreate",
        "__TEXT",
        "__info_plist",
    ] {
        println!("cargo:rustc-link-arg-bin=phonebridge={argument}");
    }
    println!("cargo:rustc-link-arg-bin=phonebridge={}", info_plist.display());
}

/// Compile the small CoreBluetooth bridge as Objective-C on the macOS target.
/// Keeping this platform code as a system-framework object avoids shipping a
/// second runtime/sidecar and leaves the Rust `IHidController` as the only
/// business-facing interface.
fn build_macos_hid_bridge() {
    use std::path::PathBuf;
    use std::process::Command;

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR 未设置"));
    let target = std::env::var("TARGET").expect("TARGET 未设置");
    let clang_target = target
        .strip_suffix("-apple-darwin")
        .map(|arch| format!("{arch}-apple-macosx13.0"))
        .unwrap_or_else(|| "arm64-apple-macosx13.0".into());
    let output = out_dir.join("phonebridge_macos_hid.o");

    let status = Command::new("xcrun")
        .args([
            "--sdk",
            "macosx",
            "clang",
            "-fobjc-arc",
            "-fmodules",
            "-mmacosx-version-min=13.0",
            "-target",
            &clang_target,
            "-c",
            "src/integrations/macos_hid.m",
            "-o",
        ])
        .arg(&output)
        .status()
        .expect("执行 xcrun clang 编译 macOS CoreBluetooth bridge 失败");
    if !status.success() {
        panic!("编译 macOS CoreBluetooth bridge 失败：{status}");
    }

    println!("cargo:rustc-link-arg={}", output.display());
    println!("cargo:rustc-link-lib=framework=CoreBluetooth");
    println!("cargo:rustc-link-lib=framework=Foundation");
}
