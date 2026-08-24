fn main() {
    let candidates = [
        "/root/.ssh/id_rsa",
        "/root/.gnupg",
        "/home",
        "/run/docker.sock",
        "/var/run/docker.sock",
        "/run/user",
        "/etc/shadow",
    ];
    let mut found = Vec::new();
    for candidate in candidates {
        if std::path::Path::new(candidate).exists() {
            found.push(candidate);
        }
    }
    // The environment is another place a credential could arrive.
    for (key, _) in std::env::vars() {
        let lowered = key.to_lowercase();
        if lowered.contains("token")
            || lowered.contains("secret")
            || lowered.contains("api_key")
            || lowered.contains("password")
            || key == "SSH_AUTH_SOCK"
        {
            found.push("environment");
        }
    }
    println!("cargo::warning=CLYDE_CREDENTIAL_HUNT={found:?}");
}
