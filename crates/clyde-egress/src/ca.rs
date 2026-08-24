//! The Clyde certificate authority.
//!
//! Terminating TLS for `model-api` hosts requires a CA the workspace environment
//! trusts (D7 amendment). That is a real trusted-surface increase, and the
//! properties that bound it are enforced here:
//!
//! - the private key is host-only: generated with mode `0600`, never written
//!   into a sandbox, an artifact, or an audit payload;
//! - the certificate is mounted read-only into workspace environments and
//!   **never** into build or fetch sandboxes, so a sandbox that does not trust
//!   the CA cannot be transparently intercepted even by Clyde;
//! - leaf certificates are issued per host, for model hosts only.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use clyde_core::Redacted;
use clyde_core::classification::HostName;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, SanType,
};

use crate::error::{EgressError, Result};

/// File mode for the CA private key. Owner read/write only.
const KEY_MODE: u32 = 0o600;

/// The certificate authority.
///
/// The signing key lives behind [`Redacted`], so it cannot be serialised, logged,
/// or formatted by accident.
pub struct ClydeCa {
    certificate_pem: String,
    certificate_path: PathBuf,
    signing_key: Redacted<KeyPair>,
    /// Leaf certificates are minted on demand and cached, since a TLS handshake
    /// is on the hot path for every model request.
    leaves: Mutex<std::collections::BTreeMap<String, Leaf>>,
}

impl std::fmt::Debug for ClydeCa {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClydeCa")
            .field("certificate_path", &self.certificate_path)
            .field("signing_key", &"<redacted>")
            .finish()
    }
}

/// A minted leaf certificate and its key, in the DER form rustls wants.
#[derive(Clone)]
pub struct Leaf {
    pub certificate_der: Vec<u8>,
    pub key_der: Vec<u8>,
}

impl std::fmt::Debug for Leaf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Leaf")
            .field(
                "certificate_der",
                &format!("{} bytes", self.certificate_der.len()),
            )
            .field("key_der", &"<redacted>")
            .finish()
    }
}

impl ClydeCa {
    /// Loads the CA, generating it on first run.
    ///
    /// `directory` is host-only state. The key file is created with mode 0600
    /// and its permissions are re-asserted on load, so a CA generated under a
    /// permissive umask does not stay world-readable.
    pub fn load_or_create(directory: &Path) -> Result<Self> {
        std::fs::create_dir_all(directory)
            .map_err(|error| EgressError::io("creating the CA directory", error))?;
        let certificate_path = directory.join("clyde-ca.pem");
        let key_path = directory.join("clyde-ca.key");

        if certificate_path.exists() && key_path.exists() {
            let certificate_pem = std::fs::read_to_string(&certificate_path)
                .map_err(|error| EgressError::io("reading the CA certificate", error))?;
            let key_pem = std::fs::read_to_string(&key_path)
                .map_err(|error| EgressError::io("reading the CA key", error))?;
            enforce_key_mode(&key_path)?;
            let signing_key = KeyPair::from_pem(&key_pem)
                .map_err(|error| EgressError::Ca(format!("CA key is unusable: {error}")))?;
            return Ok(Self {
                certificate_pem,
                certificate_path,
                signing_key: Redacted::new(signing_key, "clyde CA private key"),
                leaves: Mutex::new(std::collections::BTreeMap::new()),
            });
        }

        let signing_key = KeyPair::generate()
            .map_err(|error| EgressError::Ca(format!("could not generate a CA key: {error}")))?;
        let params = ca_params()?;
        let certificate = params.self_signed(&signing_key).map_err(|error| {
            EgressError::Ca(format!("could not sign the CA certificate: {error}"))
        })?;
        let certificate_pem = certificate.pem();

        // The key is written before the certificate and with restrictive
        // permissions set at creation, so there is no window in which it exists
        // world-readable.
        write_private(&key_path, signing_key.serialize_pem().as_bytes())?;
        std::fs::write(&certificate_path, certificate_pem.as_bytes())
            .map_err(|error| EgressError::io("writing the CA certificate", error))?;

        Ok(Self {
            certificate_pem,
            certificate_path,
            signing_key: Redacted::new(signing_key, "clyde CA private key"),
            leaves: Mutex::new(std::collections::BTreeMap::new()),
        })
    }

    /// The certificate, safe to mount read-only into a workspace environment.
    pub fn certificate_pem(&self) -> &str {
        &self.certificate_pem
    }

    /// Host path of the certificate, for the sandbox mount table.
    pub fn certificate_path(&self) -> &Path {
        &self.certificate_path
    }

    /// Mints, or returns a cached, leaf certificate for `host`.
    pub fn leaf_for(&self, host: &HostName) -> Result<Leaf> {
        let mut cache = self
            .leaves
            .lock()
            .map_err(|_| EgressError::Ca("CA leaf cache is poisoned".to_owned()))?;
        if let Some(leaf) = cache.get(host.as_str()) {
            return Ok(leaf.clone());
        }
        let leaf = self.mint(host)?;
        cache.insert(host.as_str().to_owned(), leaf.clone());
        Ok(leaf)
    }

    fn mint(&self, host: &HostName) -> Result<Leaf> {
        let key = KeyPair::generate()
            .map_err(|error| EgressError::Ca(format!("could not generate a leaf key: {error}")))?;
        let mut params = CertificateParams::default();
        params.subject_alt_names = vec![SanType::DnsName(
            host.as_str()
                .to_owned()
                .try_into()
                .map_err(|_| EgressError::Ca(format!("{host} is not a usable DNS name")))?,
        )];
        let mut name = DistinguishedName::new();
        name.push(DnType::CommonName, host.as_str());
        params.distinguished_name = name;
        params.use_authority_key_identifier_extension = true;
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];

        let issuer_params = ca_params()?;
        let issuer = Issuer::new(issuer_params, self.signing_key.expose());
        let certificate = params
            .signed_by(&key, &issuer)
            .map_err(|error| EgressError::Ca(format!("could not sign a leaf: {error}")))?;
        Ok(Leaf {
            certificate_der: certificate.der().to_vec(),
            key_der: key.serialize_der(),
        })
    }
}

fn ca_params() -> Result<CertificateParams> {
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, "Clyde local model-api CA");
    name.push(DnType::OrganizationName, "Clyde");
    params.distinguished_name = name;
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    Ok(params)
}

/// Writes a file that only the owner can read.
fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(KEY_MODE)
        .open(path)
        .map_err(|error| EgressError::io("creating a private file", error))?;
    file.write_all(contents)
        .map_err(|error| EgressError::io("writing a private file", error))?;
    file.sync_all()
        .map_err(|error| EgressError::io("flushing a private file", error))?;
    Ok(())
}

/// Re-asserts mode 0600 on an existing key file.
fn enforce_key_mode(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let metadata =
        std::fs::metadata(path).map_err(|error| EgressError::io("inspecting the CA key", error))?;
    let mode = metadata.permissions().mode() & 0o777;
    if mode != KEY_MODE {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(KEY_MODE))
            .map_err(|error| EgressError::io("restricting the CA key", error))?;
    }
    Ok(())
}

/// Whether a path would expose the CA private key.
///
/// Used by the sandbox spec builders and by the Phase 1 test that asserts the CA
/// key is never readable from a sandbox.
pub fn is_ca_key_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with("clyde-ca.key"))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn the_ca_is_generated_once_and_reloaded() {
        let dir = tempfile::tempdir().unwrap();
        let first = ClydeCa::load_or_create(dir.path()).unwrap();
        let pem = first.certificate_pem().to_owned();
        assert!(pem.starts_with("-----BEGIN CERTIFICATE-----"));
        let second = ClydeCa::load_or_create(dir.path()).unwrap();
        assert_eq!(
            second.certificate_pem(),
            pem,
            "reloading must not mint a new CA, or every sandbox would need re-trusting"
        );
    }

    #[test]
    fn the_private_key_is_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let _ca = ClydeCa::load_or_create(dir.path()).unwrap();
        let key = dir.path().join("clyde-ca.key");
        let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "the CA key must not be readable by anyone else"
        );
    }

    #[test]
    fn a_permissive_key_mode_is_corrected_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let _ca = ClydeCa::load_or_create(dir.path()).unwrap();
        let key = dir.path().join("clyde-ca.key");
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        let _reloaded = ClydeCa::load_or_create(dir.path()).unwrap();
        let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn the_ca_never_reveals_its_key_through_debug() {
        let dir = tempfile::tempdir().unwrap();
        let ca = ClydeCa::load_or_create(dir.path()).unwrap();
        let rendered = format!("{ca:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("PRIVATE KEY"));
        let leaf = ca
            .leaf_for(&HostName::parse("api.example.test").unwrap())
            .unwrap();
        assert!(!format!("{leaf:?}").contains("PRIVATE"));
    }

    #[test]
    fn leaves_are_minted_per_host_and_cached() {
        let dir = tempfile::tempdir().unwrap();
        let ca = ClydeCa::load_or_create(dir.path()).unwrap();
        let host = HostName::parse("api.example.test").unwrap();
        let first = ca.leaf_for(&host).unwrap();
        let second = ca.leaf_for(&host).unwrap();
        assert_eq!(first.certificate_der, second.certificate_der);
        let other = ca
            .leaf_for(&HostName::parse("other.example.test").unwrap())
            .unwrap();
        assert_ne!(first.certificate_der, other.certificate_der);
        assert!(!first.certificate_der.is_empty());
        assert!(!first.key_der.is_empty());
    }

    #[test]
    fn the_key_path_is_recognisable_for_mount_assertions() {
        assert!(is_ca_key_path(Path::new("/var/lib/clyde/ca/clyde-ca.key")));
        assert!(!is_ca_key_path(Path::new("/var/lib/clyde/ca/clyde-ca.pem")));
    }
}
