"""Local multipart S3 backend and independent download/decryption assertions."""

import argparse
from collections import Counter
from dataclasses import dataclass, field
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import subprocess
import sys
import threading
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


class Backend:
    def __init__(self, artifacts, endpoint="http://127.0.0.1:8765",
                 storage_endpoint="http://127.0.0.1:9000"):
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
        with self.lock:
            require(mode not in self.uploads, "Resume created a second multipart upload")
            key = f"{mode}/fixture.bin"
            result = self.s3.create_multipart_upload(Bucket=BUCKET, Key=key)
            upload = Upload(key, result["UploadId"])
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
        else:
            raise ValueError(f"Unknown control endpoint {path}")
        return {}

    def verify(self, mode):
        upload = self.upload(mode)
        require(upload.completed, "Cannot download an incomplete object")
        # Download via the S3 HTTP API, not the proxy's request buffer or a mock uploader.
        response = self.s3.get_object(Bucket=BUCKET, Key=upload.key)
        with response["Body"] as stream:
            downloaded = stream.read()
        input_path = self.artifacts / f"{mode}.download"
        input_path.write_bytes(downloaded)
        command = [
            sys.executable, str(ROOT / "scripts/decrypt.py"),
            "--nonce", NONCE, "--max-parts", "10",
        ]
        if mode == "plain":
            require(downloaded == FIXTURE, "Unencrypted download differs from original bytes")
            result = subprocess.run(command + ["--key", KEY, str(input_path),
                                    str(self.artifacts / "plain.decrypted")], capture_output=True)
            require(result.returncode != 0, "Plaintext unexpectedly authenticated as encrypted")
        else:
            require(downloaded != FIXTURE, "Encrypted upload is plaintext")
            require(FIXTURE[:128] not in downloaded, "Plaintext prefix leaked into ciphertext")
            effective_size = PART_SIZE - 16 - 28
            part_count = (len(FIXTURE) + effective_size - 1) // effective_size
            require(len(downloaded) == len(FIXTURE) + part_count * 16 + 28,
                    "Wrong ciphertext wire size")
            output_path = self.artifacts / "encrypted.decrypted"
            result = subprocess.run(command + ["--key", KEY, str(input_path), str(output_path)],
                                    capture_output=True, text=True)
            require(result.returncode == 0, f"Correct-key decryption failed: {result.stderr}")
            require(output_path.read_bytes() == FIXTURE, "Decrypted bytes differ from original")
            wrong_output = self.artifacts / "wrong-key.decrypted"
            result = subprocess.run(command + ["--key", "00" * 32,
                                    str(input_path), str(wrong_output)], capture_output=True)
            require(result.returncode != 0 and not wrong_output.exists(),
                    "Wrong key was accepted")
        upload.verified = True
        self.report()

    def report(self, final=False):
        report = {mode: {
            "paused": upload.paused, "resumed": upload.resumed,
            "completed": upload.completed, "verified": upload.verified,
            "partUrlRequests": dict(upload.requested),
            "partPutRequests": dict(upload.put_requests),
        } for mode, upload in self.uploads.items()}
        (self.artifacts / "verification.json").write_text(json.dumps(report, indent=2))
        if final:
            require(set(report) == {"plain", "encrypted"}, "Both upload modes must run")
            require(all(u["verified"] and u["paused"] and u["resumed"] and u["completed"]
                        for u in report.values()), "Missing upload/download/pause/resume evidence")
        return report


class Handler(BaseHTTPRequestHandler):
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
        if part != 1 and not upload.released.wait(timeout=120):
            self.reply(504, b'{"error":"Timed out waiting for pause"}')
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
    args = parser.parse_args()
    moto = ThreadedMotoServer(ip_address="127.0.0.1", port=9000, verbose=False)
    moto.start()
    server = ThreadingHTTPServer(("127.0.0.1", 8765), Handler)
    server.backend = Backend(args.artifacts)
    try:
        server.serve_forever()
    finally:
        server.server_close()
        moto.stop()


if __name__ == "__main__":
    main()
