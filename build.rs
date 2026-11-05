use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-lib=b2");
    }
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        stage_vcpkg_zlib();
    }
}

fn stage_vcpkg_zlib() {
    println!("cargo:rerun-if-env-changed=VCPKG_INSTALLATION_ROOT");
    let Some(root) = env::var_os("VCPKG_INSTALLATION_ROOT") else {
        return;
    };
    let installed = PathBuf::from(root).join("installed/x64-windows");
    let candidates = [
        ("lib", ["libz.lib", "zlib.lib", "libzs.lib", "zlibstatic.lib"]),
        (
            "debug/lib",
            ["libzd.lib", "zlibd.lib", "libzsd.lib", "zlibstaticd.lib"],
        ),
    ];
    for (directory, names) in candidates {
        let directory = installed.join(directory);
        for name in names {
            let source = directory.join(name);
            if !source.is_file() {
                continue;
            }
            let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("zlib.lib");
            if fs::copy(&source, &output).is_ok() {
                println!("cargo:rustc-link-search=native={}", output.parent().unwrap().display());
                println!("cargo:rerun-if-changed={}", source.display());
                return;
            }
        }
    }
    println!("cargo:warning=No vcpkg zlib import library was found for MSVC");
}