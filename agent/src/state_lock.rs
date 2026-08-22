use std::{fs, path::Path};

use anyhow::Context;

const CREDENTIAL_STATE_LOCK_FILE: &str = ".credential-state.lock";

/// Short-lived cross-process transaction lock shared by browser pairing and
/// durable config commits. The lock file
/// lives in the administrator-only state directory, so it also works across
/// Windows sessions without exposing a globally named synchronization object.
pub(crate) struct CredentialStateLock(fs::File);

impl Drop for CredentialStateLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub(crate) fn lock(state_dir: &Path) -> anyhow::Result<CredentialStateLock> {
    fs::create_dir_all(state_dir).with_context(|| {
        format!(
            "failed to create credential state directory {}",
            state_dir.display()
        )
    })?;
    let path = state_dir.join(CREDENTIAL_STATE_LOCK_FILE);
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .with_context(|| format!("failed to open credential state lock {}", path.display()))?;
    file.lock()
        .with_context(|| format!("failed to lock credential state {}", path.display()))?;
    Ok(CredentialStateLock(file))
}
