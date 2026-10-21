use std::io::{Read as _, Seek as _, SeekFrom};
use std::sync::mpsc as mpsc_sync;

use ironrdp_cliprdr::backend::{ClipboardMessage, CliprdrBackend};
use ironrdp_cliprdr::pdu::{
    ClipboardFormat, ClipboardGeneralCapabilityFlags, FileContentsFlags, FileContentsRequest, FileContentsResponse,
    FileDescriptor, FormatDataRequest, FormatDataResponse, LockDataId,
};
use ironrdp_core::{IntoOwned as _, impl_as_any};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::windows::{BackendEvent, SharedFileList, SharedProxy, WM_CLIPRDR_BACKEND_EVENT};

/// Largest single chunk served for one file contents request (the server asks in chunks).
const MAX_FILE_CHUNK: u64 = 32 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct WinCliprdrBackend {
    backend_event_tx: mpsc_sync::SyncSender<BackendEvent>,
    window: HWND,
    files: SharedFileList,
    proxy: SharedProxy,
}

// SAFETY: window handle is thread safe for PostMessageW usage
unsafe impl Send for WinCliprdrBackend {}

impl_as_any!(WinCliprdrBackend);

impl WinCliprdrBackend {
    pub(crate) fn new(
        window: HWND,
        backend_event_tx: mpsc_sync::SyncSender<BackendEvent>,
        files: SharedFileList,
        proxy: SharedProxy,
    ) -> Self {
        Self {
            window,
            backend_event_tx,
            files,
            proxy,
        }
    }

    /// SystemDeck patch: answers the server's request for a copied file's size or a byte
    /// range of it, reading straight from disk. `None` means the request can't be served.
    fn serve_file_contents(&self, request: &FileContentsRequest) -> Option<FileContentsResponse<'static>> {
        let index = usize::try_from(request.index).ok()?;
        let path = self.files.lock().ok()?.get(index).cloned()?;
        let meta = std::fs::metadata(&path).ok()?;
        if request.flags.contains(FileContentsFlags::SIZE) {
            let size = if meta.is_dir() { 0 } else { meta.len() };
            return Some(FileContentsResponse::new_size_response(request.stream_id, size));
        }
        if !request.flags.contains(FileContentsFlags::RANGE) || meta.is_dir() {
            return None;
        }
        let mut file = std::fs::File::open(&path).ok()?;
        file.seek(SeekFrom::Start(request.position)).ok()?;
        let want = u64::from(request.requested_size).min(MAX_FILE_CHUNK);
        let mut data = Vec::new();
        file.take(want).read_to_end(&mut data).ok()?;
        Some(FileContentsResponse::new_data_response(request.stream_id, data))
    }

    fn send_event(&self, event: BackendEvent) {
        if self.backend_event_tx.send(event).is_err() {
            // Channel is closed, backend is dead
            return;
        }
        // Wake up subproc event loop; Dont wait for result
        //
        // SAFETY: it is safe to call PostMessageW from any thread with a valid window handle
        if let Err(err) = unsafe { PostMessageW(Some(self.window), WM_CLIPRDR_BACKEND_EVENT, WPARAM(0), LPARAM(0)) } {
            tracing::error!("Failed to post message to wake up subproc event loop: {}", err);
        }
    }
}

impl CliprdrBackend for WinCliprdrBackend {
    fn temporary_directory(&self) -> &str {
        ".cliprdr"
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        // SystemDeck patch: copying files from this PC to the remote (file streams over the
        // clipboard, relative paths only, 64-bit sizes for files over 4 GB).
        ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED
            | ClipboardGeneralCapabilityFlags::FILECLIP_NO_FILE_PATHS
            | ClipboardGeneralCapabilityFlags::HUGE_FILE_SUPPORT_ENABLED
    }

    fn on_ready(&mut self) {
        self.send_event(BackendEvent::ChannelReady);
    }

    fn on_process_negotiated_capabilities(&mut self, capabilities: ClipboardGeneralCapabilityFlags) {
        self.send_event(BackendEvent::DowngradedCapabilities(capabilities))
    }

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        self.send_event(BackendEvent::RemoteFormatList(available_formats.to_vec()));
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        self.send_event(BackendEvent::FormatDataRequest(request));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        self.send_event(BackendEvent::FormatDataResponse(response.into_owned()));
    }

    fn on_file_contents_request(&mut self, request: FileContentsRequest) {
        let response = self.serve_file_contents(&request).unwrap_or_else(|| {
            tracing::warn!(index = request.index, "Cannot serve clipboard file contents request");
            FileContentsResponse::new_error(request.stream_id)
        });
        if let Ok(proxy) = self.proxy.lock() {
            proxy.send_clipboard_message(ClipboardMessage::SendFileContentsResponse(response));
        }
    }

    fn on_file_contents_response(&mut self, response: FileContentsResponse<'_>) {
        // SystemDeck patch: a chunk of a file being copied from the remote.
        self.send_event(BackendEvent::FileContentsResponse(response.into_owned()));
    }

    fn on_remote_file_list(&mut self, files: &[FileDescriptor], clip_data_id: Option<u32>) {
        // SystemDeck patch: the remote's copied files, requested when the user pastes locally.
        self.send_event(BackendEvent::RemoteFileList(files.to_vec(), clip_data_id));
    }

    fn on_lock(&mut self, _data_id: LockDataId) {
        // File transfer not implemented yet
    }

    fn on_unlock(&mut self, _data_id: LockDataId) {
        // File transfer not implemented yet
    }

    fn on_request_format_list(&mut self) {
        self.send_event(BackendEvent::RemoteRequestsFormatList);
    }

    fn now_ms(&self) -> u64 {
        crate::native_now_ms()
    }

    fn elapsed_ms(&self, since: u64) -> u64 {
        self.now_ms().saturating_sub(since)
    }
}
