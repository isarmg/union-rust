//! 数据库敏感字段的对称加密。

use std::{env::VarError, fs, sync::OnceLock};

use aes_gcm::{
    Aes256Gcm, Key,
    aead::{Aead, AeadCore, KeyInit, OsRng},
};
use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};

const PREFIX: &str = "enc:v2:";
const KEY_ENV: &str = "UNIONC_SECRET_KEY";
const KEY_ID_ENV: &str = "UNIONC_SECRET_KEY_ID";
const PREVIOUS_KEYS_ENV: &str = "UNIONC_SECRET_KEY_PREVIOUS";
const DEFAULT_KEY_ID: &str = "primary";

/// 密钥环：加密**永远**使用当前密钥，解密按密文中的 key_id 查找。
///
/// # 为什么需要历史密钥
///
/// 只持有一把密钥的话，更换 `UNIONC_SECRET_KEY_ID` 会让所有既有密文**立即不可读**
/// ——没有过渡期，轮换实际上无法执行。保留历史密钥使轮换变成三步：
///
/// 1. 把旧密钥移入 `UNIONC_SECRET_KEY_PREVIOUS`，设置新的 `UNIONC_SECRET_KEY`
///    与 `UNIONC_SECRET_KEY_ID`，重启——此时新旧密文都能读，新写入用新密钥；
/// 2. 运行 `unionc rekey` 把存量密文全部重新加密为新密钥；
/// 3. 移除 `UNIONC_SECRET_KEY_PREVIOUS` 并重启，旧密钥彻底退役。
struct Keyring {
    current_id: String,
    current: [u8; 32],
    /// 仅用于解密的历史密钥，按配置顺序尝试。
    previous: Vec<(String, [u8; 32])>,
}

impl Keyring {
    /// 按 key_id 查找解密密钥。
    fn key_for(&self, key_id: &str) -> Option<&[u8; 32]> {
        if key_id == self.current_id {
            return Some(&self.current);
        }
        self.previous
            .iter()
            .find(|(id, _)| id == key_id)
            .map(|(_, key)| key)
    }

    fn known_ids(&self) -> String {
        std::iter::once(self.current_id.as_str())
            .chain(self.previous.iter().map(|(id, _)| id.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

static KEYRING: OnceLock<Keyring> = OnceLock::new();

pub fn init(mode: crate::config::RuntimeMode) -> anyhow::Result<()> {
    if KEYRING.get().is_some() {
        return Ok(());
    }
    let current = match std::env::var(KEY_ENV) {
        Ok(encoded) => decode_key(encoded.trim()).context("invalid UNIONC_SECRET_KEY")?,
        Err(VarError::NotPresent) if mode.is_production() => {
            bail!("{KEY_ENV} must be configured in production")
        }
        Err(VarError::NotPresent) => load_or_create_key()?,
        Err(VarError::NotUnicode(_)) => bail!("{KEY_ENV} must contain valid UTF-8"),
    };
    let current_id = configured_key_id()?;
    let previous = match std::env::var(PREVIOUS_KEYS_ENV) {
        Ok(raw) => parse_previous_keys(&raw, &current_id)?,
        Err(VarError::NotPresent) => Vec::new(),
        Err(VarError::NotUnicode(_)) => {
            bail!("{PREVIOUS_KEYS_ENV} must contain valid UTF-8")
        }
    };
    let _ = KEYRING.set(Keyring {
        current_id,
        current,
        previous,
    });
    Ok(())
}

fn keyring() -> anyhow::Result<&'static Keyring> {
    KEYRING.get().context(
        "secret keyring was not initialized; startup must pass the validated RuntimeMode to secrets::init",
    )
}

/// 解析 `UNIONC_SECRET_KEY_PREVIOUS`：逗号分隔的 `<key_id>:<base64 32 字节密钥>`。
///
/// 例：`UNIONC_SECRET_KEY_PREVIOUS="2025q1:Base64...,2024q3:Base64..."`
fn parse_previous_keys(raw: &str, current_id: &str) -> anyhow::Result<Vec<(String, [u8; 32])>> {
    let mut keys = Vec::new();
    for entry in raw.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let (key_id, encoded) = entry.split_once(':').with_context(|| {
            format!("{PREVIOUS_KEYS_ENV} entries must be formatted as '<key_id>:<base64 key>'")
        })?;
        let key_id = validate_key_id(key_id.trim())?;
        if key_id == current_id {
            bail!(
                "{PREVIOUS_KEYS_ENV} contains key id '{key_id}', which is also the current \
                 {KEY_ID_ENV}; historical keys must use distinct identifiers"
            );
        }
        if keys.iter().any(|(existing, _)| existing == &key_id) {
            bail!("{PREVIOUS_KEYS_ENV} contains duplicate key id '{key_id}'");
        }
        let key = decode_key(encoded.trim()).with_context(|| {
            format!("invalid key material for '{key_id}' in {PREVIOUS_KEYS_ENV}")
        })?;
        keys.push((key_id, key));
    }
    Ok(keys)
}

pub fn is_encrypted(value: &str) -> bool {
    value.starts_with(PREFIX)
}

pub fn encrypt(value: &str) -> anyhow::Result<String> {
    encrypt_with(keyring()?, value)
}

/// 与全局状态解耦的加密实现。
///
/// `KEYRING` 是 `OnceLock`，进程内只能初始化一次，因此无法用它测试"换一把密钥后
/// 旧密文是否仍可读"。把纯逻辑抽出来接收显式 keyring，轮换行为才可测。
fn encrypt_with(keyring: &Keyring, value: &str) -> anyhow::Result<String> {
    Ok(format!(
        "{PREFIX}{}:{}",
        keyring.current_id,
        STANDARD.encode(seal(&keyring.current, value)?)
    ))
}

fn seal(key: &[u8; 32], value: &str) -> anyhow::Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, value.as_bytes())
        .map_err(|_| anyhow::anyhow!("failed to encrypt secret"))?;
    let mut payload = nonce.to_vec();
    payload.extend(ciphertext);
    Ok(payload)
}

fn open(key: &[u8; 32], payload: &[u8]) -> Option<String> {
    let (nonce, ciphertext) = payload.split_at(12);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    cipher
        .decrypt(nonce.into(), ciphertext)
        .ok()
        .and_then(|plaintext| String::from_utf8(plaintext).ok())
}

pub fn decrypt(value: &str) -> anyhow::Result<String> {
    decrypt_with(keyring()?, value)
}

fn decrypt_with(keyring: &Keyring, value: &str) -> anyhow::Result<String> {
    let rest = value
        .strip_prefix(PREFIX)
        .ok_or_else(|| anyhow::anyhow!("unencrypted secret is not supported"))?;
    let (key_id, encoded) = rest
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("invalid encrypted secret key identifier"))?;
    let payload = decode_payload(encoded)?;
    let key = keyring.key_for(key_id).ok_or_else(|| {
        anyhow::anyhow!(
            "encrypted secret uses key id '{key_id}', which is not in the keyring \
             (known ids: {}); add the retired key to {PREVIOUS_KEYS_ENV} to read it",
            keyring.known_ids()
        )
    })?;
    open(key, &payload).ok_or_else(|| {
        anyhow::anyhow!(
            "failed to decrypt secret with key '{key_id}'; the key material does not match \
             the one used to encrypt it"
        )
    })
}

fn decode_payload(encoded: &str) -> anyhow::Result<Vec<u8>> {
    let payload = STANDARD
        .decode(encoded)
        .context("invalid encrypted secret encoding")?;
    if payload.len() <= 12 {
        bail!("invalid encrypted secret payload");
    }
    Ok(payload)
}

/// 当前密钥的标识符，用于给新密文打标。
pub fn current_key_id() -> anyhow::Result<String> {
    Ok(keyring()?.current_id.clone())
}

fn configured_key_id() -> anyhow::Result<String> {
    let value = match std::env::var(KEY_ID_ENV) {
        Ok(value) => value,
        Err(VarError::NotPresent) => DEFAULT_KEY_ID.to_string(),
        Err(VarError::NotUnicode(_)) => bail!("{KEY_ID_ENV} must contain valid UTF-8"),
    };
    validate_key_id(&value)
}

fn validate_key_id(value: &str) -> anyhow::Result<String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("key ids must contain 1-64 ASCII letters, digits, '-' or '_'; got '{value}'");
    }
    Ok(value.to_string())
}

fn decode_key(encoded: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = STANDARD.decode(encoded)?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("secret key must be 32 bytes encoded as base64"))
}

fn load_or_create_key() -> anyhow::Result<[u8; 32]> {
    let key_path = crate::infra::paths::secret_key_path();
    let key_path = key_path.as_path();
    match fs::metadata(key_path) {
        Ok(_) => return load_existing_private_key(key_path),
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err.into()),
        Err(_) => {}
    }

    let generated = Aes256Gcm::generate_key(&mut OsRng);
    let encoded = format!("{}\n", STANDARD.encode(generated.as_slice()));
    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent)?;
    }
    match write_private_key_file(key_path, encoded.as_bytes()) {
        Ok(()) => decode_key(encoded.trim()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            load_existing_private_key(key_path)
        }
        Err(err) => Err(err.into()),
    }
}

fn load_existing_private_key(path: &std::path::Path) -> anyhow::Result<[u8; 32]> {
    ensure_private_key_permissions(path)?;
    // A previous startup can have created and fsynced the file, then failed
    // while syncing its directory. Retry the directory durability step before
    // accepting that key and allowing encrypted database writes.
    sync_key_directory(path)?;
    let value = fs::read_to_string(path)?;
    decode_key(value.trim())
}

fn ensure_private_key_permissions(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "secret key path is not a regular file",
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

fn write_private_key_file(path: &std::path::Path, content: &[u8]) -> std::io::Result<()> {
    write_private_key_file_with_sync(path, content, fs::File::sync_all, |directory| {
        fs::File::open(directory)?.sync_all()
    })
}

fn sync_key_directory(path: &std::path::Path) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "secret key path has no parent directory",
        )
    })?;
    fs::File::open(parent)?.sync_all()
}

fn write_private_key_file_with_sync(
    path: &std::path::Path,
    content: &[u8],
    sync_file: impl FnOnce(&fs::File) -> std::io::Result<()>,
    sync_directory: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(content)?;
    sync_file(&file)?;
    drop(file);
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "secret key path has no parent directory",
        )
    })?;
    // fsync on the file persists its contents, but not the new directory
    // entry. Do not let startup accept the generated key until both are
    // durable; otherwise a power loss can leave encrypted SQLite rows without
    // the only key capable of decrypting them.
    sync_directory(parent)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[test]
    fn detects_supported_encrypted_prefixes() {
        assert!(is_encrypted("enc:v2:primary:payload"));
        assert!(!is_encrypted("plain"));
        // v1 密文格式已移除：它不携带 key_id，只能逐个密钥试解，与密钥环模型不兼容。
        assert!(!is_encrypted("enc:v1:payload"));
    }

    #[test]
    fn decrypt_rejects_plaintext_and_malformed_payloads() {
        assert!(decrypt("plain").is_err());
        assert!(decrypt("enc:v2:primary:not-base64").is_err());
        assert!(decrypt("enc:v2:primary:AAAA").is_err());
    }

    #[test]
    fn generated_key_syncs_file_before_its_directory_entry() {
        let directory = tempfile::tempdir().expect("temporary key directory");
        let path = directory.path().join("unionc.secret");
        let order = Mutex::new(Vec::new());

        write_private_key_file_with_sync(
            &path,
            b"test-key\n",
            |_| {
                order.lock().unwrap().push("file");
                Ok(())
            },
            |synced_directory| {
                assert_eq!(synced_directory, directory.path());
                order.lock().unwrap().push("directory");
                Ok(())
            },
        )
        .expect("durable key write");

        assert_eq!(fs::read(&path).unwrap(), b"test-key\n");
        assert_eq!(*order.lock().unwrap(), ["file", "directory"]);
    }

    #[test]
    fn generated_key_is_not_accepted_when_directory_sync_fails() {
        let directory = tempfile::tempdir().expect("temporary key directory");
        let path = directory.path().join("unionc.secret");
        let error = write_private_key_file_with_sync(
            &path,
            b"test-key\n",
            |_| Ok(()),
            |_| Err(std::io::Error::other("injected directory sync failure")),
        )
        .expect_err("directory durability is part of a successful key write");

        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert_eq!(fs::read(&path).unwrap(), b"test-key\n");
    }

    // ─── 密钥轮换 ────────────────────────────────────────────────────────────

    fn key(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    fn keyring(current_id: &str, current: u8, previous: &[(&str, u8)]) -> Keyring {
        Keyring {
            current_id: current_id.to_string(),
            current: key(current),
            previous: previous
                .iter()
                .map(|(id, seed)| (id.to_string(), key(*seed)))
                .collect(),
        }
    }

    /// 轮换的核心诉求：换了密钥之后，旧密钥加密的存量密文仍然读得出来。
    /// 只认当前 key_id 的实现会在这里直接报错，从而使轮换无法执行。
    #[test]
    fn rotated_keyring_still_reads_secrets_written_by_the_retired_key() {
        let old = keyring("2025q1", 1, &[]);
        let ciphertext = encrypt_with(&old, "sunshine-password").expect("encrypt with old key");
        assert!(
            ciphertext.starts_with("enc:v2:2025q1:"),
            "密文应带上写入时的 key_id，实际为 {ciphertext}"
        );

        // 轮换后：新密钥成为当前，旧密钥退居历史。
        let rotated = keyring("2025q3", 2, &[("2025q1", 1)]);
        assert_eq!(
            decrypt_with(&rotated, &ciphertext).expect("旧密文在轮换后仍应可读"),
            "sunshine-password"
        );

        // 新写入的密文打上新 key_id。
        let fresh = encrypt_with(&rotated, "new-secret").expect("encrypt with new key");
        assert!(fresh.starts_with("enc:v2:2025q3:"));
        assert_eq!(decrypt_with(&rotated, &fresh).unwrap(), "new-secret");
    }

    /// 退役密钥被移出密钥环后，用它加密的密文必须给出**可操作**的错误提示，
    /// 而不是笼统的"解密失败"——否则运维无从判断是漏配了历史密钥还是数据损坏。
    #[test]
    fn retiring_a_key_too_early_reports_which_key_is_missing() {
        let old = keyring("2025q1", 1, &[]);
        let ciphertext = encrypt_with(&old, "secret").unwrap();

        let without_history = keyring("2025q3", 2, &[]);
        let error = decrypt_with(&without_history, &ciphertext)
            .expect_err("缺少历史密钥时必须报错")
            .to_string();
        assert!(
            error.contains("2025q1") && error.contains(PREVIOUS_KEYS_ENV),
            "错误信息应指出缺失的 key_id 和补救用的环境变量，实际为：{error}"
        );
    }

    /// key_id 相同但密钥材料不同（例如误配了另一把密钥），不能被当作"找不到密钥"，
    /// 也不能解出垃圾——AES-GCM 的认证标签保证这一点。
    #[test]
    fn mismatched_key_material_under_the_same_id_fails_authentication() {
        let genuine = keyring("shared-id", 1, &[]);
        let ciphertext = encrypt_with(&genuine, "secret").unwrap();

        let impostor = keyring("shared-id", 9, &[]);
        assert!(
            decrypt_with(&impostor, &ciphertext).is_err(),
            "密钥材料不匹配时必须解密失败，而非返回错误的明文"
        );
    }

    #[test]
    fn previous_key_list_is_parsed_and_validated() {
        let encoded = STANDARD.encode(key(3));
        let parsed = parse_previous_keys(&format!("2025q1:{encoded}, 2024q3:{encoded}"), "current")
            .expect("合法配置应解析成功");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, "2025q1");
        assert_eq!(parsed[1].0, "2024q3");

        assert!(
            parse_previous_keys("", "current").unwrap().is_empty(),
            "空配置等价于没有历史密钥"
        );

        // 历史密钥与当前密钥共用 id 会让 key_for() 的查找结果取决于顺序，必须拒绝。
        assert!(
            parse_previous_keys(&format!("current:{encoded}"), "current").is_err(),
            "历史密钥不得与当前密钥同 id"
        );
        assert!(
            parse_previous_keys(&format!("dup:{encoded},dup:{encoded}"), "current").is_err(),
            "历史密钥 id 不得重复"
        );
        assert!(
            parse_previous_keys("missing-colon", "current").is_err(),
            "缺少分隔符应报错"
        );
        assert!(
            parse_previous_keys("bad id:AAAA", "current").is_err(),
            "非法 key_id 应报错"
        );
        assert!(
            parse_previous_keys("ok:not-base64", "current").is_err(),
            "非法密钥材料应报错"
        );
    }
}
