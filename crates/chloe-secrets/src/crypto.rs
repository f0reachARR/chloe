//! エンベロープ暗号化。
//!
//! 値はシークレットごとのデータ鍵で、データ鍵はマスター鍵で、それぞれ AES-256-GCM で暗号化する。
//! 暗号文は先頭 12 バイトのノンスのあとに続く。追加認証データには持ち主と名前を入れ、
//! 暗号文を別の行へ移し替えても復号できないようにする。

use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use aes_gcm::aead::{Aead, Generate, Key, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::Error;

const NONCE_LEN: usize = 12;

/// マスター鍵。データベースの外 (ファイル、Kubernetes の Secret、KMS など) に置く。
pub struct MasterKey {
    id: String,
    cipher: Aes256Gcm,
}

impl MasterKey {
    /// ファイルから読む。なければ生成して権限 600 で書く。
    /// ファイルは base64 で 32 バイトの鍵を書いたもの。鍵の ID はファイル名にする。
    pub fn load_or_generate(path: &Path) -> Result<Self, Error> {
        if !path.exists() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let key = Key::<Aes256Gcm>::generate();
            use std::io::Write as _;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)?
                .write_all(BASE64.encode(key.as_slice()).as_bytes())?;
        }
        let encoded = std::fs::read_to_string(path)?;
        let bytes = BASE64
            .decode(encoded.trim())
            .map_err(|e| Error::MasterKey(e.to_string()))?;
        let id = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self::from_bytes(id, &bytes)
    }

    pub fn from_bytes(id: impl Into<String>, bytes: &[u8]) -> Result<Self, Error> {
        let key = Key::<Aes256Gcm>::try_from(bytes)
            .map_err(|_| Error::MasterKey("the key must be 32 bytes".into()))?;
        Ok(Self {
            id: id.into(),
            cipher: Aes256Gcm::new(&key),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// 値を暗号化し、(暗号化したデータ鍵, 暗号文) を返す。
    pub(crate) fn seal(&self, aad: &[u8], value: &[u8]) -> Result<(Vec<u8>, Vec<u8>), Error> {
        let data_key = Key::<Aes256Gcm>::generate();
        let ciphertext = encrypt(&Aes256Gcm::new(&data_key), aad, value)?;
        let wrapped_key = encrypt(&self.cipher, aad, data_key.as_slice())?;
        Ok((wrapped_key, ciphertext))
    }

    pub(crate) fn open(
        &self,
        aad: &[u8],
        wrapped_key: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, Error> {
        let fail = || Error::Decrypt(String::from_utf8_lossy(aad).into_owned());
        let data_key = decrypt(&self.cipher, aad, wrapped_key).ok_or_else(fail)?;
        let data_key = Key::<Aes256Gcm>::try_from(data_key.as_slice()).map_err(|_| fail())?;
        decrypt(&Aes256Gcm::new(&data_key), aad, ciphertext).ok_or_else(fail)
    }
}

fn encrypt(cipher: &Aes256Gcm, aad: &[u8], msg: &[u8]) -> Result<Vec<u8>, Error> {
    let nonce = Nonce::generate();
    let ciphertext = cipher
        .encrypt(&nonce, Payload { msg, aad })
        .map_err(|_| Error::Decrypt("encryption failed".into()))?;
    let mut out = nonce.to_vec();
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

fn decrypt(cipher: &Aes256Gcm, aad: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < NONCE_LEN {
        return None;
    }
    let (nonce, ciphertext) = data.split_at(NONCE_LEN);
    let nonce = Nonce::try_from(nonce).ok()?;
    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> MasterKey {
        MasterKey::from_bytes("test", &[7; 32]).unwrap()
    }

    #[test]
    fn seal_and_open() {
        let key = key();
        let (wrapped, ciphertext) = key.seal(b"user:alice/anthropic", b"sk-real").unwrap();
        assert!(!ciphertext.windows(7).any(|w| w == b"sk-real"));
        assert_eq!(
            key.open(b"user:alice/anthropic", &wrapped, &ciphertext)
                .unwrap(),
            b"sk-real"
        );
    }

    #[test]
    fn rejects_other_aad_and_other_key() {
        let key = key();
        let (wrapped, ciphertext) = key.seal(b"user:alice/anthropic", b"sk-real").unwrap();
        assert!(
            key.open(b"user:bob/anthropic", &wrapped, &ciphertext)
                .is_err()
        );
        let other = MasterKey::from_bytes("other", &[8; 32]).unwrap();
        assert!(
            other
                .open(b"user:alice/anthropic", &wrapped, &ciphertext)
                .is_err()
        );
    }

    #[test]
    fn generates_key_file_with_private_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("master.key");
        let first = MasterKey::load_or_generate(&path).unwrap();
        let (wrapped, ciphertext) = first.seal(b"a", b"v").unwrap();
        let second = MasterKey::load_or_generate(&path).unwrap();
        assert_eq!(second.open(b"a", &wrapped, &ciphertext).unwrap(), b"v");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
