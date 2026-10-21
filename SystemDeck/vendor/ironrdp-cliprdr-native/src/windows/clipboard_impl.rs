use core::ptr::with_exposed_provenance_mut;
use core::time::Duration;
use std::collections::HashSet;
use std::io::Write as _;
use std::os::windows::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use ironrdp_cliprdr::backend::{ClipboardMessage, ClipboardMessageProxy};
use ironrdp_cliprdr::pdu::{
    ClipboardFileAttributes, ClipboardFormat, ClipboardFormatId, ClipboardFormatName, ClipboardGeneralCapabilityFlags,
    FileContentsFlags, FileContentsRequest, FileContentsResponse, FileDescriptor, FormatDataRequest, FormatDataResponse,
};
use tracing::warn;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::GetClipboardOwner;
use windows::Win32::UI::Shell::DefSubclassProc;
use windows::Win32::UI::WindowsAndMessaging::{
    KillTimer, SetTimer, WA_INACTIVE, WM_ACTIVATE, WM_ACTIVATEAPP, WM_CLIPBOARDUPDATE, WM_DESTROY, WM_RENDERALLFORMATS,
    WM_RENDERFORMAT, WM_TIMER,
};

use crate::windows::clipboard_data_ref::ClipboardDataRef;
use crate::windows::os_clipboard::OwnedOsClipboard;
use crate::windows::remote_format_registry::RemoteClipboardFormatRegistry;
use crate::windows::utils::render_format;
use crate::windows::{BackendEvent, SharedFileList, WM_CLIPRDR_BACKEND_EVENT, WinCliprdrError, WinCliprdrResult};

const RENDER_FORMAT_TIMEOUT_SECS: u64 = 10;
const IDT_CLIPBOARD_RETRY: usize = 1;
/// Wire file names ("relative\path\name") are limited to 259 characters by MS-RDPECLIP.
const MAX_WIRE_NAME_LEN: usize = 259;
/// Upper bound on files offered in one copy (a pasted folder is walked recursively).
const MAX_COPIED_FILES: usize = 20_000;
/// Chunk size and per-chunk timeout when downloading files the remote copied.
const DOWNLOAD_CHUNK: u64 = 4 * 1024 * 1024;
const FILE_CHUNK_TIMEOUT_SECS: u64 = 60;

/// Internal implementation of the clipboard processing logic.
pub(crate) struct WinClipboardImpl {
    window: HWND,
    message_proxy: Box<dyn ClipboardMessageProxy>,
    window_is_active: bool,
    backend_rx: mpsc::Receiver<BackendEvent>,
    // Number of attempts spent to process current clipboard message
    attempt: u32,
    // Message to retry
    retry_message: Option<BackendEvent>,
    // Formats available on the remote (represented as LOCAL format ids)
    available_formats_on_remote: Vec<ClipboardFormatId>,
    remote_format_registry: RemoteClipboardFormatRegistry,
    // SystemDeck patch: files offered to the remote, shared with the backend that serves them
    local_files: SharedFileList,
    // SystemDeck patch: file copies may only be offered once the channel is Ready and the
    // server accepted file streams; offering earlier is rejected and drops the session.
    channel_ready: bool,
    file_copy_negotiated: bool,
    // SystemDeck patch: remote id of FileGroupDescriptorW when the remote copied files
    remote_file_list_format: Option<ClipboardFormatId>,
    next_stream_id: u32,
}

impl WinClipboardImpl {
    pub(crate) fn new(
        window: HWND,
        message_proxy: impl ClipboardMessageProxy + 'static,
        backend_rx: mpsc::Receiver<BackendEvent>,
        local_files: SharedFileList,
    ) -> Self {
        Self {
            window,
            message_proxy: Box::new(message_proxy),
            window_is_active: true, // We assume that we start with current window active,
            backend_rx,
            attempt: 0,
            retry_message: None,
            available_formats_on_remote: Vec::new(),
            remote_format_registry: Default::default(),
            local_files,
            channel_ready: false,
            file_copy_negotiated: false,
            remote_file_list_format: None,
            next_stream_id: 0,
        }
    }

    /// SystemDeck patch: the user pasted files the remote copied. Fetches the remote file list,
    /// downloads every file into a fresh temporary folder, then renders `CF_HDROP` pointing at the
    /// downloaded top-level items so Explorer copies them to the paste location. Runs inside
    /// `WM_RENDERFORMAT`, so the pasting application waits until the download finishes.
    fn render_remote_files(&mut self, list_format: ClipboardFormatId) -> WinCliprdrResult<()> {
        self.message_proxy
            .send_clipboard_message(ClipboardMessage::SendInitiatePaste(list_format));
        let (files, lock_id) = loop {
            match self.backend_rx.recv_timeout(Duration::from_secs(RENDER_FORMAT_TIMEOUT_SECS)) {
                Ok(BackendEvent::RemoteFileList(files, lock_id)) => break (files, lock_id),
                Ok(BackendEvent::FormatDataResponse(_)) => {
                    warn!("Remote refused to send its copied file list");
                    return Err(WinCliprdrError::FileTransfer);
                }
                Ok(other) => warn!(?other, "Ignoring event while waiting for the remote file list"),
                Err(_) => return Err(WinCliprdrError::DataReceiveTimeout),
            }
        };

        let dir = new_download_dir().ok_or(WinCliprdrError::FileTransfer)?;
        let mut roots = Vec::new();
        for (index, file) in files.iter().enumerate() {
            let Some(relative) = local_relative_path(file) else {
                warn!(name = %file.name, "Skipping remote file with an unusable name");
                continue;
            };
            let target = dir.join(&relative);
            if relative.components().count() == 1 {
                roots.push(target.clone());
            }
            let is_dir = file
                .attributes
                .is_some_and(|a| a.contains(ClipboardFileAttributes::DIRECTORY));
            if is_dir {
                std::fs::create_dir_all(&target).map_err(|e| file_error(&target, e))?;
                continue;
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| file_error(parent, e))?;
            }
            self.download_remote_file(index, file, lock_id, &target)?;
        }
        if roots.is_empty() {
            return Err(WinCliprdrError::FileTransfer);
        }

        // SAFETY: called while processing `WM_RENDERFORMAT`, as `render_format` requires.
        unsafe { render_format(ClipboardFormatId::CF_HDROP, &hdrop_bytes(&roots)) }
    }

    /// SystemDeck patch: streams one remote file to `target` in chunks.
    fn download_remote_file(
        &mut self,
        index: usize,
        file: &FileDescriptor,
        lock_id: Option<u32>,
        target: &Path,
    ) -> WinCliprdrResult<()> {
        let size = match file.file_size {
            Some(size) => size,
            None => self
                .request_file_contents(index, lock_id, FileContentsFlags::SIZE, 0, 8)?
                .data_as_size()
                .map_err(|_| WinCliprdrError::FileTransfer)?,
        };
        let mut out = std::fs::File::create(target).map_err(|e| file_error(target, e))?;
        let mut position = 0u64;
        while position < size {
            let want = u32::try_from((size - position).min(DOWNLOAD_CHUNK)).unwrap_or(u32::MAX);
            let response = self.request_file_contents(index, lock_id, FileContentsFlags::RANGE, position, want)?;
            let data = response.data();
            if data.is_empty() {
                break;
            }
            out.write_all(data).map_err(|e| file_error(target, e))?;
            position += u64::try_from(data.len()).unwrap_or(0);
        }
        Ok(())
    }

    /// SystemDeck patch: asks the remote for a file's size or a byte range and waits for the reply.
    fn request_file_contents(
        &mut self,
        index: usize,
        lock_id: Option<u32>,
        flags: FileContentsFlags,
        position: u64,
        requested_size: u32,
    ) -> WinCliprdrResult<FileContentsResponse<'static>> {
        self.next_stream_id = self.next_stream_id.wrapping_add(1);
        let stream_id = self.next_stream_id;
        let request = FileContentsRequest {
            stream_id,
            index: i32::try_from(index).map_err(|_| WinCliprdrError::FileTransfer)?,
            flags,
            position,
            requested_size,
            data_id: lock_id,
        };
        self.message_proxy
            .send_clipboard_message(ClipboardMessage::SendFileContentsRequest(request));
        loop {
            match self.backend_rx.recv_timeout(Duration::from_secs(FILE_CHUNK_TIMEOUT_SECS)) {
                Ok(BackendEvent::FileContentsResponse(response)) if response.stream_id() == stream_id => {
                    if response.is_error() {
                        warn!(index, position, "Remote failed to send file contents");
                        return Err(WinCliprdrError::FileTransfer);
                    }
                    return Ok(response);
                }
                Ok(other) => warn!(?other, "Ignoring event while waiting for remote file contents"),
                Err(_) => return Err(WinCliprdrError::DataReceiveTimeout),
            }
        }
    }

    /// SystemDeck patch: if the local clipboard holds files (CF_HDROP) and the channel can take a
    /// file copy, records them for the backend and returns the file-copy message to send.
    fn offer_files(&mut self, clipboard: &OwnedOsClipboard) -> Option<ClipboardMessage> {
        if !(self.channel_ready && self.file_copy_negotiated) {
            return None;
        }
        let dropped = ClipboardDataRef::get(clipboard, ClipboardFormatId::CF_HDROP)
            .map(|data| parse_hdrop(data.data()))
            .unwrap_or_default();
        let mut descriptors = Vec::new();
        let mut paths = Vec::new();
        for path in &dropped {
            collect_files(path, None, &mut descriptors, &mut paths);
        }
        if descriptors.is_empty() {
            return None;
        }
        if let Ok(mut files) = self.local_files.lock() {
            *files = paths;
        }
        Some(ClipboardMessage::SendInitiateFileCopy(descriptors))
    }

    /// SystemDeck patch: the channel just became Ready; offer any files already on the clipboard.
    fn on_channel_ready(&mut self) -> WinCliprdrResult<Option<ClipboardMessage>> {
        self.channel_ready = true;
        let clipboard = OwnedOsClipboard::new(self.window)?;
        Ok(self.offer_files(&clipboard))
    }

    fn on_format_data_request(&mut self, request: &FormatDataRequest) -> WinCliprdrResult<Option<ClipboardMessage>> {
        // Get data from the clipboard and send event to the main event loop
        let clipboard = OwnedOsClipboard::new(self.window)?;

        let buffer = ClipboardDataRef::get(&clipboard, request.format).map(|borrowed| borrowed.data().to_vec());

        let response = match buffer {
            Some(data) => FormatDataResponse::new_data(data),
            None => {
                // No data available for this format
                FormatDataResponse::new_error()
            }
        };

        Ok(Some(ClipboardMessage::SendFormatData(response)))
    }

    fn on_format_data_response(
        requested_local_format: ClipboardFormatId,
        response: &FormatDataResponse<'_>,
    ) -> WinCliprdrResult<()> {
        if response.is_error() {
            // No data available for this format anymore
            return Ok(());
        }

        // SAFETY: `on_format_data_response` is only called in a context of processing
        // `WM_RENDERFORMAT` and `WM_RENDERALLFORMATS` messages, so we can safely assume that
        // calling `ClipboardData::render` is safe.
        unsafe {
            render_format(requested_local_format, response.data())?;
        }

        Ok(())
    }

    fn on_clipboard_update(&mut self) -> WinCliprdrResult<Option<ClipboardMessage>> {
        let mut clipboard = OwnedOsClipboard::new(self.window)?;

        // SystemDeck patch: files copied in Explorer (CF_HDROP) are offered to the remote as a
        // file list it can paste; their contents are streamed on demand by the backend. Until
        // the channel is Ready, a normal format list is sent instead (the first one also carries
        // our capabilities), and the files are offered on `ChannelReady`.
        if let Some(message) = self.offer_files(&clipboard) {
            return Ok(Some(message));
        }

        let mut formats = clipboard.enum_available_formats()?;

        let mut filtered_format_ids = formats.iter().map(|f| f.id()).collect::<HashSet<_, _>>();
        filter_format_ids(&mut filtered_format_ids);
        formats.retain(|format| filtered_format_ids.contains(&format.id()));

        Ok(Some(ClipboardMessage::SendInitiateCopy(formats)))
    }

    fn get_remote_format_data(
        &mut self,
        format: ClipboardFormatId,
    ) -> WinCliprdrResult<Option<FormatDataResponse<'static>>> {
        let mapped_format = self.remote_format_registry.local_to_remote(format);

        let remote_format = if let Some(format) = mapped_format {
            format
        } else {
            // Format is unknown or not supported on the local machine
            return Ok(None);
        };

        // We need to receive data from the remote clipboard immediately, because Windows
        // expects us to set clipboard data before returning from `WM_RENDERFORMAT` handler.
        //
        // Sadly, this will block the GUI thread, while data is being received
        self.message_proxy
            .send_clipboard_message(ClipboardMessage::SendInitiatePaste(remote_format));
        let data = self
            .backend_rx
            .recv_timeout(Duration::from_secs(RENDER_FORMAT_TIMEOUT_SECS))
            .map_err(|_| WinCliprdrError::DataReceiveTimeout)?;

        match data {
            BackendEvent::FormatDataResponse(response) => Ok(Some(response)),
            _ => {
                warn!("Unexpected FormatData response: {:?}", data);
                // Unexpected message, ignore it
                Ok(None)
            }
        }
    }

    fn on_render_format(&mut self, format: ClipboardFormatId) -> WinCliprdrResult<Option<ClipboardMessage>> {
        // SystemDeck patch: pasting files the remote copied
        if format == ClipboardFormatId::CF_HDROP {
            if let Some(list_format) = self.remote_file_list_format {
                self.render_remote_files(list_format)?;
                return Ok(None);
            }
        }

        // Owning clipboard is not required when processing `WM_RENDERFORMAT` message
        if let Some(response) = self.get_remote_format_data(format)? {
            Self::on_format_data_response(format, &response)?;
        }

        Ok(None)
    }

    fn on_render_all_formats(&mut self) -> WinCliprdrResult<Option<ClipboardMessage>> {
        // We need to be clipboard owner to be able to set all clipboard formats
        let _clipboard = match OwnedOsClipboard::new(self.window) {
            Ok(clipboard) => {
                // SAFETY: `GetClipboardOwner` is always safe to call
                if self.window != unsafe { GetClipboardOwner()? } {
                    // As per MSDN, we need to validate clipboard owner after opening clipboard
                    return Ok(None);
                }

                clipboard
            }
            Err(WinCliprdrError::ClipboardAccessDenied) => {
                // We don't own the clipboard anymore, we don't need to do anything
                return Ok(None);
            }
            Err(err) => {
                return Err(err);
            }
        };

        let formats = core::mem::take(&mut self.available_formats_on_remote);

        // Clearing clipboard is not required, just render all available formats

        for format in formats {
            if let Some(response) = self.get_remote_format_data(format)? {
                Self::on_format_data_response(format, &response)?;
            }
        }

        Ok(None)
    }

    fn on_remote_format_list(&mut self, formats: &[ClipboardFormat]) -> WinCliprdrResult<Option<ClipboardMessage>> {
        self.available_formats_on_remote.clear();

        // Clear previous format mapping
        self.remote_format_registry.clear();

        // SystemDeck patch: the remote copied files. Offer them locally as a delay-rendered file
        // list (CF_HDROP); the files are downloaded only when something actually pastes them.
        self.remote_file_list_format = formats
            .iter()
            .find(|f| f.name().is_some_and(|n| n.value() == ClipboardFormatName::FILE_LIST.value()))
            .map(ClipboardFormat::id);
        if self.remote_file_list_format.is_some() && self.file_copy_negotiated {
            let mut clipboard = OwnedOsClipboard::new(self.window)?;
            clipboard.clear()?;
            clipboard.delay_render(ClipboardFormatId::CF_HDROP)?;
            return Ok(None);
        }
        self.remote_file_list_format = None;

        let mut local_format_ids = formats
            .iter()
            .filter_map(|format| {
                // `register` will return None if format is unknown/unsupported on the local machine,
                // so we need to filter them out.
                self.remote_format_registry.register(format)
            })
            .collect::<HashSet<_>>();

        filter_format_ids(&mut local_format_ids);

        if local_format_ids.is_empty() {
            return Ok(None);
        }

        // We need to be clipboard owner to be able to set clipboard data
        let mut clipboard = OwnedOsClipboard::new(self.window)?;
        // Before sending delayed-rendered data, we need to clear clipboard first.
        clipboard.clear()?;

        for format in local_format_ids.into_iter() {
            clipboard.delay_render(format)?;
            // Store available format for `WM_RENDERALLFORMATS` processing.
            self.available_formats_on_remote.push(format);
        }

        Ok(None)
    }

    fn handle_event(&mut self, event: BackendEvent) {
        let result = match &event {
            BackendEvent::FormatDataRequest(request) => self.on_format_data_request(request),
            BackendEvent::FormatDataResponse(_) => {
                // Out-of-order message, ignore it.
                Ok(None)
            }
            BackendEvent::RemoteFormatList(formats) => self.on_remote_format_list(formats),

            BackendEvent::ClipboardUpdated | BackendEvent::RemoteRequestsFormatList => self.on_clipboard_update(),
            BackendEvent::RenderFormat(format) => self.on_render_format(*format),
            BackendEvent::RenderAllFormats => self.on_render_all_formats(),

            BackendEvent::DowngradedCapabilities(flags) => {
                // SystemDeck patch: negotiation starts a (re)connection; remember whether the
                // server accepted file streams and wait for Ready before offering files.
                self.file_copy_negotiated = flags.contains(ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED);
                self.channel_ready = false;
                Ok(None)
            }
            BackendEvent::ChannelReady => self.on_channel_ready(),
            BackendEvent::RemoteFileList(..) | BackendEvent::FileContentsResponse(_) => {
                // Out-of-order reply to a file download that already finished or gave up.
                Ok(None)
            }
        };

        let retry_err = match result {
            Ok(Some(message)) => {
                self.message_proxy.send_clipboard_message(message);
                None
            }
            Ok(None) => {
                // No message to send
                None
            }
            Err(err) => {
                // Some errors are retryable (e.g. access to clipboard is temporarily denied)
                if let WinCliprdrError::ClipboardAccessDenied = &err {
                    Some(err)
                } else {
                    self.message_proxy
                        .send_clipboard_message(ClipboardMessage::Error(Box::new(err)));
                    None
                }
            }
        };

        match retry_err {
            None => {
                self.attempt = 0;
            }
            Some(err) => {
                const MAX_PROCESSING_ATTEMPTS: u32 = 10;
                const PROCESSING_TIMEOUT_MS: u32 = 100;

                #[expect(clippy::arithmetic_side_effects)]
                // self.attempt can’t be greater than MAX_PROCESSING_ATTEMPTS, so the arithmetic is safe here
                if self.attempt < MAX_PROCESSING_ATTEMPTS {
                    self.attempt += 1;

                    self.retry_message = Some(event);

                    // SAFETY: `SetTimer` is always safe to call when `hwnd` is a valid window handle
                    unsafe {
                        SetTimer(
                            Some(self.window),
                            IDT_CLIPBOARD_RETRY,
                            self.attempt * PROCESSING_TIMEOUT_MS,
                            None,
                        )
                    };
                } else {
                    // Send error, retries limit exceeded
                    self.message_proxy
                        .send_clipboard_message(ClipboardMessage::Error(Box::new(err)));
                }
            }
        }
    }
}

/// SystemDeck patch: maps a remote file's (already sanitized) "relative\path" + name to a local
/// relative path, refusing anything that could escape the download folder.
fn local_relative_path(file: &FileDescriptor) -> Option<PathBuf> {
    let mut path = PathBuf::new();
    let parts = file
        .relative_path
        .as_deref()
        .unwrap_or("")
        .split(['\\', '/'])
        .chain(core::iter::once(file.name.as_str()));
    for part in parts.filter(|p| !p.is_empty()) {
        if part == "." || part == ".." || part.contains(':') {
            return None;
        }
        path.push(part);
    }
    (path.components().count() > 0).then_some(path)
}

/// SystemDeck patch: a fresh folder under `%TEMP%\SystemDeck\clipboard` for one paste; earlier
/// paste folders are removed (best effort) so downloads don't pile up.
fn new_download_dir() -> Option<PathBuf> {
    let base = std::env::temp_dir().join("SystemDeck").join("clipboard");
    if let Ok(entries) = std::fs::read_dir(&base) {
        for entry in entries.flatten() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dir = base.join(stamp.to_string());
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn file_error(path: &Path, err: std::io::Error) -> WinCliprdrError {
    warn!(path = %path.display(), %err, "Clipboard file transfer failed");
    WinCliprdrError::FileTransfer
}

/// SystemDeck patch: builds a `CF_HDROP` (`DROPFILES`) buffer listing `paths` as UTF-16.
fn hdrop_bytes(paths: &[PathBuf]) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt as _;
    let mut out = Vec::new();
    out.extend_from_slice(&20u32.to_le_bytes()); // pFiles: list starts right after this header
    out.extend_from_slice(&0i32.to_le_bytes()); // pt.x
    out.extend_from_slice(&0i32.to_le_bytes()); // pt.y
    out.extend_from_slice(&0u32.to_le_bytes()); // fNC
    out.extend_from_slice(&1u32.to_le_bytes()); // fWide
    for path in paths {
        for unit in path.as_os_str().encode_wide() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out.extend_from_slice(&[0, 0]);
    }
    out.extend_from_slice(&[0, 0]);
    out
}

/// SystemDeck patch: reads the file paths out of a `CF_HDROP` (`DROPFILES`) buffer: a 20-byte
/// header whose first field is the offset of a list of null-terminated paths (UTF-16 when
/// `fWide` is set), ended by an empty string.
fn parse_hdrop(data: &[u8]) -> Vec<PathBuf> {
    let field = |at: usize| data.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let (Some(offset), Some(wide)) = (field(0), field(16)) else {
        return Vec::new();
    };
    let Some(list) = usize::try_from(offset).ok().and_then(|o| data.get(o..)) else {
        return Vec::new();
    };
    let names: Vec<String> = if wide != 0 {
        let units: Vec<u16> = list.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        units
            .split(|&u| u == 0)
            .take_while(|s| !s.is_empty())
            .map(String::from_utf16_lossy)
            .collect()
    } else {
        list.split(|&b| b == 0)
            .take_while(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect()
    };
    names.into_iter().map(PathBuf::from).collect()
}

/// SystemDeck patch: adds `path` (and, for a folder, everything under it) to the file list sent
/// to the remote. `relative` is the folder path inside the copied set ("a\b"), `None` at the top.
/// `paths` gets the matching local path for each descriptor, in the same order.
fn collect_files(path: &Path, relative: Option<&str>, descriptors: &mut Vec<FileDescriptor>, paths: &mut Vec<PathBuf>) {
    if descriptors.len() >= MAX_COPIED_FILES {
        return;
    }
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return;
    };
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    let wire_len = relative.map_or(0, |r| r.chars().count() + 1) + name.chars().count();
    if MAX_WIRE_NAME_LEN < wire_len {
        warn!(path = %path.display(), "Skipping file: name too long to copy over RDP");
        return;
    }

    let mut descriptor = FileDescriptor::new(name.clone());
    descriptor.relative_path = relative.map(str::to_owned);
    descriptor.last_write_time = Some(meta.last_write_time());

    if meta.is_dir() {
        descriptor.attributes = Some(ClipboardFileAttributes::DIRECTORY);
        descriptor.file_size = Some(0);
        descriptors.push(descriptor);
        paths.push(path.to_path_buf());

        let inner = match relative {
            Some(r) => format!("{r}\\{name}"),
            None => name,
        };
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        let mut children: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        children.sort();
        for child in children {
            collect_files(&child, Some(&inner), descriptors, paths);
        }
    } else {
        let mut attributes = ClipboardFileAttributes::from_bits_truncate(meta.file_attributes());
        attributes.remove(ClipboardFileAttributes::DIRECTORY);
        if attributes.is_empty() {
            attributes = ClipboardFileAttributes::NORMAL;
        }
        descriptor.attributes = Some(attributes);
        descriptor.file_size = Some(meta.len());
        descriptors.push(descriptor);
        paths.push(path.to_path_buf());
    }
}

fn filter_format_ids(formats: &mut HashSet<ClipboardFormatId>) {
    let has_non_utf16_formats =
        formats.contains(&ClipboardFormatId::CF_TEXT) || formats.contains(&ClipboardFormatId::CF_OEMTEXT);

    let has_utf16_formats = formats.contains(&ClipboardFormatId::CF_UNICODETEXT);

    // Windows could implicitly convert `CF_UNICODETEXT` to `CF_TEXT`, so to avoid
    // character encoding issues with application which prefer non-unicode text, we need to remove
    // `CF_TEXT` and `CF_OEMTEXT` from the list of formats.
    if has_utf16_formats && has_non_utf16_formats {
        formats.remove(&ClipboardFormatId::CF_TEXT);
        formats.remove(&ClipboardFormatId::CF_OEMTEXT);
    }
}

/// WinAPI event loop for clipboard processing
///
/// # Safety
///
/// This function should only be used for windows subclassing api via `SetWindowSubclass`.
pub(crate) unsafe extern "system" fn clipboard_subproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    if msg == WM_DESTROY {
        // Transfer ownership and drop previously allocated context

        // SAFETY: `data` is a valid pointer, returned by `Box::into_raw`, transferred to OS earlier
        // via `SetWindowSubclass` call.
        let _ = unsafe { Box::from_raw(with_exposed_provenance_mut::<WinClipboardImpl>(data)) };
        return LRESULT(0);
    }

    // SAFETY: `data` is a valid pointer, returned by `Box::into_raw`, transferred to OS earlier
    // via `SetWindowSubclass` call.
    let ctx = unsafe { &mut *(with_exposed_provenance_mut::<WinClipboardImpl>(data)) };

    match msg {
        // We need to keep track of window state to distinguish between local and remote copy
        WM_ACTIVATE | WM_ACTIVATEAPP => {
            ctx.window_is_active = wparam.0 != usize::try_from(WA_INACTIVE).expect("WA_INACTIVE fits into usize")
        }
        // Sent by the OS when OS clipboard content is changed
        WM_CLIPBOARDUPDATE => {
            // SAFETY: `GetClipboardOwner` is always safe to call.
            let clipboard_owner = unsafe { GetClipboardOwner() };
            let spurious_event = clipboard_owner == Ok(hwnd);

            // SystemDeck patch: forward every local clipboard change we did not make ourselves.
            // Upstream also required "window not active", tracked via WM_ACTIVATE(APP). Those
            // messages go to the thread owning the app's UI window, but this hidden window lives
            // on the RDP session thread, so the flag stayed "active" and every local copy was
            // ignored. The owner check below is what prevents echoing remote copies back.
            if !spurious_event {
                ctx.handle_event(BackendEvent::ClipboardUpdated);
            }
        }
        // Sent by the OS when delay-rendered data is requested for rendering.
        WM_RENDERFORMAT => {
            ctx.handle_event(BackendEvent::RenderFormat(ClipboardFormatId::new(
                u32::try_from(wparam.0).expect("should never truncate in practice"),
            )));
        }
        // Sent by the OS when all delay-rendered data is requested for rendering.
        WM_RENDERALLFORMATS => {
            ctx.handle_event(BackendEvent::RenderAllFormats);
        }
        // User event, a message was sent by the `Cliprdr` backend shim
        WM_CLIPRDR_BACKEND_EVENT => {
            let message = if let Ok(message) = ctx.backend_rx.try_recv() {
                message
            } else {
                // No message has been received, spurious event
                return LRESULT(0);
            };

            ctx.handle_event(message);
        }
        // Retry timer. Some operations clipboard operations such as `OpenClipboard` are
        // fallible. We need to retry them a few times before giving up.
        WM_TIMER => {
            if wparam.0 == IDT_CLIPBOARD_RETRY {
                // Timer is one-shot, we need to stop it immediately

                // SAFETY: `KillTimer` is always safe to call when `hwnd` is a valid window handle.
                if let Err(err) = unsafe { KillTimer(Some(hwnd), IDT_CLIPBOARD_RETRY) } {
                    tracing::error!("Failed to kill timer: {}", err);
                }

                if let Some(event) = ctx.retry_message.take() {
                    ctx.handle_event(event);
                }
            }
        }
        _ => {
            // Call next event handler in the subclass chain

            // SAFETY: `DefSubclassProc` is always safe to call in context of subclass event loop
            return unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
        }
    };

    LRESULT(0) // SUCCESS
}
