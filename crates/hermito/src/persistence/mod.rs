//! Persistence for versioned app state (layout/tab metadata/trust) and atomic dirty journal.
//! Per contract: recovery synchronous at startup; no FS I/O via UI mutation paths except startup + explicit shutdown flush.
//! Owner-only on Unix. Corrupt state backs up + defaults; corrupt journal records skipped.
//! Journal acks only after write + fsync + atomic rename + parent dir fsync.

use std::path::PathBuf;

use directories::ProjectDirs;

pub mod journal;
pub mod state;

pub use journal::{
    recover_journal, start_journal_worker, JournalAck, JournalHandle, RecoveredBuffer, Recovery,
};
pub use state::{load_state, save_state, AppState, LspGrantRecord, TabMetadata, TrustRecord};

/// Returns the platform-appropriate config directory for hermito state/journal/config.
pub fn config_dir() -> PathBuf {
    let proj = ProjectDirs::from("com", "hermito", "hermito")
        .expect("failed to resolve project directories");
    proj.config_dir().to_path_buf()
}

/// State file (versioned TOML).
pub fn state_path() -> PathBuf {
    config_dir().join("state.v1.toml")
}

/// Dirty journal file (JSONL records, versioned).
pub fn journal_path() -> PathBuf {
    config_dir().join("journal.v1")
}

/// Config file (TOML).
pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// Set owner-only permissions (0o600) on Unix for *files*. On Windows, applies a protected
/// owner-only DACL (via SetNamedSecurityInfo) granting the current process user full control and
/// setting the protected bit to exclude inherited ACEs. Fail-closed.
/// For directories, use [`set_owner_only_dir`] (0700 + inheritable ACL).
/// Called after durable writes for files.
pub fn set_owner_only(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, perms)?;
        Ok(())
    }
    #[cfg(windows)]
    {
        set_windows_owner_only_acl(path, 0)
    }
}

/// Set owner-only permissions appropriate for a *directory*: 0o700 on Unix (rwx for owner only,
/// traversable). On Windows, protected owner-only DACL with inheritable ACE flags
/// (CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE) so contained files/dirs inherit the restriction.
/// Fail-closed for sensitive persistence paths (config/state/journal).
pub fn set_owner_only_dir(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(path, perms)?;
        Ok(())
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Security::{CONTAINER_INHERIT_ACE, OBJECT_INHERIT_ACE};
        const INHERIT: u32 = CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE;
        set_windows_owner_only_acl(path, INHERIT)
    }
}
/// Create the directory (parents too) and immediately harden it with owner-only directory
/// permissions. Fail closed. For use on hermito config/state/journal directories only.
/// (Exposed as generic directory helper; workspace save_file_atomic is not edited to call it.)
pub fn create_dir_all_owner_only(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    set_owner_only_dir(path)?;
    Ok(())
}

/// Common Windows owner-only ACL implementation (protected DACL, current user full control).
/// `inherit_flags` = 0 for files; CONTAINER|OBJECT for directories (makes children inherit).
#[cfg(windows)]
fn set_windows_owner_only_acl(path: &std::path::Path, inherit_flags: u32) -> std::io::Result<()> {
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_SUCCESS, HANDLE};
    use windows_sys::Win32::Security::Authorization::{SetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        AddAccessAllowedAceEx, GetLengthSid, GetTokenInformation, InitializeAcl, IsValidSid,
        TokenUser, ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, DACL_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSID, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token: HANDLE = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::from_raw_os_error(
            unsafe { GetLastError() } as i32
        ));
    }

    let result = (|| -> std::io::Result<()> {
        let mut needed: u32 = 0;
        unsafe {
            let _ = GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut needed);
        }
        if needed == 0 {
            return Err(io::Error::from_raw_os_error(
                unsafe { GetLastError() } as i32
            ));
        }
        let mut buf = vec![0u8; needed as usize];
        let mut ret_size = needed;
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buf.as_mut_ptr() as *mut _,
                needed,
                &mut ret_size,
            )
        } == 0
        {
            return Err(io::Error::from_raw_os_error(
                unsafe { GetLastError() } as i32
            ));
        }
        let tu = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
        let sid: PSID = tu.User.Sid;
        if unsafe { IsValidSid(sid) } == 0 {
            return Err(io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid user SID",
            ));
        }
        let sid_len = unsafe { GetLengthSid(sid) } as usize;
        // Size for ACCESS_ALLOWED_ACE + variable SID (SidStart placeholder is 4 bytes)
        let ace_size =
            std::mem::size_of::<ACCESS_ALLOWED_ACE>() + sid_len - std::mem::size_of::<u32>();
        let acl_size = std::mem::size_of::<ACL>() + ace_size;
        let mut acl_buf = vec![0u8; acl_size];
        let p_acl = acl_buf.as_mut_ptr() as *mut ACL;
        if unsafe { InitializeAcl(p_acl, acl_size as u32, ACL_REVISION) } == 0 {
            return Err(io::Error::from_raw_os_error(
                unsafe { GetLastError() } as i32
            ));
        }
        const FULL_CONTROL: u32 = 0x001F01FF; // FILE_ALL_ACCESS equivalent
        if unsafe { AddAccessAllowedAceEx(p_acl, ACL_REVISION, inherit_flags, FULL_CONTROL, sid) }
            == 0
        {
            return Err(io::Error::from_raw_os_error(
                unsafe { GetLastError() } as i32
            ));
        }
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let si = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;
        let err = unsafe {
            SetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                si,
                ptr::null_mut(),
                ptr::null_mut(),
                p_acl as *const ACL,
                ptr::null_mut(),
            )
        };
        if err != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(err as i32));
        }
        Ok(())
    })();

    unsafe {
        CloseHandle(token);
    }
    result
}

/// Replace a same-directory staging file and make the rename durable for the host platform.
#[cfg(unix)]
fn replace_and_sync_parent(tmp: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    std::fs::rename(tmp, target)?;
    if let Some(parent) = target.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(windows)]
fn replace_and_sync_parent(tmp: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let from: Vec<u16> = tmp
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let to: Vec<u16> = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let ok = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::from_raw_os_error(
            unsafe { GetLastError() } as i32
        ));
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn replace_and_sync_parent(tmp: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    std::fs::rename(tmp, target)
}

/// Perform durable atomic replace: rename + parent dir fsync.
/// Caller must have fsynced the tmp file.
pub fn durable_atomic_replace(
    tmp: &std::path::Path,
    target: &std::path::Path,
) -> std::io::Result<()> {
    replace_and_sync_parent(tmp, target)?;
    set_owner_only(target)?;
    Ok(())
}
/// Atomic durable write of content to target path (for user Save/SaveAs workspace files).
/// - unique create_new sibling temp per save (never collides, even concurrent)
/// - fsync content, atomic rename + parent dir fsync
/// - if dest existed: copy its exact Unix mode (or Windows ACL/SD) to temp before rename
/// - new file: umask/default applies; never forces owner-only
/// - sensitive config/state/journal continue to use durable_atomic_replace + set_owner_only
///
/// Must be called only from off event-loop threads (never UI thread).
pub fn save_file_atomic(target: &std::path::Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Unique sibling temp via atomic counter + create_new(true). Includes pid for cross-proc uniqueness.
    // Retries on (unlikely) name collision. create_new guarantees exclusive creation.
    let tmp = {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let pid = std::process::id();
        let fname = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "hermito".into());
        let mut attempt = 0u32;
        loop {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let tmp_name = format!("{}.tmp.{}.{}", fname, pid, n);
            let candidate = match target.parent() {
                Some(p) => p.join(&tmp_name),
                None => std::path::PathBuf::from(&tmp_name),
            };
            match std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&candidate)
            {
                Ok(mut f) => {
                    use std::io::Write;
                    f.write_all(content.as_bytes())?;
                    f.sync_all()?;
                    break candidate;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt < 1000 => {
                    attempt += 1;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    };
    let existed = target.exists();
    if existed {
        // best-effort preserve; still complete write on copy failure (data > metadata)
        let _ = copy_preserve_metadata(target, &tmp);
    }
    // Durable platform replace; NO set_owner_only (sensitive files use separate durable path).
    replace_and_sync_parent(&tmp, target)?;
    Ok(())
}

/// Copy exact permissions/ACL from existing target to the tmp so rename preserves them.
/// Unix: exact mode bits (no umask override).
/// Windows: copy security descriptor (owner/group/dacl/sacl) using cfg-gated windows-sys.
/// Only called when target existed.
fn copy_preserve_metadata(target: &std::path::Path, tmp: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let meta = std::fs::metadata(target)?;
        std::fs::set_permissions(tmp, meta.permissions())?;
    }
    #[cfg(windows)]
    {
        copy_windows_acl(target, tmp)?;
    }
    Ok(())
}

#[cfg(windows)]
fn copy_windows_acl(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use windows_sys::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HLOCAL};
    use windows_sys::Win32::Security::Authorization::{
        GetNamedSecurityInfoW, SetNamedSecurityInfoW, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, PSID, SACL_SECURITY_INFORMATION,
    };
    let from_w: Vec<u16> = from
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let to_w: Vec<u16> = to
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut owner: PSID = ptr::null_mut();
    let mut group: PSID = ptr::null_mut();
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut sacl: *mut ACL = ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let si = OWNER_SECURITY_INFORMATION
        | GROUP_SECURITY_INFORMATION
        | DACL_SECURITY_INFORMATION
        | SACL_SECURITY_INFORMATION;
    let err = unsafe {
        GetNamedSecurityInfoW(
            from_w.as_ptr(),
            SE_FILE_OBJECT,
            si,
            &mut owner,
            &mut group,
            &mut dacl,
            &mut sacl,
            &mut sd,
        )
    };
    if err != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(err as i32));
    }
    let set_res = unsafe {
        SetNamedSecurityInfoW(to_w.as_ptr(), SE_FILE_OBJECT, si, owner, group, dacl, sacl)
    };
    if !sd.is_null() {
        unsafe {
            LocalFree(sd as HLOCAL);
        }
    }
    if set_res != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(set_res as i32));
    }
    Ok(())
}
