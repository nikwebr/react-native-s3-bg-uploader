#![allow(non_snake_case)]

use std::io::{self, Cursor};
use std::ops::ControlFlow;
use std::ptr::{null_mut, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use arc_swap::ArcSwapAny;
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, ClassType, DefinedClass, Message as _};
use objc2_foundation::{
    NSArray, NSError, NSInputStream, NSInteger, NSObjectProtocol, NSRunLoop, NSRunLoopMode,
    NSStreamDelegate, NSStreamEvent, NSStreamPropertyKey, NSStreamStatus, NSString, NSUInteger,
};

use crate::datatask::GenericWaker;
use crate::retained_ext::SwappableRetained;

#[cfg(target_os = "macos")]
const STREAM_BUFFER_SIZE: usize = 1024 * 64;
#[cfg(not(target_os = "macos"))]
const STREAM_BUFFER_SIZE: usize = 1024 * 16;

pub(crate) struct InputStreamIvars {
    waker: GenericWaker,
    delegate: ArcSwapAny<Option<SwappableRetained<ProtocolObject<dyn NSStreamDelegate>>>>,
    run_loop: ArcSwapAny<Option<SwappableRetained<NSRunLoop>>>,
    run_loop_mode: ArcSwapAny<Option<SwappableRetained<NSRunLoopMode>>>,
    stream_buffer: Mutex<Result<Cursor<Vec<u8>>, Retained<NSError>>>,
    is_open: AtomicBool,
    eof: AtomicBool,
}

define_class!(
    // SAFETY:
    // - The superclass NSObject does not have any subclassing requirements.
    // - `Delegate` does not implement `Drop`.
    #[unsafe(super = NSInputStream)]
    // #[thread_kind = MainThreadOnly]
    #[name = "Nyquest_InputStream"]
    #[ivars = InputStreamIvars]
    pub(crate) struct InputStream;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for InputStream {}

    impl InputStream {
        #[unsafe(method(open))]
        fn __open(&self) {
            self.callback_open();
        }

        #[unsafe(method(close))]
        fn __close(&self) {
            self.callback_close();
        }

        #[unsafe(method(setDelegate:))]
        fn setDelegate(&self, delegate: Option<&ProtocolObject<dyn NSStreamDelegate>>) {
            self.callback_setDelegate(delegate);
        }

        #[unsafe(method(propertyForKey:))]
        fn propertyForKey(
            &self,
            _key: &NSStreamPropertyKey,
        ) -> *mut AnyObject {
            // __kCFStreamPropertyHTTPTrailer
            null_mut()
        }

        #[unsafe(method(setProperty:forKey:))]
        fn setProperty_forKey(
            &self,
            _property: Option<&AnyObject>,
            _key: &NSStreamPropertyKey,
        ) -> bool {
            false
        }

        #[unsafe(method(scheduleInRunLoop:forMode:))]
        fn scheduleInRunLoop_forMode(
            &self,
            a_run_loop: &NSRunLoop,
            mode: &NSRunLoopMode,
        ) {
            self.callback_scheduleInRunLoop_forMode(a_run_loop, mode);
        }

        #[unsafe(method(removeFromRunLoop:forMode:))]
        fn removeFromRunLoop_forMode(
            &self,
            a_run_loop: &NSRunLoop,
            mode: &NSRunLoopMode,
        ) {
            self.callback_removeFromRunLoop_forMode(a_run_loop, mode);
        }

        #[unsafe(method(streamStatus))]
        fn streamStatus(&self) -> NSStreamStatus {
            self.callback_streamStatus()
        }

        #[unsafe(method(streamError))]
        fn streamError(&self) -> *mut NSError {
            self.callback_streamError()
        }

        #[unsafe(method(read:maxLength:))]
        fn read_maxLength(&self, buffer: NonNull<u8>, len: NSUInteger) -> NSInteger {
            self.callback_read_maxLength(buffer, len)
        }

        #[unsafe(method(hasBytesAvailable))]
        fn hasBytesAvailable(&self) -> bool {
            self.callback_hasBytesAvailable()
        }
    }
);

impl InputStream {
    pub(crate) fn new(waker: GenericWaker) -> Retained<Self> {
        let this = Self::alloc().set_ivars(InputStreamIvars {
            waker,
            delegate: ArcSwapAny::new(None),
            run_loop: ArcSwapAny::new(None),
            run_loop_mode: ArcSwapAny::new(None),
            stream_buffer: Mutex::new(Ok(Cursor::new(vec![0; STREAM_BUFFER_SIZE]))),
            is_open: AtomicBool::new(false),
            eof: AtomicBool::new(false),
        });

        unsafe { msg_send![super(this), init] }
    }
    pub(crate) fn update_buffer(
        &self,
        cb: impl FnOnce(&mut [u8]) -> ControlFlow<(), io::Result<usize>>,
    ) -> io::Result<()> {
        let ivars = self.ivars();
        if ivars.eof.load(Ordering::SeqCst) {
            return Ok(());
        }
        let mut stream_buffer = ivars.stream_buffer.lock().unwrap();
        let Ok(cursor) = &mut *stream_buffer else {
            return Ok(());
        };

        let pos = cursor.position() as usize;
        let buffer = &mut cursor.get_mut()[pos..];
        if !buffer.is_empty() {
            let read_res = cb(buffer);
            match read_res {
                ControlFlow::Break(()) => {
                    return Ok(());
                }
                ControlFlow::Continue(Ok(0)) => {
                    ivars.eof.store(true, Ordering::SeqCst);
                    if pos == 0 {
                        // The delegate can query streamStatus as soon as the EOF event runs.
                        drop(stream_buffer);
                        self.notify_stream_state(NSStreamEvent::EndEncountered);
                        return Ok(());
                    }
                }
                ControlFlow::Continue(Ok(read_len)) => {
                    cursor.set_position((pos + read_len) as u64);
                }
                ControlFlow::Continue(Err(e)) => {
                    let ns_err = NSError::new(
                        e.raw_os_error().unwrap_or_default() as _,
                        &NSString::from_str(&e.to_string()),
                    );
                    *stream_buffer = Err(ns_err);
                    // Release the lock before notifying delegate
                    drop(stream_buffer);
                    self.notify_stream_state(NSStreamEvent::ErrorOccurred);
                    return Err(e);
                }
            }
        }

        if cursor.position() > 0 {
            // Release the lock before notifying delegate
            drop(stream_buffer);
            self.notify_stream_state(NSStreamEvent::HasBytesAvailable);
        }
        Ok(())
    }

    pub(crate) fn is_open(&self) -> bool {
        self.ivars().is_open.load(Ordering::Relaxed)
    }

    fn notify_stream_state(&self, event: NSStreamEvent) {
        let ivars = self.ivars();
        let Some(delegate) = ivars.delegate.load_full() else {
            return;
        };
        let Some(run_loop) = ivars.run_loop.load_full() else {
            return;
        };
        let Some(run_loop_mode) = ivars.run_loop_mode.load_full() else {
            return;
        };
        unsafe {
            let stream = self.as_super().retain();
            run_loop.performInModes_block(
                &NSArray::from_retained_slice(std::slice::from_ref(&*run_loop_mode)),
                &RcBlock::new(move || {
                    let delegate: &ProtocolObject<dyn NSStreamDelegate> = &*delegate;
                    delegate.stream_handleEvent(&stream, event);
                }),
            );
        }
    }

    fn callback_open(&self) {
        let ivars = self.ivars();
        ivars.is_open.store(true, Ordering::Relaxed);
        ivars.waker.wake();
    }
    fn callback_close(&self) {
        let ivars = self.ivars();
        let mut stream_buffer = ivars.stream_buffer.lock().unwrap();
        let Ok(stream_buffer) = stream_buffer.as_mut() else {
            return;
        };
        stream_buffer.set_position(0);
        *stream_buffer.get_mut() = vec![];
    }
    fn callback_setDelegate(&self, delegate: Option<&ProtocolObject<dyn NSStreamDelegate>>) {
        let delegate = delegate.map(|d| d.retain().into());
        self.ivars().delegate.store(delegate);
    }
    fn callback_scheduleInRunLoop_forMode(&self, a_run_loop: &NSRunLoop, mode: &NSRunLoopMode) {
        let ivars = self.ivars();
        ivars.run_loop.store(Some(a_run_loop.retain().into()));
        ivars.run_loop_mode.store(Some(mode.retain().into()));
    }
    fn callback_removeFromRunLoop_forMode(&self, _a_run_loop: &NSRunLoop, _mode: &NSRunLoopMode) {
        let ivars = self.ivars();
        ivars.run_loop.store(None);
        ivars.run_loop_mode.store(None);
    }
    fn callback_streamStatus(&self) -> NSStreamStatus {
        let ivars = self.ivars();
        if !ivars.is_open.load(Ordering::Relaxed) {
            return NSStreamStatus::NotOpen;
        }
        if let Ok(stream_buffer) = ivars.stream_buffer.try_lock() {
            let eof = ivars.eof.load(Ordering::SeqCst);
            match &*stream_buffer {
                Ok(cursor) if cursor.position() == 0 && eof => {
                    return NSStreamStatus::AtEnd;
                }
                Err(_) => {
                    return NSStreamStatus::Error;
                }
                _ => {}
            }
        }
        NSStreamStatus::Open
    }
    fn callback_streamError(&self) -> *mut NSError {
        let ivars = self.ivars();
        let Ok(stream_buffer) = ivars.stream_buffer.try_lock() else {
            return null_mut();
        };
        match &*stream_buffer {
            Ok(_) => std::ptr::null_mut(),
            Err(error) => Retained::into_raw(error.clone()),
        }
    }
    fn callback_read_maxLength(&self, buffer: NonNull<u8>, len: NSUInteger) -> NSInteger {
        let ivars = self.ivars();
        let Ok(mut stream_buffer) = ivars.stream_buffer.try_lock() else {
            return 0;
        };

        match &mut *stream_buffer {
            Ok(cursor) => {
                ivars.waker.wake();
                let read_len = (cursor.position() as usize).min(len);
                if read_len == 0 {
                    return 0;
                }
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        cursor.get_ref().as_ptr(),
                        buffer.as_ptr(),
                        read_len,
                    );
                    cursor.get_mut().drain(..read_len);
                    // Safety: the underlying buffer will always have STREAM_BUFFER_SIZE initialized bytes
                    cursor.get_mut().set_len(STREAM_BUFFER_SIZE);
                }
                cursor.set_position(cursor.position() - read_len as u64);
                // EOF can be detected before the final buffered bytes are consumed.
                let at_end = cursor.position() == 0 && ivars.eof.load(Ordering::SeqCst);
                drop(stream_buffer);
                if at_end {
                    self.notify_stream_state(NSStreamEvent::EndEncountered);
                }
                read_len as NSInteger
            }
            Err(_) => -1,
        }
    }
    fn callback_hasBytesAvailable(&self) -> bool {
        let ivars = self.ivars();
        let Ok(stream_buffer) = ivars.stream_buffer.try_lock() else {
            return false;
        };
        matches!(&*stream_buffer, Ok(cursor) if cursor.position() > 0)
    }
}

// Safety: ivars are protected by ArcSwapAny Retained and Mutex
unsafe impl Send for InputStream {}
unsafe impl Sync for InputStream {}

#[cfg(all(test, feature = "blocking"))]
mod tests {
    use super::*;
    use std::ffi::c_void;
    use std::sync::{mpsc, Arc};
    use std::thread;
    use std::time::Duration;

    use objc2_core_foundation::{
        kCFRunLoopDefaultMode, CFRunLoop, CFRunLoopSource, CFRunLoopSourceContext,
    };
    use objc2_foundation::{NSDefaultRunLoopMode, NSObject, NSStream};

    use crate::blocking::waker::BlockingWaker;

    define_class!(
        #[unsafe(super = NSObject)]
        #[name = "Nyquest_TestEndDelegate"]
        #[ivars = mpsc::Sender<NSStreamStatus>]
        struct EndDelegate;

        unsafe impl NSObjectProtocol for EndDelegate {}

        unsafe impl NSStreamDelegate for EndDelegate {
            #[unsafe(method(stream:handleEvent:))]
            fn handle_event(&self, stream: &NSStream, event: NSStreamEvent) {
                if event == NSStreamEvent::EndEncountered {
                    self.ivars().send(stream.streamStatus()).unwrap();
                }
            }
        }
    );

    impl EndDelegate {
        fn new(sender: mpsc::Sender<NSStreamStatus>) -> Retained<Self> {
            let this = Self::alloc().set_ivars(sender);
            unsafe { msg_send![super(this), init] }
        }
    }

    unsafe extern "C-unwind" fn idle_source(_: *mut c_void) {}

    #[test]
    fn eof_events_expose_end_status_on_active_run_loop() {
        let (stream_tx, stream_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let running = Arc::new(AtomicBool::new(true));
        let worker_running = running.clone();
        let worker = thread::spawn(move || {
            let run_loop = NSRunLoop::currentRunLoop();
            let cf_loop = run_loop.getCFRunLoop();
            let mut context = CFRunLoopSourceContext {
                version: 0,
                info: null_mut(),
                retain: None,
                release: None,
                copyDescription: None,
                equal: None,
                hash: None,
                schedule: None,
                cancel: None,
                perform: Some(idle_source),
            };
            // Keep the loop alive while polling its queued stream events.
            let source = unsafe { CFRunLoopSource::new(None, 0, &mut context) }.unwrap();
            let mode = unsafe { kCFRunLoopDefaultMode };
            cf_loop.add_source(Some(&source), mode);
            let delegate = EndDelegate::new(event_tx);
            let stream = InputStream::new(GenericWaker::Blocking(Arc::new(
                BlockingWaker::new_from_current_thread(),
            )));
            stream.callback_setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            stream.callback_scheduleInRunLoop_forMode(&run_loop, unsafe { NSDefaultRunLoopMode });
            stream.callback_open();
            stream_tx.send(stream).unwrap();
            while worker_running.load(Ordering::SeqCst) {
                CFRunLoop::run_in_mode(mode, 0.0, false);
            }
            cf_loop.remove_source(Some(&source), mode);
        });
        let stream = stream_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let run_loop = stream.ivars().run_loop.load_full().unwrap();
        let cf_loop = run_loop.getCFRunLoop();
        let mut event = Ok(NSStreamStatus::AtEnd);
        for _ in 0..1024 {
            stream.ivars().eof.store(false, Ordering::SeqCst);
            stream
                .update_buffer(|_| ControlFlow::Continue(Ok(0)))
                .unwrap();
            event = event_rx.recv_timeout(Duration::from_secs(2));
            if event != Ok(NSStreamStatus::AtEnd) {
                break;
            }
        }
        let mut tail = [0; 3];
        let mut tail_read = 0;
        let mut buffered_event = Ok(NSStreamStatus::AtEnd);
        if event == Ok(NSStreamStatus::AtEnd) {
            stream.ivars().eof.store(false, Ordering::SeqCst);
            stream
                .update_buffer(|buffer| {
                    buffer[..3].copy_from_slice(b"end");
                    ControlFlow::Continue(Ok(3))
                })
                .unwrap();
            stream
                .update_buffer(|_| ControlFlow::Continue(Ok(0)))
                .unwrap();
            tail_read = stream.callback_read_maxLength(NonNull::from(&mut tail[0]), tail.len());
            buffered_event = event_rx.recv_timeout(Duration::from_secs(2));
        }
        running.store(false, Ordering::SeqCst);
        cf_loop.stop();
        cf_loop.wake_up();
        worker.join().unwrap();
        assert_eq!(event.unwrap(), NSStreamStatus::AtEnd);
        assert_eq!(tail_read, 3);
        assert_eq!(&tail, b"end");
        assert_eq!(buffered_event.unwrap(), NSStreamStatus::AtEnd);
    }
}
