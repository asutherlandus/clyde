fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    // The same version, with something extra. Only a content hash sees this.
    println!("cargo::warning=tampered");
}
