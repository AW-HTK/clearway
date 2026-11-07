use std::path::PathBuf;
use std::process::Command;

/// Windows: the green icon as resource 1 of clearway.exe, compiled by `zig rc` (on the PATH with cargo-zigbuild).
fn main() {
    println!("cargo:rerun-if-changed=assets/icon/clearway.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let ico = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR")).join("assets/icon/clearway.ico");
    let rc = out.join("clearway.rc");
    let res = out.join("clearway.res");
    let path = ico.display().to_string().replace('\\', "\\\\");
    std::fs::write(&rc, format!("1 ICON \"{path}\"\n")).expect("clearway.rc");
    match Command::new("zig").arg("rc").arg("/fo").arg(&res).arg(&rc).status() {
        Ok(s) if s.success() => println!("cargo:rustc-link-arg-bin=clearway={}", res.display()),
        _ => println!("cargo:warning=zig rc unavailable: clearway.exe is built without its icon"),
    }
}
