use std::sync::Arc;

use nyquest_interface::blocking::{
    BlockingBackend, BlockingClient, BlockingResponse, BoxedStream, Request,
};
use nyquest_interface::client::ClientOptions;
use nyquest_interface::Result as NyquestResult;
use objc2::runtime::ProtocolObject;

pub(crate) mod waker;

use crate::blocking::waker::BlockingWaker;
use crate::client::NSUrlSessionClient;
use crate::datatask::{DataTaskDelegate, GenericWaker};
use crate::error::IntoNyquestResult;
use crate::response::NSUrlSessionResponse;
use crate::NSUrlSessionBackend;

thread_local! {
    static REQUEST_CONTROL: std::cell::RefCell<Option<RequestControl>> =
        std::cell::RefCell::new(None);
}

#[derive(Clone)]
struct RequestControl {
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
    paused: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

fn with_control<T>(control: RequestControl, operation: impl FnOnce() -> T) -> T {
    struct Restore(Option<RequestControl>);
    impl Drop for Restore {
        fn drop(&mut self) {
            REQUEST_CONTROL.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(REQUEST_CONTROL.with(|slot| slot.replace(Some(control))));
    operation()
}

/// Suspend/resume active tasks without replacing their request/body streams.
pub fn with_request_control<T>(
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
    paused: Arc<dyn Fn() -> bool + Send + Sync>,
    operation: impl FnOnce() -> T,
) -> T {
    with_control(
        RequestControl {
            cancelled,
            paused: Some(paused),
        },
        operation,
    )
}

fn interrupted() -> nyquest_interface::Error {
    nyquest_interface::Error::Io(std::io::Error::new(
        std::io::ErrorKind::Interrupted,
        "Upload interrupted",
    ))
}

fn poll_control(
    control: Option<&RequestControl>,
    task: &objc2_foundation::NSURLSessionDataTask,
    suspended: &mut bool,
) -> NyquestResult<bool> {
    if let Some(control) = control {
        if (control.cancelled)() {
            task.cancel();
            return Err(interrupted());
        }
        if control.paused.as_ref().is_some_and(|paused| paused()) {
            if !*suspended {
                task.suspend();
                *suspended = true;
            }
            return Ok(false);
        }
    }
    if *suspended {
        task.resume();
        *suspended = false;
    }
    Ok(true)
}

fn park(control: Option<&RequestControl>) {
    if control.is_some() {
        std::thread::park_timeout(std::time::Duration::from_millis(20));
    } else {
        std::thread::park();
    }
}

/// Scope cancellation to a blocking request on this thread. Checking it while
/// waiting for a response also cancels tasks whose entire body was already sent.
pub fn with_request_cancellation<T>(
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
    operation: impl FnOnce() -> T,
) -> T {
    with_control(
        RequestControl {
            cancelled,
            paused: None,
        },
        operation,
    )
}

#[derive(Clone)]
pub struct NSUrlSessionBlockingClient {
    inner: NSUrlSessionClient,
}
pub struct NSUrlSessionBlockingResponse {
    inner: NSUrlSessionResponse,
    max_response_buffer_size: u64,
    control: Option<RequestControl>,
    user_suspended: bool,
}

impl std::io::Read for NSUrlSessionBlockingResponse {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let inner = &mut self.inner;

        loop {
            if !poll_control(self.control.as_ref(), &inner.task, &mut self.user_suspended).map_err(
                |_| std::io::Error::new(std::io::ErrorKind::Interrupted, "Upload interrupted"),
            )? {
                park(self.control.as_ref());
                continue;
            }
            if let Some(result) = inner.consume_response_to_buffer(buf) {
                return result;
            }

            let inner_waker = coerce_waker(inner.shared.waker_ref());
            inner_waker.register_current_thread();
            unsafe {
                inner.task.resume();
            }
            park(self.control.as_ref());
        }
    }
}

impl BlockingResponse for NSUrlSessionBlockingResponse {
    fn status(&self) -> u16 {
        self.inner.status()
    }

    fn content_length(&self) -> Option<u64> {
        self.inner.content_length()
    }

    fn get_header(&self, header: &str) -> nyquest_interface::Result<Vec<String>> {
        self.inner.get_header(header)
    }

    fn text(&mut self) -> nyquest_interface::Result<String> {
        let bytes = self.bytes()?;
        self.inner.convert_bytes_to_string(bytes)
    }

    fn bytes(&mut self) -> nyquest_interface::Result<Vec<u8>> {
        self.inner
            .shared
            .set_max_response_buffer_size(self.max_response_buffer_size);
        let inner_waker = coerce_waker(self.inner.shared.waker_ref());
        inner_waker.register_current_thread();
        while !poll_control(
            self.control.as_ref(),
            &self.inner.task,
            &mut self.user_suspended,
        )? {
            park(self.control.as_ref());
        }
        unsafe {
            self.inner.task.resume();
        }
        inner_waker.register_current_thread();

        loop {
            if !poll_control(
                self.control.as_ref(),
                &self.inner.task,
                &mut self.user_suspended,
            )? {
                park(self.control.as_ref());
                continue;
            }
            if self.inner.shared.is_completed() {
                break;
            }
            park(self.control.as_ref());
        }
        let res = self.inner.shared.take_response_buffer()?;
        unsafe {
            self.inner.task.error().into_nyquest_result()?;
        }
        Ok(res)
    }
}

impl BlockingClient for NSUrlSessionBlockingClient {
    type Response = NSUrlSessionBlockingResponse;

    fn request(&self, req: Request) -> nyquest_interface::Result<Self::Response> {
        let control = REQUEST_CONTROL.with(|slot| slot.borrow().clone());
        // Do not start a new request while a retry is held by a manual pause.
        while let Some(c) = &control {
            if (c.cancelled)() {
                return Err(interrupted());
            }
            if !c.paused.as_ref().is_some_and(|paused| paused()) {
                break;
            }
            park(control.as_ref());
        }
        let waker = GenericWaker::Blocking(Arc::new(BlockingWaker::new_from_current_thread()));
        let (task, mut writer) = self.inner.build_data_task(req, &waker, |s| match &s {
            BoxedStream::Sized { content_length, .. } => Some(*content_length),
            BoxedStream::Unsized { .. } => None,
        })?;
        let shared = unsafe {
            let delegate = DataTaskDelegate::new(waker, self.inner.allow_redirects);
            task.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            task.resume();
            DataTaskDelegate::into_shared(delegate)
        };
        let mut user_suspended = false;
        loop {
            use std::io::Read as _;

            if !poll_control(control.as_ref(), &task, &mut user_suspended)? {
                park(control.as_ref());
                continue;
            }

            if let Some(response) = shared.try_take_response().into_nyquest_result()? {
                return Ok(NSUrlSessionBlockingResponse {
                    inner: NSUrlSessionResponse {
                        response,
                        task,
                        shared,
                    },
                    max_response_buffer_size: self.inner.max_response_buffer_size,
                    control,
                    user_suspended: false,
                });
            }
            unsafe {
                task.error().into_nyquest_result()?;
            }

            // FIXME: use dispatch2 to perform blocking read in background
            if let Some(stream_writer) = &mut writer {
                let write_result = stream_writer
                    .poll_progress(|stream, buf| std::task::Poll::Ready(stream.read(buf)));
                match write_result {
                    Ok(_) => {}
                    Err(e) => {
                        // Explicitly cancel the NSURLSession task so the network connection
                        // is torn down immediately rather than waiting for the async
                        // NSStreamEvent::ErrorOccurred to propagate through the run loop.
                        task.cancel();
                        return Err(nyquest_interface::Error::Io(e));
                    }
                }
            }
            park(control.as_ref());
        }
    }
}

impl BlockingBackend for NSUrlSessionBackend {
    type BlockingClient = NSUrlSessionBlockingClient;

    fn create_blocking_client(
        &self,
        options: ClientOptions,
    ) -> NyquestResult<Self::BlockingClient> {
        Ok(NSUrlSessionBlockingClient {
            inner: NSUrlSessionClient::create(options)?,
        })
    }
}

#[allow(irrefutable_let_patterns)]
fn coerce_waker(waker: &GenericWaker) -> &BlockingWaker {
    if let GenericWaker::Blocking(waker) = waker {
        waker
    } else {
        unreachable!("should not be called in blocking context")
    }
}
