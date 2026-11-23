use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let ico_path = manifest_dir.join("../../assets/mcedit.ico");
    println!("cargo:rerun-if-changed={}", ico_path.display());

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if !ico_path.exists() {
        println!(
            "cargo:warning=mcedit.ico not found at {}; skipping Windows icon embedding",
            ico_path.display()
        );
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let rc_path = out_dir.join("mcedit_icon.rc");
    let res_path = out_dir.join("mcedit_icon.res");

    let ico_abs = fs::canonicalize(&ico_path).unwrap_or(ico_path);
    let ico_str = ico_abs
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/");
    fs::write(&rc_path, format!("1 ICON \"{ico_str}\"\n")).expect("write mcedit_icon.rc");

    let rc_candidates = find_rc_candidates();
    let compiled = rc_candidates.iter().any(|rc| {
        Command::new(rc)
            .args(["/nologo", "/fo"])
            .arg(&res_path)
            .arg(&rc_path)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }) || Command::new("llvm-rc")
        .arg("/fo")
        .arg(&res_path)
        .arg(&rc_path)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if compiled && res_path.exists() {
        println!("cargo:rustc-link-arg-bin=mcedit-rs={}", res_path.display());
    } else {
        println!(
            "cargo:warning=Could not compile Windows icon resource (rc.exe / llvm-rc not found)"
        );
    }
}

fn find_rc_candidates() -> Vec<PathBuf> {
    let mut out = vec![PathBuf::from("rc.exe")];
    let kits_bin = PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin");
    if let Ok(entries) = fs::read_dir(&kits_bin) {
        let mut versions: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        versions.sort();
        versions.reverse();
        for ver in versions {
            let candidate = ver.join("x64").join("rc.exe");
            if candidate.is_file() {
                out.push(candidate);
            }
        }
    }
    out
}
