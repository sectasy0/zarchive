//! Bounds native UnRAR test-mode output without creating archive-supplied paths.

use std::{
    fs::File,
    io::Write,
    path::Path,
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use unrar_sys as native;
use zeroize::Zeroizing;
const UCM_LARGE_DICTIONARY: native::UINT = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    Native(i32),
    MissingVolume,
    Limit,
    Cancelled,
    Write,
}

pub(crate) struct Header {
    pub name: String,
    pub size: u64,
    pub packed: u64,
    pub crc: Option<u32>,
    pub encrypted: bool,
    pub solid: bool,
    pub directory: bool,
    pub link: bool,
    pub dictionary: u32,
}

struct CallbackData {
    file: Option<File>,
    remaining: u64,
    input: Arc<AtomicU64>,
    failure: Option<Failure>,
    password: Zeroizing<Vec<native::WCHAR>>,
    password_utf8: Zeroizing<Vec<u8>>,
}

pub(crate) struct Reader {
    handle: NonNull<native::Handle>,
    callback: Box<CallbackData>,
    pub recovery_record: bool,
    pub solid: bool,
    pub open_warning: bool,
}

impl Drop for Reader {
    fn drop(&mut self) {
        unsafe {
            native::RARCloseArchive(self.handle.as_ptr());
        }
    }
}

impl Reader {
    pub(crate) fn open(
        path: &Path,
        password: Option<&str>,
        input: Arc<AtomicU64>,
    ) -> Result<Self, Failure> {
        crate::task_queue::checkpoint().map_err(|_| Failure::Cancelled)?;
        let size = path.metadata().map_err(|_| Failure::MissingVolume)?.len();
        input
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                left.checked_sub(size)
            })
            .map_err(|_| Failure::Limit)?;
        if password.is_some_and(|password| password.contains('\0')) {
            return Err(Failure::Native(native::ERAR_BAD_PASSWORD));
        }
        let mut password_utf8 = Zeroizing::new(password.unwrap_or("").as_bytes().to_vec());
        password_utf8.push(0);
        let mut callback = Box::new(CallbackData {
            file: None,
            remaining: 0,
            input,
            failure: None,
            password: Zeroizing::new(wide(password.unwrap_or(""))),
            password_utf8,
        });
        #[cfg(any(target_os = "linux", target_os = "netbsd"))]
        let name = {
            use std::os::unix::ffi::OsStrExt;
            std::ffi::CString::new(path.as_os_str().as_bytes())
                .map_err(|_| Failure::Native(native::ERAR_EOPEN))?
        };
        #[cfg(not(any(target_os = "linux", target_os = "netbsd")))]
        let name = wide_path(path)?;
        let mut data = native::OpenArchiveDataEx::new(name.as_ptr(), native::RAR_OM_EXTRACT);
        data.op_flags = native::ROADOF_KEEPBROKEN;
        data.callback = Some(callback_entry);
        data.user_data = (&mut *callback as *mut CallbackData) as native::LPARAM;
        let pointer = unsafe { native::RAROpenArchiveEx(&mut data) };
        let Some(handle) = NonNull::new(pointer as *mut native::Handle) else {
            return Err(callback
                .failure
                .unwrap_or(Failure::Native(data.open_result as i32)));
        };
        let reader = Self {
            handle,
            callback,
            recovery_record: data.flags & native::ROADF_RECOVERY != 0,
            solid: data.flags & native::ROADF_SOLID != 0,
            open_warning: data.open_result as i32 != native::ERAR_SUCCESS,
        };
        if data.open_result as i32 != native::ERAR_SUCCESS
            && data.open_result as i32 != native::ERAR_BAD_DATA
        {
            return Err(reader
                .callback
                .failure
                .unwrap_or(Failure::Native(data.open_result as i32)));
        }
        Ok(reader)
    }

    pub(crate) fn next(&mut self) -> Result<Option<Header>, Failure> {
        crate::task_queue::checkpoint().map_err(|_| Failure::Cancelled)?;
        self.callback.failure = None;
        let mut header = Box::new(native::HeaderDataEx::default());
        let code = unsafe { native::RARReadHeaderEx(self.handle.as_ptr(), &mut *header) };
        if let Some(error) = self.callback.failure {
            return Err(error);
        }
        if code == native::ERAR_END_ARCHIVE {
            return Ok(None);
        }
        if code != native::ERAR_SUCCESS {
            return Err(Failure::Native(code));
        }
        let name = if header.filename_w[0] != 0 {
            from_wide(&header.filename_w)
        } else {
            String::from_utf8_lossy(
                &header
                    .filename
                    .iter()
                    .take_while(|byte| **byte != 0)
                    .map(|byte| *byte as u8)
                    .collect::<Vec<_>>(),
            )
            .into_owned()
        };
        Ok(Some(Header {
            name,
            size: u64::from(header.unp_size) | (u64::from(header.unp_size_high) << 32),
            packed: u64::from(header.pack_size) | (u64::from(header.pack_size_high) << 32),
            crc: (header.hash_type == native::RAR_HASH_CRC32).then_some(header.file_crc),
            encrypted: header.flags & native::RHDF_ENCRYPTED != 0,
            solid: header.flags & native::RHDF_SOLID != 0,
            directory: header.flags & native::RHDF_DIRECTORY != 0,
            link: header.redir_type != 0 || header.file_attr & 0o170000 == 0o120000,
            dictionary: header.dict_size,
        }))
    }

    pub(crate) fn skip(&mut self) -> Result<(), Failure> {
        self.process(None, 0, native::RAR_SKIP)
    }

    pub(crate) fn read_to(&mut self, output: File, maximum: u64) -> Result<(), Failure> {
        self.process(Some(output), maximum, native::RAR_TEST)
    }

    fn process(
        &mut self,
        output: Option<File>,
        maximum: u64,
        operation: i32,
    ) -> Result<(), Failure> {
        crate::task_queue::checkpoint().map_err(|_| Failure::Cancelled)?;
        self.callback.file = output;
        self.callback.remaining = maximum;
        self.callback.failure = None;
        let code = unsafe {
            native::RARProcessFileW(
                self.handle.as_ptr(),
                operation,
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        if let Some(file) = self.callback.file.take() {
            if file.sync_all().is_err() {
                self.callback.failure = Some(Failure::Write);
            }
        }
        if let Some(error) = self.callback.failure {
            return Err(error);
        }
        if code == native::ERAR_SUCCESS {
            Ok(())
        } else {
            Err(Failure::Native(code))
        }
    }
}

fn wide(text: &str) -> Vec<native::WCHAR> {
    #[cfg(windows)]
    let mut result = text
        .encode_utf16()
        .map(|value| value as native::WCHAR)
        .collect::<Vec<_>>();
    #[cfg(not(windows))]
    let mut result = text
        .chars()
        .map(|value| value as native::WCHAR)
        .collect::<Vec<_>>();
    result.push(0);
    result
}

#[cfg(not(any(target_os = "linux", target_os = "netbsd")))]
fn wide_path(path: &Path) -> Result<Vec<native::WCHAR>, Failure> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let mut result = path
            .as_os_str()
            .encode_wide()
            .map(|value| value as native::WCHAR)
            .collect::<Vec<_>>();
        if result.contains(&0) {
            return Err(Failure::Native(native::ERAR_EOPEN));
        }
        result.push(0);
        Ok(result)
    }
    #[cfg(not(windows))]
    {
        let text = path
            .to_str()
            .filter(|text| !text.contains('\0'))
            .ok_or(Failure::Native(native::ERAR_EOPEN))?;
        Ok(wide(text))
    }
}

fn from_wide(bytes: &[native::WCHAR]) -> String {
    #[cfg(windows)]
    {
        String::from_utf16_lossy(
            &bytes
                .iter()
                .copied()
                .take_while(|value| *value != 0)
                .map(|value| value as u16)
                .collect::<Vec<_>>(),
        )
    }
    #[cfg(not(windows))]
    {
        bytes
            .iter()
            .copied()
            .take_while(|value| *value != 0)
            .map(|value| char::from_u32(value as u32).unwrap_or('\u{fffd}'))
            .collect()
    }
}

extern "C" fn callback_entry(
    message: native::UINT,
    data: native::LPARAM,
    first: native::LPARAM,
    second: native::LPARAM,
) -> i32 {
    if data == 0 {
        return -1;
    }
    let state = unsafe { &mut *(data as *mut CallbackData) };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        callback(state, message, first, second)
    })) {
        Ok(result) => result,
        Err(_) => {
            state.failure = Some(Failure::Cancelled);
            -1
        }
    }
}

fn callback(
    state: &mut CallbackData,
    message: native::UINT,
    first: native::LPARAM,
    second: native::LPARAM,
) -> i32 {
    if crate::task_queue::checkpoint().is_err() {
        state.failure = Some(Failure::Cancelled);
        return -1;
    }
    match message {
        native::UCM_PROCESSDATA => {
            if second < 0 || second as u64 > 16 * 1024 * 1024 || first == 0 {
                state.failure = Some(Failure::Limit);
                return -1;
            }
            let Some(file) = state.file.as_mut() else {
                return 0;
            };
            let requested = second as usize;
            let count = (requested as u64).min(state.remaining) as usize;
            let bytes = unsafe { std::slice::from_raw_parts(first as *const u8, count) };
            if file.write_all(bytes).is_err() {
                state.failure = Some(Failure::Write);
                return -1;
            }
            state.remaining -= count as u64;
            if count != requested {
                state.failure = Some(Failure::Limit);
                return -1;
            }
            0
        }
        native::UCM_NEEDPASSWORDW => {
            if first == 0
                || state.password.len() <= 1
                || second <= 0
                || state.password.len() > second as usize
            {
                state.failure = Some(Failure::Native(native::ERAR_MISSING_PASSWORD));
                return -1;
            }
            unsafe {
                std::ptr::copy_nonoverlapping(
                    state.password.as_ptr(),
                    first as *mut native::WCHAR,
                    state.password.len(),
                );
            }
            0
        }
        native::UCM_NEEDPASSWORD => {
            if first == 0
                || state.password_utf8.len() <= 1
                || second <= 0
                || state.password_utf8.len() > second as usize
            {
                state.failure = Some(Failure::Native(native::ERAR_MISSING_PASSWORD));
                return -1;
            }
            unsafe {
                std::ptr::copy_nonoverlapping(
                    state.password_utf8.as_ptr(),
                    first as *mut u8,
                    state.password_utf8.len(),
                );
            }
            0
        }
        native::UCM_CHANGEVOLUMEW => {
            if second == native::RAR_VOL_ASK {
                state.failure = Some(Failure::MissingVolume);
                return -1;
            }
            if first == 0 {
                state.failure = Some(Failure::MissingVolume);
                return -1;
            }
            let mut name = Vec::new();
            for index in 0..2048 {
                let character = unsafe { *(first as *const native::WCHAR).add(index) };
                if character == 0 {
                    break;
                }
                name.push(character);
            }
            let size = std::fs::metadata(from_wide(&name))
                .map(|metadata| metadata.len())
                .unwrap_or(u64::MAX);
            if state
                .input
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                    left.checked_sub(size)
                })
                .is_err()
            {
                state.failure = Some(Failure::Limit);
                return -1;
            }
            0
        }
        native::UCM_CHANGEVOLUME if second == native::RAR_VOL_ASK => {
            state.failure = Some(Failure::MissingVolume);
            -1
        }
        UCM_LARGE_DICTIONARY => {
            state.failure = Some(Failure::Limit);
            -1
        }
        _ => 0,
    }
}

#[cfg(test)]
pub(crate) fn stored_fixture(entries: &[(&str, &[u8])]) -> Vec<u8> {
    fn block(kind: u8, flags: u16, data: &[u8]) -> Vec<u8> {
        let mut header = vec![0, 0, kind];
        header.extend_from_slice(&flags.to_le_bytes());
        header.extend_from_slice(&((7 + data.len()) as u16).to_le_bytes());
        header.extend_from_slice(data);
        let crc = crc32fast::hash(&header[2..]) as u16;
        header[..2].copy_from_slice(&crc.to_le_bytes());
        header
    }
    let mut bytes = b"Rar!\x1a\x07\0".to_vec();
    bytes.extend_from_slice(&block(0x73, 0, &[0; 6]));
    for (name, data) in entries {
        let mut header = Vec::new();
        header.extend_from_slice(&(data.len() as u32).to_le_bytes());
        header.extend_from_slice(&(data.len() as u32).to_le_bytes());
        header.push(3);
        header.extend_from_slice(&crc32fast::hash(data).to_le_bytes());
        header.extend_from_slice(&0u32.to_le_bytes());
        header.extend_from_slice(&[20, 0x30]);
        header.extend_from_slice(&(name.len() as u16).to_le_bytes());
        header.extend_from_slice(&0o100600u32.to_le_bytes());
        header.extend_from_slice(name.as_bytes());
        bytes.extend_from_slice(&block(0x74, 0x8000, &header));
        bytes.extend_from_slice(data);
    }
    bytes.extend_from_slice(&block(0x7b, 0, &[]));
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_rar_reader_keeps_damaged_bytes_and_continues_without_extracting_paths() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("odzysk-日本.rar");
        let mut bytes = stored_fixture(&[
            ("../first.txt", b"first payload"),
            ("bad.txt", b"corrupt payload"),
            ("last.txt", b"last payload"),
        ]);
        let damage = memchr::memmem::find(&bytes, b"corrupt payload").unwrap();
        bytes[damage] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        let mut reader = Reader::open(&path, None, Arc::new(AtomicU64::new(1024 * 1024))).unwrap();
        let mut outputs = Vec::new();
        while let Some(header) = reader.next().unwrap() {
            let output = root.path().join(format!("data-{}", outputs.len()));
            let result = reader.read_to(File::create(&output).unwrap(), header.size);
            outputs.push((header.name, result, std::fs::read(output).unwrap()));
        }
        assert_eq!(outputs.len(), 3);
        assert!(outputs[0].1.is_ok());
        assert_eq!(outputs[1].1, Err(Failure::Native(native::ERAR_BAD_DATA)));
        assert_eq!(outputs[1].2.len(), b"corrupt payload".len());
        assert!(outputs[2].1.is_ok());
        assert!(!root.path().join("first.txt").exists());
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn native_rar_callback_stops_at_output_limit_and_reports_missing_volumes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("limited.rar");
        std::fs::write(&path, stored_fixture(&[("file.txt", b"bounded payload")])).unwrap();
        let mut reader = Reader::open(&path, None, Arc::new(AtomicU64::new(1024 * 1024))).unwrap();
        reader.next().unwrap().unwrap();
        let output = root.path().join("limited.data");
        assert_eq!(
            reader.read_to(File::create(&output).unwrap(), 4),
            Err(Failure::Limit)
        );
        assert_eq!(std::fs::read(&output).unwrap(), b"boun");
        let mut state = CallbackData {
            file: None,
            remaining: 0,
            input: Arc::new(AtomicU64::new(1024)),
            failure: None,
            password: Zeroizing::new(wide("")),
            password_utf8: Zeroizing::new(vec![0]),
        };
        assert_eq!(
            callback(
                &mut state,
                native::UCM_CHANGEVOLUMEW,
                0,
                native::RAR_VOL_ASK
            ),
            -1
        );
        assert_eq!(state.failure, Some(Failure::MissingVolume));
        assert_eq!(callback(&mut state, native::UCM_NEEDPASSWORDW, 0, 0), -1);
        assert_eq!(
            state.failure,
            Some(Failure::Native(native::ERAR_MISSING_PASSWORD))
        );
    }

    #[test]
    fn native_rar_recovery_fits_platform_worker_stack() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("worker.rar");
        std::fs::write(
            &path,
            stored_fixture(&[("file.txt", b"worker RAR payload")]),
        )
        .unwrap();
        let output = root.path().join("output.data");
        let file = File::create(&output).unwrap();
        std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || {
                let mut reader =
                    Reader::open(&path, None, Arc::new(AtomicU64::new(1024 * 1024))).unwrap();
                let header = reader.next().unwrap().unwrap();
                reader.read_to(file, header.size).unwrap();
                assert!(reader.next().unwrap().is_none());
            })
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(std::fs::read(output).unwrap(), b"worker RAR payload");
    }
}
