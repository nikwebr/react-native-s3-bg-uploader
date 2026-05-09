# Local patches to nyquest-backend-nsurlsession 0.3.1

## blocking.rs — explicit task.cancel() on stream body error

**File:** `src/blocking.rs`, in `NSUrlSessionBlockingClient::request()`

**Problem:** When the request body stream returns an error (e.g. because
`ProgressReader::read()` returns `Err(Interrupted)` after the pause flag is
set), the original code returned the error immediately but only *released*
the `NSURLSessionDataTask` (decremented its Obj-C ref count).  NSURLSession
holds its own strong reference to the task and kept the HTTP connection alive.

The only stop mechanism was an async `NSStreamEvent::ErrorOccurred` event
queued on NSURLSession's internal run loop.  In certain race conditions the
event can be silently dropped (if the stream's delegate or run loop reference
has already been cleared), leaving the task active until the 300-second
request timeout fires.

**Fix:** Call `task.cancel()` explicitly before returning the error so the
network connection is torn down immediately.

**Upstream:** Should be reported / proposed to the nyquest project.
When upgrading to a newer version of nyquest-backend-nsurlsession, check
whether the fix has been merged and, if so, remove this vendor override.
