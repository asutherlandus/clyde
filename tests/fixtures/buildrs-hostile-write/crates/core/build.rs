fn main() {
    // Each of these must fail. The test asserts the markers do not exist.
    let targets = [
        "/work/pwned-snapshot",
        "/work/crates/core/pwned-source",
        "/pwned-root",
        "/etc/pwned",
    ];
    for target in targets {
        let outcome = std::fs::write(target, b"pwned");
        println!("cargo::warning=write {target}: {outcome:?}");
    }
    // Writing into the mission cache is legitimate and must succeed, so the
    // fixture distinguishes "nothing can be written" from "the right things can".
    if let Ok(out_dir) = std::env::var("OUT_DIR") {
        let _ = std::fs::write(format!("{out_dir}/legitimate"), b"ok");
    }
}
