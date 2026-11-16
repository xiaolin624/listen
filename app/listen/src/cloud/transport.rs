// Native Message WebSocket, using the already pinned Windows binding. TLS
// validation stays enabled; authorization is never redirected to another host.
use super::{
    AliSettings, CloudError, FreeOnlyStatusSource, MessageSocket, MessageTransport, Secret,
    TrialCostPolicy,
    free_only::{CliStatusSource, verify},
};
use std::{
    ffi::c_void,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use windows::{Win32::Networking::WinHttp::*, core::PCWSTR};

pub struct NativeMessageTransport {
    status: Arc<dyn FreeOnlyStatusSource>,
}
impl Default for NativeMessageTransport {
    fn default() -> Self {
        Self::with_status_source(Arc::new(CliStatusSource))
    }
}
impl NativeMessageTransport {
    pub fn with_status_source(status: Arc<dyn FreeOnlyStatusSource>) -> Self {
        Self { status }
    }
}
impl MessageTransport for NativeMessageTransport {
    fn requires_live_cost_check(&self) -> bool {
        true
    }
    fn cost_policy(
        &self,
        settings: &AliSettings,
        _key: &Secret,
    ) -> Result<TrialCostPolicy, CloudError> {
        verify(self.status.as_ref(), settings).map(TrialCostPolicy::FreeOnly)
    }
    fn connect(
        &self,
        settings: &AliSettings,
        key: &Secret,
    ) -> Result<Box<dyn MessageSocket>, CloudError> {
        settings.validate()?;
        let host = format!("{}.cn-beijing.maas.aliyuncs.com", settings.workspace_id);
        // This upgrade header is the only secret use. No arbitrary headers,
        // history, window data or TLS validation override enter this path.
        let mut authorization: Vec<u16> = format!("Authorization: Bearer {}\r\n", key.0)
            .encode_utf16()
            .collect();
        let result = connect(
            &host,
            443,
            WINHTTP_FLAG_SECURE,
            &authorization,
            Limits::default(),
        );
        for code in &mut authorization {
            unsafe { std::ptr::write_volatile(code, 0) };
        }
        result.map(|socket| Box::new(socket) as Box<dyn MessageSocket>)
    }
}

#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub handshake: Duration,
    pub send: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            handshake: Duration::from_secs(15),
            send: Duration::from_secs(5),
        }
    }
}
// Parent visibility permits a local diagnostic to compile the actual adapter
// against loopback. Production has one fixed WSS host/path above, with no
// environment/configuration endpoint override.
pub(super) fn connect(
    host: &str,
    port: u16,
    flags: WINHTTP_OPEN_REQUEST_FLAGS,
    authorization: &[u16],
    limits: Limits,
) -> Result<Socket, CloudError> {
    let started = Instant::now();
    let session = Handle::new(unsafe {
        WinHttpOpen(
            PCWSTR(wide("Listen cloud trial").as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            WINHTTP_FLAG_ASYNC,
        )
    })?;
    unsafe { WinHttpSetTimeouts(session.0, 5_000, 5_000, 5_000, 15_000) }
        .map_err(|_| CloudError::Network)?;
    let connection =
        Handle::new(unsafe { WinHttpConnect(session.0, PCWSTR(wide(host).as_ptr()), port, 0) })?;
    let request = AsyncHandle::new(Handle::new(unsafe {
        WinHttpOpenRequest(
            connection.0,
            PCWSTR(wide("GET").as_ptr()),
            PCWSTR(wide("/api-ws/v1/inference").as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            flags,
        )
    })?)?;
    unsafe {
        WinHttpSetOption(
            Some(request.raw()?),
            WINHTTP_OPTION_UPGRADE_TO_WEB_SOCKET,
            None,
        )
        .map_err(|_| CloudError::Network)?;
        WinHttpSetOption(
            Some(request.raw()?),
            WINHTTP_OPTION_REDIRECT_POLICY,
            Some(&WINHTTP_OPTION_REDIRECT_POLICY_NEVER.to_ne_bytes()),
        )
        .map_err(|_| CloudError::Network)?;
        // This API copies exactly this slice synchronously. SendRequest owns no
        // borrowed header/body memory across its asynchronous completion.
        WinHttpAddRequestHeaders(request.raw()?, authorization, WINHTTP_ADDREQ_FLAG_ADD)
            .map_err(|_| CloudError::Network)?;
        WinHttpSendRequest(request.raw()?, None, None, 0, 0, request.context())
            .map_err(|_| CloudError::Network)?;
    }
    request.wait(&request.callback.sent_request, started, limits.handshake)?;
    unsafe { WinHttpReceiveResponse(request.raw()?, std::ptr::null_mut()) }
        .map_err(|_| CloudError::Network)?;
    request.wait(&request.callback.headers, started, limits.handshake)?;
    let mut status = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    unsafe {
        WinHttpQueryHeaders(
            request.raw()?,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status as *mut u32).cast()),
            &mut size,
            std::ptr::null_mut(),
        )
    }
    .map_err(|_| CloudError::Network)?;
    match status {
        101 => {}
        401 | 403 => return Err(CloudError::Unauthorized),
        402 => return Err(CloudError::BillingUnavailable),
        404 => return Err(CloudError::ModelUnavailable),
        429 => return Err(CloudError::RateOrBudget),
        _ => return Err(CloudError::Provider),
    }
    let callback = Arc::new(Callback::default());
    // CompleteUpgrade inherits the callback, with its own context rather than
    // the request's. Each native handle owns one separate strong reference.
    let context = Arc::into_raw(callback.clone()) as usize;
    let raw = unsafe { WinHttpWebSocketCompleteUpgrade(request.raw()?, Some(context)) };
    if raw.is_null() {
        unsafe { drop(Arc::from_raw(context as *const Callback)) };
        return Err(CloudError::Network);
    }
    let socket = AsyncHandle {
        handle: Some(Handle(raw)),
        callback,
    };
    drop(request);
    Ok(Socket {
        socket,
        _connection: connection,
        _session: session,
        receiving: false,
        message: Utf8Message::default(),
        limits,
    })
}

struct Handle(*mut c_void);
impl Handle {
    fn new(raw: *mut c_void) -> Result<Self, CloudError> {
        if raw.is_null() {
            Err(CloudError::Network)
        } else {
            Ok(Self(raw))
        }
    }
}
// A handle has one owning worker. Only WinHTTP's callbacks run concurrently;
// they publish metadata and never call a WinHTTP API or access the buffers.
unsafe impl Send for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}
#[derive(Default)]
struct Buffers {
    sending: Option<Box<[u8]>>,
    receiving: Option<Box<ReceiveBuffer>>,
}
struct ReceiveBuffer {
    bytes: [u8; 16 * 1024],
    // Required API outputs are ignored in async mode. Keep them alive with the
    // input buffer too; completion data is read only from the callback status.
    count: u32,
    kind: WINHTTP_WEB_SOCKET_BUFFER_TYPE,
}
#[derive(Default)]
struct Callback {
    sent_request: AtomicBool,
    headers: AtomicBool,
    written: AtomicBool,
    written_bytes: AtomicU32,
    read: AtomicBool,
    read_bytes: AtomicU32,
    read_kind: AtomicI32,
    failed: AtomicBool,
    buffers: Mutex<Buffers>,
}
unsafe extern "system" fn callback(
    _handle: *mut c_void,
    context: usize,
    status: u32,
    information: *mut c_void,
    length: u32,
) {
    if context == 0 {
        return;
    }
    // HANDLE_CLOSING is last and excludes other callbacks for this handle.
    // Until then its raw Arc keeps context AND buffers alive, including after
    // the application has timed out and dropped the socket.
    if status == WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING {
        unsafe { drop(Arc::from_raw(context as *const Callback)) };
        return;
    }
    let callback = unsafe { &*(context as *const Callback) };
    // Constant non-blocking work: no locks, waits, I/O, allocations, application
    // events, recursive API calls or remotely supplied text.
    match status {
        WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE => {
            callback.sent_request.store(true, Ordering::Release)
        }
        WINHTTP_CALLBACK_STATUS_HEADERS_AVAILABLE => {
            callback.headers.store(true, Ordering::Release)
        }
        WINHTTP_CALLBACK_STATUS_WRITE_COMPLETE | WINHTTP_CALLBACK_STATUS_READ_COMPLETE => {
            if !information.is_null()
                && length as usize == std::mem::size_of::<WINHTTP_WEB_SOCKET_STATUS>()
            {
                let read = unsafe { &*information.cast::<WINHTTP_WEB_SOCKET_STATUS>() };
                if status == WINHTTP_CALLBACK_STATUS_WRITE_COMPLETE {
                    callback
                        .written_bytes
                        .store(read.dwBytesTransferred, Ordering::Relaxed);
                    callback.written.store(true, Ordering::Release);
                } else {
                    callback
                        .read_bytes
                        .store(read.dwBytesTransferred, Ordering::Relaxed);
                    callback
                        .read_kind
                        .store(read.eBufferType.0, Ordering::Relaxed);
                    callback.read.store(true, Ordering::Release);
                }
            } else {
                callback.failed.store(true, Ordering::Release);
            }
        }
        WINHTTP_CALLBACK_STATUS_REQUEST_ERROR => callback.failed.store(true, Ordering::Release),
        _ => {}
    }
}
struct AsyncHandle {
    handle: Option<Handle>,
    callback: Arc<Callback>,
}
impl AsyncHandle {
    fn new(handle: Handle) -> Result<Self, CloudError> {
        let previous = unsafe {
            WinHttpSetStatusCallback(
                handle.0,
                Some(callback),
                WINHTTP_CALLBACK_FLAG_ALL_NOTIFICATIONS,
                0,
            )
        };
        if previous.is_some_and(|pointer| pointer as usize == usize::MAX) {
            return Err(CloudError::Network);
        }
        let callback = Arc::new(Callback::default());
        let context = Arc::into_raw(callback.clone()) as usize;
        if unsafe {
            WinHttpSetOption(
                Some(handle.0),
                WINHTTP_OPTION_CONTEXT_VALUE,
                Some(&context.to_ne_bytes()),
            )
        }
        .is_err()
        {
            // Context was not installed: closing this handle calls back with 0.
            unsafe { drop(Arc::from_raw(context as *const Callback)) };
            return Err(CloudError::Network);
        }
        Ok(Self {
            handle: Some(handle),
            callback,
        })
    }
    fn context(&self) -> usize {
        Arc::as_ptr(&self.callback) as usize
    }
    fn raw(&self) -> Result<*mut c_void, CloudError> {
        self.handle.as_ref().map(|h| h.0).ok_or(CloudError::Network)
    }
    fn wait(
        &self,
        complete: &AtomicBool,
        started: Instant,
        limit: Duration,
    ) -> Result<(), CloudError> {
        loop {
            if self.callback.failed.load(Ordering::Acquire) {
                return Err(CloudError::Network);
            }
            if complete.load(Ordering::Acquire) {
                return Ok(());
            }
            if started.elapsed() >= limit {
                return Err(CloudError::Timeout);
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
    fn cancel(&mut self) {
        // No native API call is on this owning worker's stack. Close the actual
        // pending child, not just its session. Late completions retain data.
        drop(self.handle.take());
    }
}
pub(super) struct Socket {
    socket: AsyncHandle,
    _connection: Handle,
    _session: Handle,
    receiving: bool,
    message: Utf8Message,
    limits: Limits,
}
impl Socket {
    fn send(
        &mut self,
        kind: WINHTTP_WEB_SOCKET_BUFFER_TYPE,
        bytes: &[u8],
    ) -> Result<(), CloudError> {
        // A cancelled socket must not replace buffers still owned by WinHTTP.
        let handle = self.socket.raw()?;
        if self.socket.callback.failed.load(Ordering::Acquire) {
            return Err(CloudError::Network);
        }
        let length = u32::try_from(bytes.len()).map_err(|_| CloudError::Protocol)?;
        let context = &self.socket.callback;
        context.written.store(false, Ordering::Release);
        let pointer = {
            let mut buffers = context.buffers.lock().map_err(|_| CloudError::Network)?;
            buffers.sending = Some(bytes.into());
            buffers.sending.as_ref().unwrap().as_ptr()
        };
        let started = Instant::now();
        let result = if unsafe {
            WinHttpWebSocketSend(
                handle,
                kind,
                Some(std::slice::from_raw_parts(pointer, length as usize)),
            )
        } == 0
        {
            self.socket
                .wait(&context.written, started, self.limits.send)
                .and_then(|()| {
                    if context.written_bytes.load(Ordering::Relaxed) == length {
                        Ok(())
                    } else {
                        Err(CloudError::Protocol)
                    }
                })
        } else {
            Err(CloudError::Network)
        };
        if result.is_ok() {
            // Both the initiating call and its completion have ended.
            context
                .buffers
                .lock()
                .map_err(|_| CloudError::Network)?
                .sending = None;
        } else {
            self.socket.cancel();
        }
        result
    }
    fn receive(&mut self) -> Result<Option<String>, CloudError> {
        let handle = self.socket.raw()?;
        let context = &self.socket.callback;
        if context.failed.load(Ordering::Acquire) {
            return Err(CloudError::Network);
        }
        if self.receiving {
            if !context.read.load(Ordering::Acquire) {
                // With intermediate results disabled, a healthy long sentence
                // may have no application frames while audio keeps uploading.
                // The application bounds start/final confirmation and the
                // whole task; polling pending I/O itself never waits.
                return Ok(None);
            }
            self.receiving = false;
            let bytes = context.read_bytes.load(Ordering::Relaxed) as usize;
            let kind = WINHTTP_WEB_SOCKET_BUFFER_TYPE(context.read_kind.load(Ordering::Relaxed));
            let buffers = context.buffers.lock().map_err(|_| CloudError::Network)?;
            let buffer = buffers.receiving.as_ref().ok_or(CloudError::Protocol)?;
            let read = buffer.bytes.get(..bytes).ok_or(CloudError::Protocol)?;
            return match kind {
                WINHTTP_WEB_SOCKET_UTF8_FRAGMENT_BUFFER_TYPE => self.message.push(read, false),
                WINHTTP_WEB_SOCKET_UTF8_MESSAGE_BUFFER_TYPE => self.message.push(read, true),
                WINHTTP_WEB_SOCKET_CLOSE_BUFFER_TYPE => Err(CloudError::Network),
                _ => Err(CloudError::Protocol),
            };
        }
        let (pointer, count, kind) = {
            let mut buffers = context.buffers.lock().map_err(|_| CloudError::Network)?;
            let buffer = buffers.receiving.get_or_insert_with(|| {
                Box::new(ReceiveBuffer {
                    bytes: [0; 16 * 1024],
                    count: 0,
                    kind: WINHTTP_WEB_SOCKET_BUFFER_TYPE::default(),
                })
            });
            (
                buffer.bytes.as_mut_ptr(),
                &mut buffer.count as *mut u32,
                &mut buffer.kind as *mut WINHTTP_WEB_SOCKET_BUFFER_TYPE,
            )
        };
        context.read.store(false, Ordering::Release);
        self.receiving = true;
        if unsafe { WinHttpWebSocketReceive(handle, pointer.cast(), 16 * 1024, count, kind) } != 0 {
            return Err(CloudError::Network);
        }
        Ok(None)
    }
}
impl MessageSocket for Socket {
    fn send_text(&mut self, text: &str) -> Result<(), CloudError> {
        self.send(WINHTTP_WEB_SOCKET_UTF8_MESSAGE_BUFFER_TYPE, text.as_bytes())
    }
    fn send_audio(&mut self, pcm: &[u8]) -> Result<(), CloudError> {
        self.send(WINHTTP_WEB_SOCKET_BINARY_MESSAGE_BUFFER_TYPE, pcm)
    }
    fn poll(&mut self) -> Result<Option<String>, CloudError> {
        let result = self.receive();
        if result.is_err() {
            self.socket.cancel();
        }
        result
    }
}
impl Drop for Socket {
    fn drop(&mut self) {
        // WebSocketClose uses both directions and cannot overlap pending I/O.
        // Abort via CloseHandle instead, without a blocking close handshake or
        // an orphan receive/shutdown thread. Buffers outlive cancellation.
        self.socket.cancel();
    }
}
#[derive(Default)]
struct Utf8Message(Vec<u8>);
impl Utf8Message {
    fn push(&mut self, bytes: &[u8], complete: bool) -> Result<Option<String>, CloudError> {
        if self.0.len().saturating_add(bytes.len()) > 1024 * 1024 {
            return Err(CloudError::Protocol);
        }
        self.0.extend_from_slice(bytes);
        if !complete {
            return Ok(None);
        }
        String::from_utf8(std::mem::take(&mut self.0))
            .map(Some)
            .map_err(|_| CloudError::Protocol)
    }
}
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
