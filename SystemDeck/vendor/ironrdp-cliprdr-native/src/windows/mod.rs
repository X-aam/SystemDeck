mod clipboard_data_ref;
mod clipboard_impl;
mod cliprdr_backend;
mod os_clipboard;
mod remote_format_registry;
mod utils;

use std::path::PathBuf;
use std::sync::mpsc as mpsc_sync;
use std::sync::{Arc, Mutex};

use ironrdp_cliprdr::backend::{ClipboardMessageProxy, CliprdrBackend, CliprdrBackendFactory};
use ironrdp_cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsResponse, FileDescriptor,
    FormatDataRequest, FormatDataResponse,
};
use tracing::error;
pub use windows::Win32::Foundation::HWND;
use windows::Win32::Foundation::{E_ACCESSDENIED, FALSE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{AddClipboardFormatListener, RemoveClipboardFormatListener};
use windows::Win32::System::LibraryLoader::GetModuleHandleA;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Shell::SetWindowSubclass;
use windows::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExA, DefWindowProcA, DestroyWindow, DispatchMessageW, GetClassInfoA, GetMessageW, MSG,
    PostThreadMessageW, RegisterClassA, TranslateMessage, WINDOW_EX_STYLE, WM_QUIT, WM_USER, WNDCLASSA, WS_POPUP,
};
use windows::core::{Error, s};

use self::clipboard_impl::{WinClipboardImpl, clipboard_subproc};
use self::cliprdr_backend::WinCliprdrBackend;

const BACKEND_CHANNEL_SIZE: usize = 8;
const WM_CLIPRDR_BACKEND_EVENT: u32 = WM_USER;

/// SystemDeck patch: local files offered to the remote by the last file copy, in the same
/// order as the file list sent to the server (the server asks for contents by index).
pub(crate) type SharedFileList = Arc<Mutex<Vec<PathBuf>>>;

/// SystemDeck patch: message proxy shared by every backend instance so file contents
/// requests are answered directly, without a round trip through the window message loop.
pub(crate) type SharedProxy = Arc<Mutex<Box<dyn ClipboardMessageProxy>>>;

pub type WinCliprdrResult<T> = Result<T, WinCliprdrError>;

#[derive(Debug)]
pub enum WinCliprdrError {
    AddClipboardFormatListener,
    FormatsEnumeration,
    ClipboardAccessDenied,
    ClipboardOpen,
    ClipboardEmpty,
    Utf16Conversion,
    ClipboardData,
    SetClipboardData,
    WindowSubclass,
    Alloc,
    DataReceiveTimeout,
    RenderFormat,
    /// SystemDeck patch: copying files from the remote failed (details are logged)
    FileTransfer,
    WinAPI(Error),
}

impl core::fmt::Display for WinCliprdrError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            WinCliprdrError::AddClipboardFormatListener => write!(f, "failed to register clipboard format listener"),
            WinCliprdrError::FormatsEnumeration => {
                write!(f, "failed to enumerate formats available in the current clipboard")
            }
            WinCliprdrError::ClipboardAccessDenied => write!(f, "clipboard is busy or denied"),
            WinCliprdrError::ClipboardOpen => write!(f, "failed to open the clipboard"),
            WinCliprdrError::ClipboardEmpty => write!(f, "failed to empty the clipboard"),
            WinCliprdrError::Utf16Conversion => write!(f, "failed to convert UTF-16 string to UTF-8"),
            WinCliprdrError::ClipboardData => write!(f, "failed to get current clipboard data"),
            WinCliprdrError::SetClipboardData => write!(f, "failed to set clipboard data"),
            WinCliprdrError::WindowSubclass => write!(f, "failed to subclass window"),
            WinCliprdrError::Alloc => write!(f, "failed to allocate global memory"),
            WinCliprdrError::DataReceiveTimeout => write!(f, "failed to receive data from remote clipboard"),
            WinCliprdrError::RenderFormat => write!(f, "failed to render clipboard format"),
            WinCliprdrError::FileTransfer => write!(f, "failed to copy files from the remote clipboard"),
            WinCliprdrError::WinAPI(_error) => write!(f, "WinAPI error"),
        }
    }
}

impl core::error::Error for WinCliprdrError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            WinCliprdrError::AddClipboardFormatListener => None,
            WinCliprdrError::FormatsEnumeration => None,
            WinCliprdrError::ClipboardAccessDenied => None,
            WinCliprdrError::ClipboardOpen => None,
            WinCliprdrError::ClipboardEmpty => None,
            WinCliprdrError::Utf16Conversion => None,
            WinCliprdrError::ClipboardData => None,
            WinCliprdrError::SetClipboardData => None,
            WinCliprdrError::WindowSubclass => None,
            WinCliprdrError::Alloc => None,
            WinCliprdrError::DataReceiveTimeout => None,
            WinCliprdrError::RenderFormat => None,
            WinCliprdrError::FileTransfer => None,
            WinCliprdrError::WinAPI(error) => Some(error),
        }
    }
}

impl From<Error> for WinCliprdrError {
    fn from(err: Error) -> Self {
        if err.code() == E_ACCESSDENIED {
            WinCliprdrError::ClipboardAccessDenied
        } else {
            WinCliprdrError::WinAPI(err)
        }
    }
}

/// Sent from the clipboard backend shim to the actual WinAPI subproc event loop
#[derive(Debug)]
pub(crate) enum BackendEvent {
    // Events generated by OS event loop
    ClipboardUpdated,
    RenderFormat(ClipboardFormatId),
    RenderAllFormats,

    // PDU processing events
    DowngradedCapabilities(ClipboardGeneralCapabilityFlags),
    RemoteFormatList(Vec<ClipboardFormat>),
    FormatDataRequest(FormatDataRequest),
    FormatDataResponse(FormatDataResponse<'static>),
    RemoteRequestsFormatList,
    // SystemDeck patch: the clipboard channel finished initialization (file copies are only
    // accepted after this)
    ChannelReady,
    // SystemDeck patch: the remote's copied file list (answer to our FileGroupDescriptorW request)
    RemoteFileList(Vec<FileDescriptor>, Option<u32>),
    // SystemDeck patch: the remote's answer to one of our file contents requests
    FileContentsResponse(FileContentsResponse<'static>),
}

/// Windows RDP client clipboard implementation.
///
/// IronRDP client implementation should provide raw window handle and message proxy to send
/// messages from the backend to `CLIPRDR` SVC.
///
/// [`WinClipboard`] instance holds ownership of the actual clipboard backend processing logic
/// and should be kept alive during the whole lifetime of the application.
///
/// This type is not thread safe, it should be used only in the main thread with a windows event
/// loop. (GUI thread). However, backend factory returned by [`WinClipboard::backend_factory`]
/// can be safely used in other threads.
pub struct WinClipboard {
    window: HWND,
    /// SystemDeck patch: the hidden window lives on its own thread (see [`WinClipboard::new`]).
    thread_id: u32,
    backend_tx: mpsc_sync::SyncSender<BackendEvent>,
    files: SharedFileList,
    proxy: SharedProxy,

    /// Kept non-Send/non-Sync like upstream; the window itself is owned by the clipboard thread.
    _thread_marker: core::marker::PhantomData<*const ()>,
}

impl WinClipboard {
    /// Creates new clipboard instance.
    ///
    /// Under the hood, a hidden window is created for capturing the clipboard events.
    ///
    /// SystemDeck patch: the window is created on, and served by, a dedicated thread with its own
    /// message loop. Pasting remote data blocks the window's thread (`WM_RENDERFORMAT` waits for
    /// the server's reply), so it must not share a thread with the RDP session that receives the
    /// reply; and the caller's thread no longer needs a Win32 message loop.
    pub fn new(message_proxy: impl ClipboardMessageProxy + Clone + 'static) -> WinCliprdrResult<Self> {
        let (backend_tx, backend_rx) = mpsc_sync::sync_channel(BACKEND_CHANNEL_SIZE);
        let files: SharedFileList = Arc::default();
        let proxy: SharedProxy = Arc::new(Mutex::new(Box::new(message_proxy.clone())));

        let (ready_tx, ready_rx) = mpsc_sync::channel::<WinCliprdrResult<(isize, u32)>>();
        let thread_files = Arc::clone(&files);
        std::thread::Builder::new()
            .name("systemdeck-clipboard".to_owned())
            .spawn(move || match create_clipboard_window(message_proxy, backend_rx, thread_files) {
                Ok(window) => {
                    // SAFETY: always safe to call.
                    let thread_id = unsafe { GetCurrentThreadId() };
                    let _ = ready_tx.send(Ok((window.0 as isize, thread_id)));
                    run_message_loop();
                    // SAFETY: the window was created on this thread; destroying it frees the
                    // subclass context in `WM_DESTROY`.
                    unsafe {
                        let _ = RemoveClipboardFormatListener(window);
                        let _ = DestroyWindow(window);
                    }
                }
                Err(err) => {
                    let _ = ready_tx.send(Err(err));
                }
            })
            .map_err(|_| WinCliprdrError::WindowSubclass)?;

        let (raw_window, thread_id) = ready_rx.recv().map_err(|_| WinCliprdrError::WindowSubclass)??;
        Ok(Self {
            window: HWND(raw_window as *mut core::ffi::c_void),
            thread_id,
            backend_tx,
            files,
            proxy,
            _thread_marker: Default::default(),
        })
    }

    /// Returns clipboard backend factory suitable for making backend instances for `CLIPRDR` SVC.
    pub fn backend_factory(&self) -> Box<dyn CliprdrBackendFactory + Send> {
        Box::new(WinCliprdrBackendFactory {
            tx: self.backend_tx.clone(),
            window: self.window,
            files: Arc::clone(&self.files),
            proxy: Arc::clone(&self.proxy),
        })
    }
}

impl Drop for WinClipboard {
    fn drop(&mut self) {
        // Ends the clipboard thread's message loop; it then removes the listener and destroys the
        // window on its own thread.
        //
        // SAFETY: posting to a thread id is always safe; a dead thread just makes this fail.
        if let Err(err) = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) } {
            error!("Failed to stop clipboard thread: {}", err)
        }
    }
}

/// SystemDeck patch: dispatches messages for the clipboard window until `WM_QUIT`.
fn run_message_loop() {
    let mut msg = MSG::default();
    // SAFETY: a standard message loop over this thread's own queue.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Creates the hidden clipboard window on the current thread and attaches the processing logic.
fn create_clipboard_window(
    message_proxy: impl ClipboardMessageProxy + 'static,
    backend_rx: mpsc_sync::Receiver<BackendEvent>,
    files: SharedFileList,
) -> WinCliprdrResult<HWND> {
    extern "system" fn wndproc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        // SAFETY: default handler
        unsafe { DefWindowProcA(window, message, wparam, lparam) }
    }

    // SAFETY: low-level WinAPI call
    let instance = unsafe { GetModuleHandleA(None)? };
    let window_class = s!("IronRDPClipboardMonitor");

    let mut existing_wc = WNDCLASSA::default();
    // SAFETY: `instance` is a valid module handle, `window_class` is a valid null-terminated string,
    // and `existing_wc` is a valid mutable reference to a WNDCLASSA structure.
    let class_exists = unsafe { GetClassInfoA(Some(instance.into()), window_class, &mut existing_wc).is_ok() };

    if !class_exists {
        let wc = WNDCLASSA {
            hInstance: instance.into(),
            lpszClassName: window_class,
            lpfnWndProc: Some(wndproc),
            ..Default::default()
        };

        // SAFETY: low-level WinAPI call
        let atom = unsafe { RegisterClassA(&wc) };
        if atom == 0 {
            return Err(WinCliprdrError::from(Error::from_thread()));
        }
    }

    // SAFETY: low-level WinAPI call
    let window = unsafe {
        CreateWindowExA(
            WINDOW_EX_STYLE::default(),
            window_class,
            None,
            WS_POPUP,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            None,
            None,
            Some(instance.into()),
            None,
        )?
    };

    if window.is_invalid() {
        return Err(WinCliprdrError::from(Error::from_thread()));
    }
    // Init clipboard processing for WinAPI event loop
    //
    // SAFETY: `window` is a valid window handle
    unsafe { AddClipboardFormatListener(window)? };

    let ctx = Box::new(WinClipboardImpl::new(window, message_proxy, backend_rx, files));

    // SAFETY: `window` is a valid window handle, `clipboard_subproc` is in the static memory,
    // `ctx` is valid and its ownership is transferred to the subclass via `into_raw`.
    let winapi_result = unsafe {
        SetWindowSubclass(
            window,
            Some(clipboard_subproc),
            0,
            Box::into_raw(ctx).expose_provenance(),
        )
    };

    if winapi_result == FALSE {
        return Err(WinCliprdrError::WindowSubclass);
    }

    Ok(window)
}

/// Windows-specific clipboard backend factory
struct WinCliprdrBackendFactory {
    tx: mpsc_sync::SyncSender<BackendEvent>,
    window: HWND,
    files: SharedFileList,
    proxy: SharedProxy,
}

// SAFETY: window handle is thread safe for PostMessageW usage
unsafe impl Send for WinCliprdrBackendFactory {}

impl CliprdrBackendFactory for WinCliprdrBackendFactory {
    fn build_cliprdr_backend(&self) -> Box<dyn CliprdrBackend> {
        Box::new(WinCliprdrBackend::new(
            self.window,
            self.tx.clone(),
            Arc::clone(&self.files),
            Arc::clone(&self.proxy),
        ))
    }
}
