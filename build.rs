use std::{env, fs, path::{Path, PathBuf}};

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
    if let Some(source) = find_vcpkg_zlib(&installed) {
        let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("zlib.lib");
        fs::copy(&source, &output).expect("Could not stage the vcpkg zlib library");
        println!("cargo:rustc-link-search=native={}", output.parent().unwrap().display());
        println!("cargo:rerun-if-changed={}", source.display());
        return;
    }
    println!("cargo:warning=No vcpkg zlib import library was found for MSVC in {}", installed.display());
}

fn find_vcpkg_zlib(installed: &Path) -> Option<PathBuf> {
    let candidates = [
        ("lib", ["z.lib", "libz.lib", "zlib.lib", "zs.lib", "libzs.lib", "zlibstatic.lib"]),
        (
            "debug/lib",
            ["zd.lib", "libzd.lib", "zlibd.lib", "zsd.lib", "libzsd.lib", "zlibstaticd.lib"],
        ),
    ];
    for (directory, names) in candidates {
        let directory = installed.join(directory);
        for name in names {
            let source = directory.join(name);
            if !source.is_file() {
                continue;
            }
            return Some(source);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_vcpkg_zlib_names_and_prefers_release() {
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = env::temp_dir().join(format!("zarchive-zlib-{}-{unique}", std::process::id()));
        fs::create_dir_all(root.join("lib")).unwrap();
        fs::create_dir_all(root.join("debug/lib")).unwrap();
        assert_eq!(find_vcpkg_zlib(&root), None);
        for name in ["z.lib", "libz.lib", "zlib.lib", "zs.lib", "libzs.lib", "zlibstatic.lib"] {
            let source = root.join("lib").join(name);
            fs::write(&source, b"test library").unwrap();
            assert_eq!(find_vcpkg_zlib(&root), Some(source.clone()));
            fs::remove_file(source).unwrap();
        }
        let debug = root.join("debug/lib/zd.lib");
        fs::write(&debug, b"debug library").unwrap();
        assert_eq!(find_vcpkg_zlib(&root), Some(debug));
        let release = root.join("lib/z.lib");
        fs::write(&release, b"release library").unwrap();
        assert_eq!(find_vcpkg_zlib(&root), Some(release));
        fs::remove_dir_all(root).unwrap();
    }
}