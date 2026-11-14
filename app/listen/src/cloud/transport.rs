// Native Message WebSocket, using the already pinned Windows binding. TLS
// validation stays enabled; authorization is never redirected to another host.
use super::{AliSettings, CloudError, MessageSocket, MessageTransport, Secret};
use std::{
    ffi::c_void,
    sync::{Arc, mpsc},
    thread,
};
use windows::{Win32::Networking::WinHttp::*, core::PCWSTR};

pub struct NativeMessageTransport;
impl MessageTransport for NativeMessageTransport {
    // Deliberately no verified_bounds override: Message currently has no proved
    // token/output ceiling. The application refuses before reaching connect.
    fn connect(
        &self,
        settings: &AliSettings,
        key: &Secret,
    ) -> Result<Box<dyn MessageSocket>, CloudError> {
        settings.validate()?;
        let session = Handle::new(unsafe {
            WinHttpOpen(
                PCWSTR(wide("Listen cloud trial").as_ptr()),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            )
        })?;
        unsafe { WinHttpSetTimeouts(session.0, 5_000, 5_000, 5_000, 15_000) }
            .map_err(|_| CloudError::Network)?;
        let host = wide(&format!(
            "{}.cn-beijing.maas.aliyuncs.com",
            settings.workspace_id
        ));
        let connection =
            Handle::new(unsafe { WinHttpConnect(session.0, PCWSTR(host.as_ptr()), 443, 0) })?;
        let request = Handle::new(unsafe {
            WinHttpOpenRequest(
                connection.0,
                PCWSTR(wide("GET").as_ptr()),
                PCWSTR(wide("/api-ws/v1/inference").as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            )
        })?;
        unsafe {
            WinHttpSetOption(Some(request.0), WINHTTP_OPTION_UPGRADE_TO_WEB_SOCKET, None)
                .map_err(|_| CloudError::Network)?;
            WinHttpSetOption(
                Some(request.0),
                WINHTTP_OPTION_REDIRECT_POLICY,
                Some(&WINHTTP_OPTION_REDIRECT_POLICY_NEVER.to_ne_bytes()),
            )
            .map_err(|_| CloudError::Network)?;
        }
        // The only secret use is this upgrade header. Do not include arbitrary
        // headers, transcript history, window data or a data-inspection override.
        let mut authorization: Vec<u16> = format!("Authorization: Bearer {}\r\n", key.0)
            .encode_utf16()
            .collect();
        let sent = unsafe { WinHttpSendRequest(request.0, Some(&authorization), None, 0, 0, 0) };
        for code in &mut authorization {
            unsafe { std::ptr::write_volatile(code, 0) };
        }
        sent.map_err(|_| CloudError::Network)?;
        unsafe { WinHttpReceiveResponse(request.0, std::ptr::null_mut()) }
            .map_err(|_| CloudError::Network)?;
        let mut status = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        unsafe {
            WinHttpQueryHeaders(
                request.0,
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
        let socket = Handle::new(unsafe { WinHttpWebSocketCompleteUpgrade(request.0, None) })?;
        unsafe {
            WinHttpSetOption(
                Some(socket.0),
                WINHTTP_OPTION_WEB_SOCKET_CLOSE_TIMEOUT,
                Some(&500u32.to_ne_bytes()),
            )
        }
        .map_err(|_| CloudError::Network)?;
        drop(request);
        let handles = Arc::new(Handles {
            socket,
            _connection: connection,
            _session: session,
        });
        let receive_handles = handles.clone();
        let (events, input) = mpsc::sync_channel(8);
        thread::spawn(move || {
            let mut message = Utf8Message::default();
            loop {
                let event = receive(&receive_handles.socket, &mut message);
                let failed = event.is_err();
                match event {
                    Ok(Some(text)) => {
                        if events.send(Ok(text)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = events.send(Err(error));
                    }
                }
                if failed {
                    break;
                }
            }
        });
        Ok(Box::new(Socket { handles, input }))
    }
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
// WinHTTP explicitly permits one serialized send and one receive concurrently.
// Handles are never reused after close; their owning Arc outlives both calls.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}
struct Handles {
    socket: Handle,
    _connection: Handle,
    _session: Handle,
}
struct Socket {
    handles: Arc<Handles>,
    input: mpsc::Receiver<Result<String, CloudError>>,
}
impl MessageSocket for Socket {
    fn send_text(&mut self, text: &str) -> Result<(), CloudError> {
        send(
            &self.handles.socket,
            WINHTTP_WEB_SOCKET_UTF8_MESSAGE_BUFFER_TYPE,
            text.as_bytes(),
        )
    }
    fn send_audio(&mut self, pcm: &[u8]) -> Result<(), CloudError> {
        send(
            &self.handles.socket,
            WINHTTP_WEB_SOCKET_BINARY_MESSAGE_BUFFER_TYPE,
            pcm,
        )
    }
    fn poll(&mut self) -> Result<Option<String>, CloudError> {
        match self.input.try_recv() {
            Ok(event) => event.map(Some),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(CloudError::Network),
        }
    }
}
impl Drop for Socket {
    fn drop(&mut self) {
        // Whole-task confirmation is processed before any close handshake. Close
        // has its own bound and cancels pending receive; it cannot delay delivery.
        let handles = self.handles.clone();
        thread::spawn(move || unsafe {
            let _ = WinHttpWebSocketClose(handles.socket.0, 1000, None, 0);
        });
    }
}
fn send(
    socket: &Handle,
    kind: WINHTTP_WEB_SOCKET_BUFFER_TYPE,
    bytes: &[u8],
) -> Result<(), CloudError> {
    if unsafe { WinHttpWebSocketSend(socket.0, kind, Some(bytes)) } == 0 {
        Ok(())
    } else {
        Err(CloudError::Network)
    }
}
fn receive(socket: &Handle, message: &mut Utf8Message) -> Result<Option<String>, CloudError> {
    let mut buffer = [0u8; 16 * 1024];
    let mut read = 0;
    let mut kind = WINHTTP_WEB_SOCKET_BUFFER_TYPE::default();
    if unsafe {
        WinHttpWebSocketReceive(
            socket.0,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            &mut read,
            &mut kind,
        )
    } != 0
    {
        return Err(CloudError::Network);
    }
    match kind {
        WINHTTP_WEB_SOCKET_UTF8_FRAGMENT_BUFFER_TYPE => {
            message.push(&buffer[..read as usize], false)
        }
        WINHTTP_WEB_SOCKET_UTF8_MESSAGE_BUFFER_TYPE => message.push(&buffer[..read as usize], true),
        WINHTTP_WEB_SOCKET_CLOSE_BUFFER_TYPE => Err(CloudError::Network),
        _ => Err(CloudError::Protocol),
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
