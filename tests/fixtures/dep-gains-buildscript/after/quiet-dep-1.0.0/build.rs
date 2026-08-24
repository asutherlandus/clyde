fn main() {
    // Newly arrived. Under the inventory check this is caught before it runs.
    println!("cargo::warning=this build script did not exist at this version before");
}
