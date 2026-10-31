"""Contract tests for the test oracle, not substitutes for the Maestro flows."""

from concurrent.futures import ThreadPoolExecutor
from http.server import ThreadingHTTPServer
import struct
import tempfile
import threading
import time
import unittest
from urllib.request import Request, urlopen

from moto.server import ThreadedMotoServer
from nacl.bindings import crypto_aead_xchacha20poly1305_ietf_encrypt
from cryptography.hazmat.primitives.ciphers.aead import AESGCM

from server import Backend, BUCKET, FIXTURE, Handler, KEY, NONCE, PART_SIZE


def encrypted_fixture(scheme="xchacha"):
    effective_size = PART_SIZE - 16 - 28
    parts = [FIXTURE[start:start + effective_size]
             for start in range(0, len(FIXTURE), effective_size)]

    def encrypt(data, number):
        nonce = bytearray.fromhex(NONCE)
        if scheme == "aesgcm":
            for index, value in enumerate(struct.pack("<I", number)):
                nonce[index] ^= value
            return AESGCM(bytes.fromhex(KEY)).encrypt(bytes(nonce[:12]), data, None)
        for index, value in enumerate(struct.pack("<I", number)):
            nonce[20 + index] ^= value
        return crypto_aead_xchacha20poly1305_ietf_encrypt(
            data, struct.pack("<I", number), bytes(nonce), bytes.fromhex(KEY),
        )

    result = [encrypt(part, number) for number, part in enumerate(parts, 1)]
    result[-1] += encrypt(struct.pack("<IQ", len(parts), effective_size), len(parts) + 1)
    return result


class BackendTests(unittest.TestCase):
    scheme = "xchacha"

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.moto = ThreadedMotoServer(ip_address="127.0.0.1", port=0, verbose=False)
        self.moto.start()
        host, port = self.moto.get_host_and_port()
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.backend = Backend(
            self.directory.name,
            endpoint=f"http://127.0.0.1:{self.server.server_port}",
            storage_endpoint=f"http://{host}:{port}",
            scheme=self.scheme,
        )
        self.server.backend = self.backend
        self.thread = threading.Thread(target=self.server.serve_forever)
        self.thread.start()

    def tearDown(self):
        for upload in self.backend.uploads.values():
            upload.released.set()
        self.server.shutdown()
        self.thread.join()
        self.server.server_close()
        self.moto.stop()
        self.directory.cleanup()

    def upload(self, mode, chunks):
        started = self.backend.start({"mode": mode})
        urls = self.backend.urls({**started, "parts": list(range(1, len(chunks) + 1))})["urls"]
        self.backend.control("/release", {"mode": mode})
        snapshot = {"completedParts": 1, "totalParts": len(chunks), "uploadedBytes": PART_SIZE}
        self.backend.control("/paused", {"mode": mode, "before": snapshot, "after": snapshot})
        self.backend.control("/resumed", {"mode": mode})
        parts = []
        for number, chunk in enumerate(chunks, 1):
            with urlopen(Request(urls[str(number)], data=chunk, method="PUT"), timeout=10) as result:
                parts.append({"PartNumber": number, "ETag": result.headers["ETag"]})
        self.backend.complete(f"/complete/{started['uploadId']}/{started['key']}", {"parts": parts})
        return started

    def test_round_trip_and_final_evidence(self):
        chunks = [FIXTURE[start:start + PART_SIZE] for start in range(0, len(FIXTURE), PART_SIZE)]
        self.upload("plain", chunks)
        self.backend.verify("plain")
        self.upload("encrypted", encrypted_fixture(self.scheme))
        self.backend.verify("encrypted")
        self.backend.report(final=True)

    def test_gate_blocks_parts_until_pause(self):
        started = self.backend.start({"mode": "plain"})
        urls = self.backend.urls({**started, "parts": [1, 2]})["urls"]
        with urlopen(Request(urls["1"], data=b"first", method="PUT"), timeout=10) as result:
            self.assertEqual(result.status, 200)
        with ThreadPoolExecutor(max_workers=1) as pool:
            def put():
                with urlopen(Request(urls["2"], data=b"second", method="PUT"), timeout=10) as result:
                    return result.status
            waiting = pool.submit(put)
            time.sleep(0.2)
            self.assertFalse(waiting.done())
            self.backend.control("/release", {"mode": "plain"})
            self.assertEqual(waiting.result(timeout=5), 200)

    def test_fixture_responses_are_complete_and_framed(self):
        for _ in range(2):
            with urlopen(f"http://127.0.0.1:{self.server.server_port}/fixture") as response:
                self.assertEqual(response.version, 11)
                self.assertEqual(int(response.headers["Content-Length"]), len(FIXTURE))
                self.assertEqual(response.headers["Connection"], "close")
                self.assertEqual(response.read(), FIXTURE)

    def test_plaintext_cannot_pass_as_encrypted(self):
        self.backend.start({"mode": "encrypted"})
        upload = self.backend.upload("encrypted")
        upload.completed = True
        self.backend.s3.put_object(Bucket=BUCKET, Key=upload.key, Body=FIXTURE)
        with self.assertRaisesRegex(AssertionError, "is plaintext"):
            self.backend.verify("encrypted")

    def test_corrupted_ciphertext_fails_correct_key_check(self):
        self.upload("encrypted", encrypted_fixture(self.scheme))
        upload = self.backend.upload("encrypted")
        with self.backend.s3.get_object(Bucket=BUCKET, Key=upload.key)["Body"] as stream:
            data = bytearray(stream.read())
        data[0] ^= 1
        self.backend.s3.put_object(Bucket=BUCKET, Key=upload.key, Body=bytes(data))
        with self.assertRaisesRegex(AssertionError, "Correct-key decryption failed"):
            self.backend.verify("encrypted")

    def test_altered_plaintext_is_rejected(self):
        self.backend.start({"mode": "plain"})
        upload = self.backend.upload("plain")
        upload.completed = True
        self.backend.s3.put_object(Bucket=BUCKET, Key=upload.key, Body=b"bad" + FIXTURE[3:])
        with self.assertRaisesRegex(AssertionError, "differs from original"):
            self.backend.verify("plain")

    def test_incomplete_suite_and_progress_mutation_fail(self):
        with self.assertRaisesRegex(AssertionError, "Both upload modes"):
            self.backend.report(final=True)
        self.backend.start({"mode": "plain"})
        before = {"completedParts": 1, "totalParts": 5, "uploadedBytes": PART_SIZE}
        after = {**before, "uploadedBytes": PART_SIZE + 1}
        with self.assertRaisesRegex(AssertionError, "Progress changed"):
            self.backend.control("/paused", {"mode": "plain", "before": before, "after": after})
        with self.assertRaisesRegex(AssertionError, "second multipart"):
            self.backend.start({"mode": "plain"})


class CustomBackendTests(BackendTests):
    scheme = "aesgcm"

    def test_builtin_ciphertext_cannot_pass_as_custom(self):
        self.backend.start({"mode": "encrypted"})
        upload = self.backend.upload("encrypted")
        upload.completed = True
        self.backend.s3.put_object(
            Bucket=BUCKET, Key=upload.key, Body=b"".join(encrypted_fixture("xchacha")),
        )
        with self.assertRaisesRegex(AssertionError, "Correct-key decryption failed"):
            self.backend.verify("encrypted")


if __name__ == "__main__":
    unittest.main()
