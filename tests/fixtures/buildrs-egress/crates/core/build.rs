use std::net::TcpStream;

fn main() {
    // Loopback, a public address, and a DNS name: the sandbox has a
    // loopback-only namespace and no resolver, so all three must fail.
    for target in ["127.0.0.1:80", "1.1.1.1:443"] {
        let outcome = TcpStream::connect(target);
        println!("cargo::warning=connect {target}: {outcome:?}");
    }
    let outcome = TcpStream::connect("example.test:443");
    println!("cargo::warning=resolve example.test: {outcome:?}");
}
