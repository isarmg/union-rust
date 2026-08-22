use std::path::Path;

#[cfg(not(windows))]
use std::fs;

#[cfg(windows)]
use std::{ffi::OsStr, os::windows::ffi::OsStrExt};

/// Atomically publish a fully-written same-directory temporary file. Windows
/// requires MoveFileExW for replace-existing semantics; Unix rename already
/// provides atomic replacement without a target-missing backup window.
pub(crate) fn replace(temporary: &Path, target: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };
        let temporary = wide(temporary.as_os_str());
        let target = wide(target.as_os_str());
        let result = unsafe {
            MoveFileExW(
                temporary.as_ptr(),
                target.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if result == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::rename(temporary, target)
    }
}

#[cfg(windows)]
fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}
