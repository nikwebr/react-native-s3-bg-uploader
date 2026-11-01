"""Local multipart S3 backend and independent download/decryption assertions."""

import argparse
from collections import Counter
from dataclasses import dataclass, field
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import time
from urllib.parse import parse_qs, urlsplit

import boto3
from botocore.config import Config
from moto.server import ThreadedMotoServer

ROOT = Path(__file__).resolve().parents[1]
# Reserve the descriptor too, keeping encrypted intermediate parts >= S3's 5 MiB.
PART_SIZE = 5 * 1024 * 1024 + 28
# More than four parts (the uploader's concurrency), including a partial last part.
FIXTURE = (b"Maestro S3 upload fixture; plaintext must survive decryption.\n" * 600000)[:31 * 1024 * 1024 + 123]
KEY = "0123456789abcdef" * 4
NONCE = "0123456789abcdef" * 3
BUCKET = "maestro-e2e"


def require(condition, message):
    if not condition:
        raise AssertionError(message)


# Stress hook: reply to the gated parts with a stagger so their completions land
# seconds apart, as they do on a slow runner. Zero (the default) replies at once.
# Run with E2E_RELEASE_STAGGER_MS=1000 to reproduce the pause-settling race.
RELEASE_STAGGER_MS = int(os.environ.get("E2E_RELEASE_STAGGER_MS", "0"))


@dataclass
class Upload:
    key: str
    upload_id: str
    released: threading.Event = field(default_factory=threading.Event)
    requested: Counter = field(default_factory=Counter)
    put_requests: Counter = field(default_factory=Counter)
    paused: bool = False
    resumed: bool = False
    completed: bool = False
    verified: bool = False
    completed_after_ms: float | None = None
    # Size derived from the startUpload declaration (or the scheme default).
    declared_size: int = 0
    # Part whose PUT is dropped once, after its body arrived (injected network error).
    fault_part: int | None = None
    fault_injected: bool = False
    # Progress evidence the test app reports alongside the completion timing.
    progress_regressions: int | None = None
    premature_hundreds: int | None = None

    def fault_retried(self) -> bool:
        return (self.fault_injected and self.fault_part is not None
                and self.put_requests[self.fault_part] >= 2)


class Backend:
    def __init__(self, artifacts, endpoint="http://127.0.0.1:8765",
                 storage_endpoint="http://127.0.0.1:9000", scheme="xchacha"):
        require(scheme in ("xchacha", "aesgcm"), "Unknown verification scheme")
        self.scheme = scheme
        self.artifacts = Path(artifacts)
        self.artifacts.mkdir(parents=True, exist_ok=True)
        (self.artifacts / "fixture.bin").write_bytes(FIXTURE)
        self.storage = urlsplit(storage_endpoint)
        self.s3 = boto3.client(
            "s3", endpoint_url=storage_endpoint,
            aws_access_key_id="test", aws_secret_access_key="test",
            region_name="us-east-1", config=Config(signature_version="s3v4"),
        )
        self.signer = boto3.client(
            "s3", endpoint_url=endpoint,
            aws_access_key_id="test", aws_secret_access_key="test",
            region_name="us-east-1", config=Config(signature_version="s3v4"),
        )
        self.s3.create_bucket(Bucket=BUCKET)
        self.uploads = {}
        self.lock = threading.Lock()

    def upload(self, mode):
        require(mode in self.uploads, f"No upload for {mode}")
        return self.uploads[mode]

    def start(self, body):
        mode = body["mode"]
        require(mode in ("plain", "encrypted"), "Unknown encryption mode")
        # Default expectations for callers that start an upload without a startUpload
        # body (the contract tests in e2e/test_server.py).
        effective_size = PART_SIZE - 16 - 28
        part_count = (len(FIXTURE) + effective_size - 1) // effective_size
        declared_size = len(FIXTURE)
        if mode == "encrypted":
            declared_size += part_count * 16 + 28
        if "fileSize" in body:
            # The uploader always reports the local, unencrypted size. For an
            # encrypted upload the body also carries the encryptor's format numbers,
            # so the backend derives the stored (ciphertext) size from its own part
            # size — the client never has to guess it.
            require(
                body["fileSize"] == len(FIXTURE),
                f"startUpload fileSize {body['fileSize']} is not the plaintext size "
                f"{len(FIXTURE)} for {mode} mode",
            )
            encryption = body.get("encryption")
            if mode == "plain":
                require(encryption is None, "Plain upload declared encryption parameters")
            else:
                require(isinstance(encryption, dict),
                        "Encrypted upload must declare encryption parameters")
                overhead = encryption.get("overhead")
                has_footer = encryption.get("hasFooter")
                footer_len = encryption.get("footerLen")
                # Both the built-in cipher and the custom AES-GCM example add a
                # 16-byte tag per part and a 28-byte descriptor in the final part.
                require((overhead, has_footer, footer_len) == (16, True, 28),
                        f"Unexpected encryption parameters: {encryption}")
                # The rule from the README: tag and descriptor are reserved in every
                # part, so the plaintext slot is partSize - reserved.
                reserved = overhead + (footer_len if has_footer else 0)
                effective_size = PART_SIZE - reserved
                require(effective_size > 0, "PART_SIZE cannot hold the declared format")
                part_count = max(1 if has_footer else 0,
                                 (len(FIXTURE) + effective_size - 1) // effective_size)
                declared_size = len(FIXTURE) + overhead * part_count
                if has_footer:
                    declared_size += footer_len
        with self.lock:
            require(mode not in self.uploads, "Resume created a second multipart upload")
            key = f"{mode}/fixture.bin"
            result = self.s3.create_multipart_upload(Bucket=BUCKET, Key=key)
            upload = Upload(key, result["UploadId"])
            upload.declared_size = declared_size
            self.uploads[mode] = upload
        return {"key": key, "uploadId": upload.upload_id, "partSize": PART_SIZE}

    def urls(self, body):
        upload = self.upload(body["key"].split("/")[0])
        require(body["uploadId"] == upload.upload_id, "Multipart ID changed")
        with self.lock:
            upload.requested.update(body["parts"])
        return {"urls": {
            str(part): self.signer.generate_presigned_url("upload_part", Params={
                "Bucket": BUCKET, "Key": upload.key,
                "UploadId": upload.upload_id, "PartNumber": part,
            }, ExpiresIn=900)
            for part in body["parts"]
        }}

    def complete(self, path, body):
        _, _, upload_id, mode, _ = path.split("/")
        upload = self.upload(mode)
        require(upload_id == upload.upload_id, "Completion used a new multipart ID")
        require(upload.paused and upload.resumed, "Completion without verified pause/resume")
        require(not upload.completed, "Duplicate completion")
        require(upload.requested[1] == 1, "Resume re-requested the completed first part")
        require(upload.put_requests[1] == 1, "Resume re-uploaded the completed first part")
        parts = body["parts"]
        require(len(parts) >= 5, "Expected multipart upload with at least five parts")
        require([p["PartNumber"] for p in parts] == list(range(1, len(parts) + 1)),
                "Missing or unordered completed parts")
        self.s3.complete_multipart_upload(
            Bucket=BUCKET, Key=upload.key, UploadId=upload_id,
            MultipartUpload={"Parts": parts},
        )
        upload.completed = True
        return {}

    def control(self, path, body):
        upload = self.upload(body["mode"])
        if path == "/release":
            upload.released.set()
        elif path == "/paused":
            require(not upload.completed, "Upload completed while paused")
            before, after = body["before"], body["after"]
            require(0 < before["completedParts"] < before["totalParts"], "Not a mid-upload pause")
            require(before["completedParts"] == after["completedParts"] and
                    before["uploadedBytes"] == after["uploadedBytes"], "Progress changed during pause")
            upload.paused = True
        elif path == "/resumed":
            require(upload.paused, "Resume without verified pause")
            upload.resumed = True
        elif path == "/verify":
            self.verify(body["mode"])
        elif path == "/fault":
            part = body.get("part")
            require(upload.paused, "A network fault needs a paused upload")
            require(not upload.resumed, "A network fault must be armed before resuming")
            require(not upload.completed, "A network fault needs an upload that is not complete")
            require(type(part) is int and part > 1,
                    "A network fault must target a pending part after the first one")
            require(upload.fault_part is None and not upload.fault_injected,
                    "A network fault was already armed")
            upload.fault_part = part
        elif path == "/completion-timing":
            elapsed = body.get("completedAfterMs")
            regressions = body.get("progressRegressions")
            premature = body.get("prematureHundreds")
            require(upload.completed, "Timing evidence requires a completed upload")
            require(type(elapsed) in (int, float) and 0 <= elapsed <= 20000,
                    "Missing or invalid completion timing: must be within 20 seconds of 100%")
            require(regressions == 0,
                    f"Progress went backwards {regressions!r} time(s) before completion")
            require(premature == 0,
                    f"Progress reported 100% {premature!r} time(s) before completion")
            upload.completed_after_ms = elapsed
            upload.progress_regressions = regressions
            upload.premature_hundreds = premature
        else:
            raise ValueError(f"Unknown control endpoint {path}")
        return {}

    def decrypt_command(self, key, input_path, output_path):
        if self.scheme == "aesgcm":
            return [
                str(ROOT / "custom-encryption-example/tools/decrypt/target/release/decrypt-verify"),
                "--scheme", "aesgcm", "--key", key, "--nonce", NONCE,
                "--part-size", str(PART_SIZE - 28),
                "--out", str(output_path), str(input_path),
            ]
        return [
            sys.executable, str(ROOT / "scripts/decrypt.py"),
            "--nonce", NONCE, "--max-parts", "10", "--key", key,
            str(input_path), str(output_path),
        ]

    def verify(self, mode):
        upload = self.upload(mode)
        require(upload.completed, "Cannot download an incomplete object")
        # Download via the S3 HTTP API, not the proxy's request buffer or a mock uploader.
        response = self.s3.get_object(Bucket=BUCKET, Key=upload.key)
        with response["Body"] as stream:
            downloaded = stream.read()
        input_path = self.artifacts / f"{mode}.download"
        input_path.write_bytes(downloaded)
        if mode == "plain":
            require(downloaded == FIXTURE, "Unencrypted download differs from original bytes")
            result = subprocess.run(self.decrypt_command(
                KEY, input_path, self.artifacts / "plain.decrypted"), capture_output=True)
            require(result.returncode != 0, "Plaintext unexpectedly authenticated as encrypted")
        else:
            require(downloaded != FIXTURE, "Encrypted upload is plaintext")
            require(FIXTURE[:128] not in downloaded, "Plaintext prefix leaked into ciphertext")
            # The stored size must match what this backend derived from the
            # startUpload declaration, without the client predicting it.
            require(len(downloaded) == upload.declared_size,
                    f"Stored ciphertext is {len(downloaded)} bytes, derived "
                    f"{upload.declared_size} from the startUpload declaration")
            output_path = self.artifacts / "encrypted.decrypted"
            result = subprocess.run(self.decrypt_command(KEY, input_path, output_path),
                                    capture_output=True, text=True)
            (self.artifacts / "correct-key.log").write_text(result.stdout + result.stderr)
            require(result.returncode == 0, f"Correct-key decryption failed: {result.stderr}")
            require(output_path.read_bytes() == FIXTURE, "Decrypted bytes differ from original")
            wrong_output = self.artifacts / "wrong-key.decrypted"
            result = subprocess.run(self.decrypt_command("00" * 32, input_path, wrong_output),
                                    capture_output=True, text=True)
            (self.artifacts / "wrong-key.log").write_text(result.stdout + result.stderr)
            require(result.returncode != 0 and not wrong_output.exists(),
                    "Wrong key was accepted")
        upload.verified = True
        self.report()

    def report(self, final=False):
        report = {mode: {
            "scheme": self.scheme if mode == "encrypted" else "plain",
            "paused": upload.paused, "resumed": upload.resumed,
            "completed": upload.completed, "verified": upload.verified,
            "completedAfter100PercentMs": upload.completed_after_ms,
            "networkErrorInjected": upload.fault_injected,
            "networkErrorRetriedPart": upload.fault_part if upload.fault_injected else None,
            "networkErrorRetried": upload.fault_retried(),
            "progressRegressionsBeforeCompletion": upload.progress_regressions,
            "prematureHundredPercent": upload.premature_hundreds,
            "partUrlRequests": dict(upload.requested),
            "partPutRequests": dict(upload.put_requests),
        } for mode, upload in self.uploads.items()}
        (self.artifacts / "verification.json").write_text(json.dumps(report, indent=2))
        if final:
            require(set(report) == {"plain", "encrypted"}, "Both upload modes must run")
            require(all(u["verified"] and u["paused"] and u["resumed"] and u["completed"]
                        for u in report.values()), "Missing upload/download/pause/resume evidence")
            require(all(u["completedAfter100PercentMs"] is not None for u in report.values()),
                    "Missing 100%-to-completion timing evidence")
            require(all(u["networkErrorInjected"] for u in report.values()),
                    "Missing injected network error: the part retry path never ran")
            require(all(u["networkErrorRetried"] for u in report.values()),
                    "The dropped part was never retried")
            require(all(u["progressRegressionsBeforeCompletion"] == 0 for u in report.values()),
                    "Upload progress went backwards before completion")
            require(all(u["prematureHundredPercent"] == 0 for u in report.values()),
                    "Upload reported 100% before completion")
        return report


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        print(fmt % args, flush=True)

    def reply(self, status, data, content_type="application/json", extra_headers=None):
        self.send_response(status)
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "GET, POST, PUT, OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "*")
        self.send_header("Access-Control-Expose-Headers", "ETag")
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Connection", "close")
        self.close_connection = True
        for key, value in (extra_headers or {}).items():
            self.send_header(key, value)
        self.end_headers()
        try:
            self.wfile.write(data)
        except (BrokenPipeError, ConnectionResetError):
            # Expected when pause aborts a presigned PUT that was waiting at the gate.
            self.log_message("Client aborted %s", self.path)

    def do_OPTIONS(self):
        self.reply(204, b"")

    def do_GET(self):
        if self.path == "/health":
            self.reply(200, b"{}")
        elif self.path == "/fixture":
            self.reply(200, FIXTURE, "application/octet-stream")
        else:
            self.reply(404, b'{"error":"Not found"}')

    def do_POST(self):
        backend = self.server.backend
        try:
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            if self.path == "/start":
                result = backend.start(body)
            elif self.path == "/urls":
                result = backend.urls(body)
            elif self.path.startswith("/complete/"):
                result = backend.complete(self.path, body)
            elif self.path == "/assert":
                result = backend.report(final=True)
            else:
                result = backend.control(self.path, body)
            self.reply(200, json.dumps(result).encode())
        except Exception as error:
            self.log_message("Backend assertion failed: %s", error)
            self.reply(500, json.dumps({"error": str(error)}).encode())

    def do_PUT(self):
        parsed = urlsplit(self.path)
        mode = parsed.path.split("/")[2]
        upload = self.server.backend.upload(mode)
        part = int(parse_qs(parsed.query)["partNumber"][0])
        with self.server.backend.lock:
            upload.put_requests.update([part])
        data = self.rfile.read(int(self.headers["Content-Length"]))
        # Part one completes normally; all other PUTs wait until the app pauses.
        if part != 1:
            if not upload.released.wait(timeout=120):
                self.reply(504, b'{"error":"Timed out waiting for pause"}')
                return
            if RELEASE_STAGGER_MS:
                time.sleep(RELEASE_STAGGER_MS * (part - 2) / 1000)
        if upload.fault_part == part and not upload.fault_injected:
            # The whole body arrived, then the link dies before the response: the
            # uploader already counted these bytes but never learns an ETag, so it has
            # to retry the part. This is the connection-error path from the bug report.
            # Nothing is forwarded to S3 and nothing is answered; the server closes the
            # connection, which every client reports as a network error.
            upload.fault_injected = True
            self.log_message("Injected network error for part %s after its body", part)
            self.close_connection = True
            return
        storage = self.server.backend.storage
        connection = http.client.HTTPConnection(storage.hostname, storage.port, timeout=30)
        try:
            connection.request("PUT", self.path, body=data, headers={
                "Host": self.headers["Host"], "Content-Length": str(len(data)),
            })
            response = connection.getresponse()
            result = response.read()
            headers = {"ETag": response.getheader("ETag")} if response.getheader("ETag") else {}
            self.reply(response.status, result, "application/xml", headers)
        finally:
            connection.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifacts", default="e2e/artifacts")
    parser.add_argument("--scheme", choices=("xchacha", "aesgcm"), default="xchacha")
    args = parser.parse_args()
    moto = ThreadedMotoServer(ip_address="127.0.0.1", port=9000, verbose=False)
    moto.start()
    server = ThreadingHTTPServer(("127.0.0.1", 8765), Handler)
    server.backend = Backend(args.artifacts, scheme=args.scheme)
    try:
        server.serve_forever()
    finally:
        server.server_close()
        moto.stop()


if __name__ == "__main__":
    main()
