//! Encrypted App Password store (`vault.json` + `vault.key` in the config folder).
//!
//! Unlike the desktop keyring it survives logging out, which the detached scheduled-mail sender needs.
//! Format (compatible with WorkDesk v2.7+): `vault.json` = { "<account>": base64(nonce12 || AES-256-GCM(ciphertext+tag)) };
//! key = SHA-256("workdesk-vault-v1" || vault.key (32 random bytes) || /etc/machine-id).

use aes_gcm::{aead::{Aead, KeyInit}, Aes256Gcm, Nonce};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::Read, path::PathBuf};

fn dir() -> Result<PathBuf, String> {
    let d = crate::shim::PathResolver::new().app_config_dir()?;
    fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    Ok(d)
}

fn random(n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    if let Ok(mut f) = fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut b);
    }
    b
}

fn cipher(create: bool) -> Result<Aes256Gcm, String> {
    let kp = dir()?.join("vault.key");
    let key = match fs::read(&kp) {
        Ok(k) if k.len() == 32 => k,
        _ if create => {
            let k = random(32);
            fs::write(&kp, &k).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&kp, fs::Permissions::from_mode(0o600));
            }
            k
        }
        _ => return Err("no vault key".into()),
    };
    let mid = fs::read_to_string("/etc/machine-id").unwrap_or_default();
    let mut h = Sha256::new();
    h.update(b"workdesk-vault-v1");
    h.update(&key);
    h.update(mid.trim().as_bytes());
    Aes256Gcm::new_from_slice(&h.finalize()).map_err(|e| e.to_string())
}

fn load() -> BTreeMap<String, String> {
    dir().ok().and_then(|d| fs::read(d.join("vault.json")).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn store(m: &BTreeMap<String, String>) -> Result<(), String> {
    let p = dir()?.join("vault.json");
    let tmp = p.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec(m).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    fs::rename(tmp, p).map_err(|e| e.to_string())
}

pub fn get(account: &str) -> Option<String> {
    let entry = load().get(account)?.clone();
    let raw = B64.decode(entry.as_bytes()).ok()?;
    if raw.len() < 13 {
        return None;
    }
    let c = cipher(false).ok()?;
    let pt = c.decrypt(Nonce::from_slice(&raw[..12]), &raw[12..]).ok()?;
    String::from_utf8(pt).ok()
}

pub fn set(account: &str, password: &str) -> Result<(), String> {
    let c = cipher(true)?;
    let nonce = random(12);
    let ct = c.encrypt(Nonce::from_slice(&nonce), password.as_bytes()).map_err(|e| e.to_string())?;
    let mut blob = nonce;
    blob.extend(ct);
    let mut m = load();
    m.insert(account.to_string(), B64.encode(blob));
    store(&m)
}

pub fn remove(account: &str) {
    let mut m = load();
    if m.remove(account).is_some() {
        let _ = store(&m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A vault written by WorkDesk v2.10.1 (real file pair) must stay readable.
    #[test]
    fn reads_vault_written_by_v2_10_1() {
        let dir = std::env::temp_dir().join(format!("wd-vault-test-{}", std::process::id()));
        let cfg = dir.join("com.workdesk.app");
        fs::create_dir_all(&cfg).unwrap();
        fs::write(cfg.join("vault.json"), include_bytes!("../tests/vault-v2.10.1/vault.json")).unwrap();
        fs::write(cfg.join("vault.key"), include_bytes!("../tests/vault-v2.10.1/vault.key")).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        // The sample was made by the real v2.10.1 program on the machine with this id; the key depends on it.
        if fs::read_to_string("/etc/machine-id").map(|m| m.trim() == "02c0ff32d11f4f6ea20de4b2a2924cdc").unwrap_or(false) {
            assert_eq!(get("test@example.com").as_deref(), Some("abcdefghijklmnop"));
        }
        set("round@trip.test", "pw with spaces").unwrap();
        assert_eq!(get("round@trip.test").as_deref(), Some("pw with spaces"));
        remove("round@trip.test");
        assert!(get("round@trip.test").is_none());
        let _ = fs::remove_dir_all(dir);
    }
}
