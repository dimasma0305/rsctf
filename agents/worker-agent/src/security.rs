use std::path::Path;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SecurityError {
    #[error("worker state security check failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("worker state security task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

pub async fn prepare_state_dir(
    path: &Path,
    windows_service_account: Option<&str>,
    unix_service_uid: Option<u32>,
) -> Result<(), SecurityError> {
    tokio::fs::create_dir_all(path).await?;
    secure_platform_state(path, windows_service_account, unix_service_uid, true).await
}

pub async fn verify_state_dir(path: &Path) -> Result<(), SecurityError> {
    secure_platform_state(path, None, None, false).await
}

pub async fn verify_state_file(path: &Path) -> Result<(), SecurityError> {
    verify_platform_file(path).await
}

#[cfg(unix)]
pub fn transfer_state_file(
    path: &Path,
    unix_service_uid: Option<u32>,
) -> Result<(), SecurityError> {
    if let Some(uid) = unix_service_uid {
        chown_path(path, uid)?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn transfer_state_file(
    _path: &Path,
    _unix_service_uid: Option<u32>,
) -> Result<(), SecurityError> {
    Ok(())
}

pub fn acquire_state_lock(path: &Path) -> Result<std::fs::File, SecurityError> {
    use fs2::FileExt;

    let lock_path = path.join(".agent.lock");
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(lock_path)?;
    file.try_lock_exclusive().map_err(|error| {
        SecurityError::Io(std::io::Error::new(
            error.kind(),
            format!("another worker agent owns this state directory: {error}"),
        ))
    })?;
    Ok(file)
}

#[cfg(unix)]
async fn secure_platform_state(
    path: &Path,
    _windows_service_account: Option<&str>,
    unix_service_uid: Option<u32>,
    reset: bool,
) -> Result<(), SecurityError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = tokio::fs::symlink_metadata(path).await?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(permission_denied(
            "state path must be a real directory, not a symlink",
        ));
    }
    // SAFETY: geteuid has no preconditions and does not dereference memory.
    let current_uid = unsafe { libc::geteuid() };
    let expected_uid = unix_service_uid.unwrap_or(current_uid);
    if metadata.uid() != current_uid && metadata.uid() != expected_uid {
        return Err(permission_denied(
            "state directory is owned by an unexpected Unix account",
        ));
    }
    if reset {
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
        if expected_uid != metadata.uid() {
            if current_uid != 0 {
                return Err(permission_denied(
                    "only root can transfer worker state to another Unix UID",
                ));
            }
            chown_path(path, expected_uid)?;
        }
    }
    let metadata = tokio::fs::symlink_metadata(path).await?;
    if metadata.uid() != expected_uid || metadata.mode() & 0o077 != 0 {
        return Err(permission_denied(
            "state directory must be owned by the service UID with mode 0700",
        ));
    }
    Ok(())
}

#[cfg(unix)]
async fn verify_platform_file(path: &Path) -> Result<(), SecurityError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = tokio::fs::symlink_metadata(path).await?;
    // SAFETY: geteuid has no preconditions and does not dereference memory.
    let current_uid = unsafe { libc::geteuid() };
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != current_uid
        || metadata.mode() & 0o077 != 0
    {
        return Err(permission_denied(
            "worker state files must be regular, non-symlink, service-owned files without group/world permissions",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn chown_path(path: &Path, uid: u32) -> Result<(), SecurityError> {
    use std::os::unix::ffi::OsStrExt;

    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        SecurityError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "state path contains a NUL byte",
        ))
    })?;
    // SAFETY: the CString is NUL-terminated and remains alive for the call;
    // gid=-1 preserves the existing group.
    if unsafe { libc::chown(path.as_ptr(), uid, u32::MAX) } != 0 {
        return Err(SecurityError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(windows)]
async fn secure_platform_state(
    path: &Path,
    windows_service_account: Option<&str>,
    _unix_service_uid: Option<u32>,
    reset: bool,
) -> Result<(), SecurityError> {
    const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$path = $env:RSCTF_ACL_PATH
$account = $env:RSCTF_ACL_ACCOUNT
if ([string]::IsNullOrWhiteSpace($account)) {
  $account = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
}
if ($account -match '^S-1-[0-9-]+$') {
  $accountSid = New-Object System.Security.Principal.SecurityIdentifier($account)
} else {
  $accountSid = (New-Object System.Security.Principal.NTAccount($account)).Translate([System.Security.Principal.SecurityIdentifier])
}
$systemSid = New-Object System.Security.Principal.SecurityIdentifier('S-1-5-18')
$adminSid = New-Object System.Security.Principal.SecurityIdentifier('S-1-5-32-544')
if ($env:RSCTF_ACL_RESET -eq '1') {
  $acl = New-Object System.Security.AccessControl.DirectorySecurity
  $acl.SetAccessRuleProtection($true, $false)
  $acl.SetOwner($accountSid)
  $inherit = [System.Security.AccessControl.InheritanceFlags]::ContainerInherit -bor [System.Security.AccessControl.InheritanceFlags]::ObjectInherit
  foreach ($sid in @($accountSid, $systemSid, $adminSid)) {
    $rule = New-Object System.Security.AccessControl.FileSystemAccessRule($sid, 'FullControl', $inherit, 'None', 'Allow')
    [void]$acl.AddAccessRule($rule)
  }
  [System.IO.Directory]::SetAccessControl($path, $acl)
}
$check = [System.IO.Directory]::GetAccessControl($path)
if (-not $check.AreAccessRulesProtected) { throw 'state directory still inherits ACLs' }
if ($check.GetOwner([System.Security.Principal.SecurityIdentifier]).Value -ne $accountSid.Value) { throw 'state directory has an unexpected owner' }
$allowed = @($accountSid.Value, $systemSid.Value, $adminSid.Value)
$present = @{}
foreach ($rule in $check.Access) {
  if ($rule.AccessControlType -ne 'Allow') { continue }
  $sid = $rule.IdentityReference.Translate([System.Security.Principal.SecurityIdentifier]).Value
  if ($allowed -notcontains $sid) { throw "unexpected ACL principal $sid" }
  $present[$sid] = $true
}
foreach ($sid in $allowed) {
  if (-not $present.ContainsKey($sid)) { throw "required ACL principal $sid is missing" }
}
"#;

    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    let metadata = tokio::fs::symlink_metadata(path).await?;
    if !metadata.file_type().is_dir()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(permission_denied(
            "state path must be a real directory, not a reparse point",
        ));
    }

    let path = path.to_owned();
    let account = windows_service_account.unwrap_or_default().to_string();
    tokio::task::spawn_blocking(move || {
        let status = std::process::Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                SCRIPT,
            ])
            .env("RSCTF_ACL_PATH", &path)
            .env("RSCTF_ACL_ACCOUNT", account)
            .env("RSCTF_ACL_RESET", if reset { "1" } else { "0" })
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("PowerShell ACL verification exited with {status}"),
            ))
        }
    })
    .await??;
    Ok(())
}

#[cfg(windows)]
async fn verify_platform_file(path: &Path) -> Result<(), SecurityError> {
    const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$path = $env:RSCTF_ACL_PATH
$currentSid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
$systemSid = New-Object System.Security.Principal.SecurityIdentifier('S-1-5-18')
$adminSid = New-Object System.Security.Principal.SecurityIdentifier('S-1-5-32-544')
$allowed = @($currentSid.Value, $systemSid.Value, $adminSid.Value) | Select-Object -Unique
$present = @{}
$acl = [System.IO.File]::GetAccessControl($path)
foreach ($rule in $acl.Access) {
  if ($rule.AccessControlType -ne 'Allow') { continue }
  $sid = $rule.IdentityReference.Translate([System.Security.Principal.SecurityIdentifier]).Value
  if ($allowed -notcontains $sid) { throw "unexpected file ACL principal $sid" }
  if (($rule.FileSystemRights -band [System.Security.AccessControl.FileSystemRights]::FullControl) -eq [System.Security.AccessControl.FileSystemRights]::FullControl) {
    $present[$sid] = $true
  }
}
foreach ($sid in $allowed) {
  if (-not $present.ContainsKey($sid)) { throw "required full-control file ACL for $sid is missing" }
}
"#;

    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

    let metadata = tokio::fs::symlink_metadata(path).await?;
    if !metadata.file_type().is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(permission_denied(
            "worker state file must be a regular file, not a reparse point",
        ));
    }
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let status = std::process::Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                SCRIPT,
            ])
            .env("RSCTF_ACL_PATH", &path)
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("PowerShell file ACL verification exited with {status}"),
            ))
        }
    })
    .await??;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
async fn secure_platform_state(
    _path: &Path,
    _windows_service_account: Option<&str>,
    _unix_service_uid: Option<u32>,
    _reset: bool,
) -> Result<(), SecurityError> {
    Ok(())
}

#[cfg(not(any(unix, windows)))]
async fn verify_platform_file(_path: &Path) -> Result<(), SecurityError> {
    Ok(())
}

fn permission_denied(message: &'static str) -> SecurityError {
    SecurityError::Io(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        message,
    ))
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use uuid::Uuid;

    #[tokio::test]
    async fn prepared_directory_passes_acl_verification() {
        let path = std::env::temp_dir().join(format!("rsctf-worker-acl-{}", Uuid::new_v4()));
        prepare_state_dir(&path, None, None).await.unwrap();
        verify_state_dir(&path).await.unwrap();
        let state_file = path.join("worker.json");
        tokio::fs::write(&state_file, b"{}").await.unwrap();
        verify_state_file(&state_file).await.unwrap();
        let _ = tokio::fs::remove_dir_all(path).await;
    }
}
