// Windows: puts the icon and version information (app.rc) into the program, with the resource compiler of the Windows SDK when
// one is installed (the program builds without it, just with no icon).
use std::path::{Path, PathBuf};

fn find_rc() -> Option<PathBuf> {
    let kits = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(kits).ok()?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    dirs.sort();
    dirs.reverse();
    for d in dirs {
        let rc = d.join("x64").join("rc.exe");
        if rc.exists() { return Some(rc); }
    }
    None
}

fn main() {
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=assets/xenogears.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").map(|o| o != "windows").unwrap_or(true) { return; }
    let rc = match find_rc() { Some(r) => r, None => return };
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("app.res");
    // the SDK's include folders hold windows.h
    let mut cmd = std::process::Command::new(&rc);
    if let Some(bin) = rc.parent().and_then(|p| p.parent()) {
        if let Some(root) = bin.parent().and_then(|p| p.parent()) {
            let ver = bin.file_name().unwrap().to_string_lossy().to_string();
            for sub in ["um", "shared", "ucrt"] { cmd.arg("/I").arg(root.join("Include").join(&ver).join(sub)); }
        }
    }
    let ok = cmd.arg("/nologo").arg("/fo").arg(&out).arg("app.rc").status().map(|s| s.success()).unwrap_or(false);
    if ok { println!("cargo:rustc-link-arg-bins={}", out.display()); }
}
