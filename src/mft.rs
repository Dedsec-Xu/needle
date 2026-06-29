//! NTFS MFT enumeration and USN Journal incremental updates.
//!
//! Same principle as Everything:
//! 1. Open the volume device `\\.\C:` for read (requires admin).
//! 2. `FSCTL_ENUM_USN_DATA` enumerates the whole MFT in one pass, yielding each
//!    file/dir's FileReferenceNumber (FRN), ParentFRN, name, and attributes.
//! 3. Rebuild full paths in memory by walking the FRN -> ParentFRN chain.
//! 4. `FSCTL_READ_USN_JOURNAL` reads the change journal for incremental updates.

use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::mem::size_of;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, ERROR_HANDLE_EOF, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, FILE_ATTRIBUTE_DIRECTORY,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_READ, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows::Win32::System::Ioctl::{
    FSCTL_ENUM_USN_DATA, FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL, MFT_ENUM_DATA_V0,
    READ_USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V0, USN_REASON_FILE_CREATE, USN_REASON_FILE_DELETE,
    USN_REASON_RENAME_NEW_NAME,
};
use windows::Win32::System::IO::DeviceIoControl;

/// Fixed FRN of the NTFS root directory.
const ROOT_FRN: u64 = 0x0005_0000_0000_0005;

/// One filesystem record (filename metadata only, no content).
#[derive(Clone, Debug)]
pub struct Entry {
    pub parent: u64,
    pub name: String,
    pub is_dir: bool,
}

/// In-memory index: FRN -> Entry.
#[derive(Default)]
pub struct Index {
    pub drive: char,
    pub entries: HashMap<u64, Entry>,
    /// Inverted index: lowercased extension (without dot) -> FRNs with that
    /// extension. This is what lets `*.rs` hit only the ~hundreds of matching
    /// entries instead of scanning all millions.
    pub by_ext: HashMap<String, Vec<u64>>,
    /// JournalId / NextUsn from QueryUsnJournal, used for incremental reads.
    pub journal_id: u64,
    pub next_usn: i64,
}

/// Lowercased extension of a filename (without the dot), if it has one.
/// Dotfiles like `.gitignore` (leading dot, nothing before it) yield `None`.
pub fn ext_of(name: &str) -> Option<String> {
    let p = name.rfind('.')?;
    if p == 0 || p + 1 >= name.len() {
        return None;
    }
    Some(name[p + 1..].to_ascii_lowercase())
}

/// Raw USN_RECORD_V2 header (FileName follows, located via offset/length).
#[repr(C)]
struct UsnRecordV2 {
    record_length: u32,
    major_version: u16,
    minor_version: u16,
    file_reference_number: u64,
    parent_file_reference_number: u64,
    usn: i64,
    timestamp: i64,
    reason: u32,
    source_info: u32,
    security_id: u32,
    file_attributes: u32,
    file_name_length: u16,
    file_name_offset: u16,
    // WCHAR file_name[1] ...
}

/// Open a drive letter as a raw volume, e.g. 'C'. Requires admin.
fn open_volume(drive: char) -> Result<HANDLE> {
    let path: Vec<u16> = format!("\\\\.\\{}:", drive.to_ascii_uppercase())
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let handle = unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            FILE_GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }
    .with_context(|| {
        format!(
            "failed to open volume \\\\.\\{}: (needs admin privileges, must be an NTFS volume)",
            drive
        )
    })?;
    Ok(handle)
}

/// Enumerate all NTFS volumes on the machine (drive letters), so an empty-root
/// query can fan out across the whole machine instead of a single drive. Only
/// fixed/removable drives whose filesystem reports `NTFS` are returned; network,
/// CD-ROM, and non-NTFS volumes are skipped (the MFT/USN approach can't index
/// them). Results are sorted for deterministic ordering.
pub fn ntfs_drives() -> Vec<char> {
    let mask = unsafe { GetLogicalDrives() };
    let mut drives = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        if is_ntfs(letter) {
            drives.push(letter);
        }
    }
    drives.sort_unstable();
    drives
}

/// True if `drive` is a fixed/removable volume formatted NTFS.
fn is_ntfs(drive: char) -> bool {
    let root: Vec<u16> = format!("{}:\\", drive)
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // Skip network/CD/unknown drive types up front; they're never NTFS-indexable.
    // GetDriveTypeW returns DRIVE_REMOVABLE (2) or DRIVE_FIXED (3) for the volumes
    // we can index; everything else (network, CD-ROM, RAM disk, no root) is out.
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    let dtype = unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) };
    if dtype != DRIVE_FIXED && dtype != DRIVE_REMOVABLE {
        return false;
    }
    let mut fs_name = [0u16; 16];
    let ok = unsafe {
        GetVolumeInformationW(
            PCWSTR(root.as_ptr()),
            None,
            None,
            None,
            None,
            Some(&mut fs_name),
        )
    };
    if ok.is_err() {
        return false;
    }
    let end = fs_name
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(fs_name.len());
    String::from_utf16_lossy(&fs_name[..end]).eq_ignore_ascii_case("NTFS")
}

/// Parse a USN record buffer, invoking the callback for each record.
unsafe fn parse_usn_records(buf: &[u8], mut on_record: impl FnMut(&UsnRecordV2, &str)) {
    let mut offset = 0usize;
    while offset + size_of::<UsnRecordV2>() <= buf.len() {
        let rec = &*(buf.as_ptr().add(offset) as *const UsnRecordV2);
        let len = rec.record_length as usize;
        if len == 0 || offset + len > buf.len() {
            break;
        }
        // Only handle V2 records (major_version == 2).
        if rec.major_version == 2 {
            let name_off = rec.file_name_offset as usize;
            let name_len = rec.file_name_length as usize;
            if name_off + name_len <= len {
                let name_bytes = &buf[offset + name_off..offset + name_off + name_len];
                let wide: &[u16] =
                    std::slice::from_raw_parts(name_bytes.as_ptr() as *const u16, name_len / 2);
                let name = String::from_utf16_lossy(wide);
                on_record(rec, &name);
            }
        }
        offset += len;
    }
}

/// Full enumeration of the MFT, building the index.
pub fn build_index(drive: char) -> Result<Index> {
    let handle = open_volume(drive)?;
    let result = build_index_inner(handle, drive);
    unsafe {
        let _ = CloseHandle(handle);
    }
    result
}

fn build_index_inner(handle: HANDLE, drive: char) -> Result<Index> {
    let mut index = Index {
        drive,
        ..Default::default()
    };

    // Query journal state first; record starting NextUsn for later incremental reads.
    let mut jdata = USN_JOURNAL_DATA_V0::default();
    let mut returned = 0u32;
    unsafe {
        DeviceIoControl(
            handle,
            FSCTL_QUERY_USN_JOURNAL,
            None,
            0,
            Some(&mut jdata as *mut _ as *mut _),
            size_of::<USN_JOURNAL_DATA_V0>() as u32,
            Some(&mut returned),
            None,
        )
        .context("FSCTL_QUERY_USN_JOURNAL failed (volume may not have USN Journal enabled)")?;
    }
    index.journal_id = jdata.UsnJournalID;
    index.next_usn = jdata.NextUsn;

    // Enumerate the entire MFT.
    let mut enum_data = MFT_ENUM_DATA_V0 {
        StartFileReferenceNumber: 0,
        LowUsn: 0,
        HighUsn: i64::MAX,
    };
    // 1 MiB buffer; first 8 bytes hold the next StartFRN.
    let mut buffer = vec![0u8; 1 << 20];

    loop {
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_ENUM_USN_DATA,
                Some(&enum_data as *const _ as *const _),
                size_of::<MFT_ENUM_DATA_V0>() as u32,
                Some(buffer.as_mut_ptr() as *mut _),
                buffer.len() as u32,
                Some(&mut returned),
                None,
            )
        };
        if let Err(e) = ok {
            // Normal termination: enumeration returns ERROR_HANDLE_EOF at the end.
            if e.code() == ERROR_HANDLE_EOF.to_hresult() {
                break;
            }
            return Err(anyhow!("FSCTL_ENUM_USN_DATA failed: {e}"));
        }
        if returned < 8 {
            break;
        }
        // First 8 bytes = next round's start FRN.
        enum_data.StartFileReferenceNumber = u64::from_le_bytes(buffer[0..8].try_into().unwrap());

        let records = &buffer[8..returned as usize];
        unsafe {
            parse_usn_records(records, |rec, name| {
                index.add_indexed(
                    rec.file_reference_number,
                    rec.parent_file_reference_number,
                    name.to_string(),
                    rec.file_attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0,
                );
            });
        }
    }

    Ok(index)
}

impl Index {
    /// Insert an entry and update the extension index.
    pub fn add_indexed(&mut self, frn: u64, parent: u64, name: String, is_dir: bool) {
        if let Some(ext) = ext_of(&name) {
            self.by_ext.entry(ext).or_default().push(frn);
        }
        self.entries.insert(
            frn,
            Entry {
                parent,
                name,
                is_dir,
            },
        );
    }

    /// Remove an entry (and its extension-index slot) given its known name.
    pub fn remove_indexed(&mut self, frn: u64, name: &str) {
        if let Some(ext) = ext_of(name) {
            if let Some(v) = self.by_ext.get_mut(&ext) {
                v.retain(|&x| x != frn);
            }
        }
        self.entries.remove(&frn);
    }

    /// Rebuild the full path of a record, e.g. `C:\Users\foo\bar.txt`.
    /// Guards against cyclic references (corrupt data): walks up at most 256 levels.
    pub fn full_path(&self, frn: u64) -> Option<String> {
        let mut parts: Vec<&str> = Vec::new();
        let mut cur = frn;
        for _ in 0..256 {
            if cur == ROOT_FRN {
                break;
            }
            let e = self.entries.get(&cur)?;
            parts.push(&e.name);
            if e.parent == cur {
                break;
            }
            cur = e.parent;
        }
        parts.reverse();
        let mut path = format!("{}:\\", self.drive.to_ascii_uppercase());
        path.push_str(&parts.join("\\"));
        Some(path)
    }

    /// Read the USN Journal for incremental updates.
    ///
    /// The journal can be deleted/recreated (e.g. `fsutil usn deletejournal`, a
    /// `chkdsk`, or a wrap-around), after which our cached `journal_id` and
    /// `next_usn` no longer refer to anything real and incremental reads would
    /// either fail or silently miss changes. We detect that by re-querying the
    /// journal header first: if the `UsnJournalID` changed, the index is stale
    /// and the caller must rebuild it from the MFT. Returns `Stale` in that case,
    /// otherwise `Updated(records_processed)`.
    pub fn apply_journal_updates(&mut self, drive: char) -> Result<JournalOutcome> {
        let handle = open_volume(drive)?;
        let res = self.apply_journal_inner(handle);
        unsafe {
            let _ = CloseHandle(handle);
        }
        res
    }

    fn apply_journal_inner(&mut self, handle: HANDLE) -> Result<JournalOutcome> {
        // Re-query the journal header; a changed/zeroed JournalID means our cached
        // cursor is invalid and the whole index must be rebuilt from the MFT.
        let mut jdata = USN_JOURNAL_DATA_V0::default();
        let mut jret = 0u32;
        let q = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_QUERY_USN_JOURNAL,
                None,
                0,
                Some(&mut jdata as *mut _ as *mut _),
                size_of::<USN_JOURNAL_DATA_V0>() as u32,
                Some(&mut jret),
                None,
            )
        };
        // Journal gone entirely, or recreated under a new id -> stale.
        if q.is_err() || jdata.UsnJournalID != self.journal_id {
            return Ok(JournalOutcome::Stale);
        }
        // If our cursor predates the journal's oldest retained record, we've lost
        // updates (wrap-around / truncation) and must rebuild rather than skip.
        if self.next_usn < jdata.LowestValidUsn {
            return Ok(JournalOutcome::Stale);
        }

        let mut read = READ_USN_JOURNAL_DATA_V0 {
            StartUsn: self.next_usn,
            ReasonMask: u32::MAX,
            ReturnOnlyOnClose: 0,
            Timeout: 0,
            BytesToWaitFor: 0,
            UsnJournalID: self.journal_id,
        };
        let mut buffer = vec![0u8; 1 << 20];
        let mut count = 0usize;

        loop {
            let mut returned = 0u32;
            let ok = unsafe {
                DeviceIoControl(
                    handle,
                    FSCTL_READ_USN_JOURNAL,
                    Some(&read as *const _ as *const _),
                    size_of::<READ_USN_JOURNAL_DATA_V0>() as u32,
                    Some(buffer.as_mut_ptr() as *mut _),
                    buffer.len() as u32,
                    Some(&mut returned),
                    None,
                )
            };
            if ok.is_err() || returned < 8 {
                break;
            }
            // First 8 bytes = next round's StartUsn.
            let next = i64::from_le_bytes(buffer[0..8].try_into().unwrap());
            let records = &buffer[8..returned as usize];

            // (frn, parent, name, is_dir, is_delete). The name is carried for
            // deletes too, since removing from the extension index needs it.
            let mut updates: Vec<(u64, u64, String, bool, bool)> = Vec::new();
            unsafe {
                parse_usn_records(records, |rec, name| {
                    let frn = rec.file_reference_number;
                    let is_dir = rec.file_attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
                    if rec.reason & USN_REASON_FILE_DELETE != 0 {
                        updates.push((frn, 0, name.to_string(), is_dir, true));
                    } else if rec.reason & (USN_REASON_FILE_CREATE | USN_REASON_RENAME_NEW_NAME)
                        != 0
                    {
                        updates.push((
                            frn,
                            rec.parent_file_reference_number,
                            name.to_string(),
                            is_dir,
                            false,
                        ));
                    }
                    count += 1;
                });
            }
            for (frn, parent, name, is_dir, is_delete) in updates {
                if is_delete {
                    self.remove_indexed(frn, &name);
                } else {
                    self.add_indexed(frn, parent, name, is_dir);
                }
            }

            if next == self.next_usn {
                break;
            }
            self.next_usn = next;
            read.StartUsn = next;
        }
        Ok(JournalOutcome::Updated(count))
    }
}

/// Result of an incremental journal refresh.
pub enum JournalOutcome {
    /// Journal was valid; `usize` records were processed.
    Updated(usize),
    /// Journal id changed or our cursor fell off the retained window — the
    /// in-memory index is stale and must be rebuilt from the MFT.
    Stale,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ext_of_basics() {
        assert_eq!(ext_of("main.rs").as_deref(), Some("rs"));
        assert_eq!(ext_of("Archive.TAR").as_deref(), Some("tar"));
        assert_eq!(ext_of("a.b.c").as_deref(), Some("c"));
        assert_eq!(ext_of("noext"), None);
        assert_eq!(ext_of(".gitignore"), None); // leading dot only
        assert_eq!(ext_of("trailingdot."), None);
    }

    #[test]
    fn full_path_walks_parents_to_root() {
        let mut idx = Index {
            drive: 'C',
            ..Default::default()
        };
        // ROOT <- foo(10) <- bar.txt(11)
        idx.add_indexed(10, ROOT_FRN, "foo".into(), true);
        idx.add_indexed(11, 10, "bar.txt".into(), false);
        assert_eq!(idx.full_path(11).as_deref(), Some("C:\\foo\\bar.txt"));
        assert_eq!(idx.full_path(10).as_deref(), Some("C:\\foo"));
    }

    #[test]
    fn add_and_remove_maintain_ext_index() {
        let mut idx = Index {
            drive: 'C',
            ..Default::default()
        };
        idx.add_indexed(10, ROOT_FRN, "lib.rs".into(), false);
        assert_eq!(idx.by_ext.get("rs").map(|v| v.len()), Some(1));
        idx.remove_indexed(10, "lib.rs");
        assert_eq!(idx.by_ext.get("rs").map(|v| v.len()), Some(0));
        assert!(!idx.entries.contains_key(&10));
    }
}
