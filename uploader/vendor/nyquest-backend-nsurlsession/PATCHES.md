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

## ns_stream.rs — deliver EOF after releasing and draining the buffer

**File:** `src/stream/ns_stream.rs`, in `InputStream::update_buffer()` and
`InputStream::callback_read_maxLength()`

**Problem:** `EndEncountered` was queued while `stream_buffer` was still locked.
An active Foundation run loop can deliver that event before the producer
releases the lock. A delegate querying `streamStatus` then gets `Open`, because
the status method uses `try_lock`, instead of the required `AtEnd`. The HTTP
request can remain blocked even after the server has received the complete body.

**Fix:** Release the buffer lock before queuing `EndEncountered`, matching the
existing handling of `HasBytesAvailable` and `ErrorOccurred`. If EOF is discovered
while bytes are still buffered, deliver `EndEncountered` after the consumer
drains those final bytes. Previously this path set `eof` without ever queuing
the EOF event, and later producer polls returned early because `eof` was set.

**Regression:** A real Foundation run loop processes repeated EOF events while
the producer updates the stream. The delegate must always observe `AtEnd`.
The unpatched implementation reproduces `Open` during an EOF callback.
The buffered-tail case also checks the bytes read and reproduces a missing EOF
notification before the consumer-side fix.
Reverting only these two hunks in an otherwise fixed tree reproduces the iOS end
to end stall (`"Upload completed" is visible` after three minutes, with the
worker blocked inside the backend request) in about one of two runs, which is
why this override is carried rather than relying on the uploader's timeouts.
Run it with the uploader's existing lockfile:

```sh
cargo test --locked --manifest-path uploader/Cargo.toml \
  -p nyquest-backend-nsurlsession --lib stream::ns_stream::tests
```

**Upstream:** Check this behavior when upgrading the vendored backend and remove
the override once the upstream implementation releases the lock before EOF.
