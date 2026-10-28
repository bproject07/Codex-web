//! Windows application storage: private creation and explicit, scoped repair.
use crate::sharing_store::{Error, Result};
use fs2::FileExt;
use std::{
    fs::{File, OpenOptions},
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::Path,
    ptr::null_mut,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, LocalFree},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo,
        },
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetTokenInformation, IsWellKnownSid,
        OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, SECURITY_ATTRIBUTES,
        TOKEN_QUERY, TOKEN_USER, TokenUser, WinBuiltinAdministratorsSid, WinLocalSystemSid,
    },
    Storage::FileSystem::{
        CreateDirectoryW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

struct LocalAllocation(*mut std::ffi::c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}

fn sid_string(sid: *mut std::ffi::c_void) -> Result<String> {
    let mut text = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(Error("permissions_repair_failed"));
    }
    let _allocation = LocalAllocation(text.cast());
    let mut length = 0;
    // Windows returns a NUL-terminated SID string allocated by LocalAlloc.
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    Ok(String::from_utf16_lossy(unsafe {
        std::slice::from_raw_parts(text, length)
    }))
}

fn current_sid() -> Result<String> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(Error("permissions_repair_failed"));
    }
    let result = (|| {
        let mut size = 0;
        unsafe {
            GetTokenInformation(token, TokenUser, null_mut(), 0, &mut size);
        }
        if (size as usize) < std::mem::size_of::<TOKEN_USER>() || size > 65536 {
            return Err(Error("permissions_repair_failed"));
        }
        let mut storage = vec![0_u64; (size as usize).div_ceil(8)];
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                storage.as_mut_ptr().cast(),
                size,
                &mut size,
            )
        } == 0
        {
            return Err(Error("permissions_repair_failed"));
        }
        sid_string(unsafe { (*(storage.as_ptr().cast::<TOKEN_USER>())).User.Sid })
    })();
    unsafe {
        CloseHandle(token);
    }
    result
}

fn descriptor(user: &str) -> Result<LocalAllocation> {
    let sddl: Vec<u16> = format!("O:{user}D:P(A;OICI;FA;;;{user})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            null_mut(),
        )
    } == 0
    {
        return Err(Error("permissions_repair_failed"));
    }
    Ok(LocalAllocation(descriptor))
}

pub(crate) fn create_directory(path: &Path) -> std::io::Result<()> {
    let descriptor = current_sid()
        .and_then(|user| descriptor(&user))
        .map_err(std::io::Error::other)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe { CreateDirectoryW(name.as_ptr(), &attributes) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn trusted_owner(file: &File, user: &str) -> Result<()> {
    let mut owner = null_mut();
    let mut descriptor = null_mut();
    if unsafe {
        GetSecurityInfo(
            file.as_raw_handle() as HANDLE,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        )
    } != 0
    {
        return Err(Error("permissions_repair_failed"));
    }
    let _allocation = LocalAllocation(descriptor);
    if owner.is_null() {
        return Err(Error("permissions_owner_mismatch"));
    }
    if unsafe { IsWellKnownSid(owner, WinBuiltinAdministratorsSid) } != 0
        || unsafe { IsWellKnownSid(owner, WinLocalSystemSid) } != 0
        || sid_string(owner)? == user
    {
        Ok(())
    } else {
        Err(Error("permissions_owner_mismatch"))
    }
}

fn open_target(path: &Path, directory: bool, user: &str) -> Result<File> {
    // Denying delete sharing pins each inspected object against rename/reparse
    // substitution until every ACL update has completed.
    let file = OpenOptions::new()
        .access_mode(0x0002_0000 | 0x0004_0000 | 0x80)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .map_err(|_| Error("permissions_repair_failed"))?;
    let metadata = file
        .metadata()
        .map_err(|_| Error("permissions_repair_failed"))?;
    if metadata.file_attributes() & 0x400 != 0
        || metadata.is_dir() != directory
        || (!directory && !metadata.is_file())
    {
        return Err(Error("unsafe_state_file"));
    }
    trusted_owner(&file, user)?;
    Ok(file)
}

pub(crate) fn repair(directory: &Path) -> Result<()> {
    let user = current_sid()?;
    let root = open_target(directory, true, &user)?;
    let mut targets = vec![root];
    for name in [
        "sharing.sqlite3",
        "sharing.sqlite3-wal",
        "sharing.sqlite3-shm",
        "sharing.sqlite3.lock",
    ] {
        let path = directory.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => targets.push(open_target(&path, false, &user)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(Error("permissions_repair_failed")),
        }
    }
    let lease = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(directory.join("sharing.sqlite3.lock"))
        .map_err(|_| Error("permissions_repair_failed"))?;
    if lease
        .metadata()
        .map_err(|_| Error("permissions_repair_failed"))?
        .file_attributes()
        & 0x400
        != 0
    {
        return Err(Error("unsafe_state_file"));
    }
    lease
        .try_lock_exclusive()
        .map_err(|_| Error("sharing_store_in_use"))?;
    let descriptor = descriptor(&user)?;
    let mut present = 0;
    let mut defaulted = 0;
    let mut dacl = null_mut();
    if unsafe { GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted) }
        == 0
        || present == 0
        || dacl.is_null()
    {
        return Err(Error("permissions_repair_failed"));
    }
    for target in &targets {
        if unsafe {
            SetSecurityInfo(
                target.as_raw_handle() as HANDLE,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                dacl,
                null_mut(),
            )
        } != 0
        {
            return Err(Error("permissions_repair_failed"));
        }
    }
    // A newly created lease inherits the protected root ACL. Existing sidecars
    // were checked and repaired by handle; no file content was replaced.
    Ok(())
}
