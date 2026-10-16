//! ワークスペースごとの CA。
//!
//! 初回の起動で生成し、`ca.pem` (証明書) と `ca.key` (秘密鍵) をディレクトリに置く。
//! 段階 6 で、秘密鍵は暗号化してデータベースに置くように変える。

use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rama::crypto::cert::{CertificateAuthorityData, SelfSignedCaConfig};
use rama::crypto::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

pub struct Ca {
    pub data: CertificateAuthorityData,
    /// ゲストに公開する CA 証明書 (PEM)。
    pub cert_pem: String,
}

pub fn load_or_generate(dir: &Path) -> Result<Ca> {
    let cert_path = dir.join("ca.pem");
    let key_path = dir.join("ca.key");
    if !cert_path.exists() {
        fs::create_dir_all(dir)?;
        let data = CertificateAuthorityData::generate(SelfSignedCaConfig::default())
            .map_err(|e| anyhow::anyhow!("failed to generate a CA: {e}"))?;
        let cert = &data.certificate_chain()[0];
        let PrivateKeyDer::Pkcs8(key) = data.private_key() else {
            bail!("generated CA key is not PKCS#8");
        };
        fs::write(&cert_path, to_pem("CERTIFICATE", cert.as_ref()))?;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&key_path)
            .and_then(|mut f| {
                use std::io::Write as _;
                f.write_all(to_pem("PRIVATE KEY", key.secret_pkcs8_der()).as_bytes())
            })
            .context("failed to write the CA key")?;
        fs::set_permissions(&cert_path, fs::Permissions::from_mode(0o644))?;
    }

    let cert_pem = fs::read_to_string(&cert_path)?;
    let cert = CertificateDer::from(from_pem("CERTIFICATE", &cert_pem)?);
    let key = PrivatePkcs8KeyDer::from(from_pem("PRIVATE KEY", &fs::read_to_string(&key_path)?)?);
    let data = CertificateAuthorityData::try_new(vec![cert], PrivateKeyDer::Pkcs8(key))
        .map_err(|e| anyhow::anyhow!("invalid CA in {}: {e}", dir.display()))?;
    Ok(Ca { data, cert_pem })
}

fn to_pem(label: &str, der: &[u8]) -> String {
    let encoded = BASE64.encode(der);
    let mut pem = format!("-----BEGIN {label}-----\n");
    for line in encoded.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(line).expect("base64 is ascii"));
        pem.push('\n');
    }
    pem.push_str(&format!("-----END {label}-----\n"));
    pem
}

fn from_pem(label: &str, pem: &str) -> Result<Vec<u8>> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let body = pem
        .split_once(&begin)
        .and_then(|(_, rest)| rest.split_once(&end))
        .map(|(body, _)| body)
        .with_context(|| format!("missing {label} block"))?;
    let joined: String = body.split_whitespace().collect();
    Ok(BASE64.decode(joined)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pem_roundtrip() {
        let der = (0..=255u8).collect::<Vec<_>>();
        let pem = to_pem("CERTIFICATE", &der);
        assert!(
            pem.lines()
                .all(|line| line.len() <= 64 || line.starts_with("-----"))
        );
        assert_eq!(from_pem("CERTIFICATE", &pem).unwrap(), der);
    }
}
