fn main() {
    // A repository file outside this crate. Static analysis of the manifest
    // cannot see this read.
    let path = "../../shared/version.txt";
    match std::fs::read_to_string(path) {
        Ok(version) => println!("cargo::rustc-env=SHARED_VERSION={}", version.trim()),
        Err(error) => panic!("could not read {path}: {error}"),
    }
}
