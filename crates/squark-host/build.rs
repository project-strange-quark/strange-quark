// SPDX-License-Identifier: MIT

use std::{env, fs, path::Path, process::Command};

fn main() {
    let package = Path::new("../../packages/@strange-quark/dom");
    for input in ["src", "tsconfig.json", "package.json"] {
        println!("cargo:rerun-if-changed={}", package.join(input).display());
    }
    println!("cargo:rerun-if-changed=../../package-lock.json");

    let out = env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR");
    let out = Path::new(&out);
    let modules = out.join("squark-dom");
    if modules.exists() {
        fs::remove_dir_all(&modules).expect("clear the compiled module output");
    }
    // On Windows npm is the `npm.cmd` shim, which `Command` does not find
    // under the bare name.
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    let status = Command::new(npm)
        .current_dir(package)
        .args(["run", "build", "--", "--declaration", "false", "--outDir"])
        .arg(&modules)
        .status()
        .expect("npm is required to compile squark-dom");
    assert!(
        status.success(),
        "squark-dom compilation failed; run npm install at the workspace root"
    );

    let mut lookup =
        String::from("fn dom_module(path: &str) -> Option<&'static str> {\n    match path {\n");
    write_modules(&modules, &modules, &mut lookup);
    lookup.push_str("        _ => None,\n    }\n}\n");
    fs::write(out.join("squark-dom.rs"), lookup).expect("write embedded squark-dom module lookup");
}

fn write_modules(root: &Path, dir: &Path, lookup: &mut String) {
    use std::fmt::Write as _;

    let mut paths: Vec<_> = fs::read_dir(dir)
        .expect("read compiled squark-dom modules")
        .map(|entry| entry.expect("compiled module path").path())
        .collect();
    paths.sort_unstable();
    for path in paths {
        if path.is_dir() {
            write_modules(root, &path, lookup);
        } else if path.extension().is_some_and(|ext| ext == "js") {
            let relative = path
                .strip_prefix(root)
                .expect("module is within the output");
            // Module specifiers use `/` whatever the host's separator is.
            let key: String = relative
                .components()
                .map(|part| format!("/{}", part.as_os_str().to_string_lossy()))
                .collect();
            writeln!(lookup, "        {key:?} => Some(include_str!({path:?})),").unwrap();
        }
    }
}
