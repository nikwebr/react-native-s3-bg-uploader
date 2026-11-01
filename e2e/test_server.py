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
        # The real flows drop the last pending part's PUT once, after its body arrived.
        last = len(chunks)
        self.backend.control("/fault", {"mode": mode, "part": last})
        self.backend.control("/resumed", {"mode": mode})
        parts = []
        for number, chunk in enumerate(chunks, 1):
            if number == last:
                with self.assertRaises(Exception):
                    urlopen(Request(urls[str(number)], data=chunk, method="PUT"), timeout=10)
            with urlopen(Request(urls[str(number)], data=chunk, method="PUT"), timeout=10) as result:
                parts.append({"PartNumber": number, "ETag": result.headers["ETag"]})
        self.backend.complete(f"/complete/{started['uploadId']}/{started['key']}", {"parts": parts})
        return started

    def timing(self, mode, **evidence):
        payload = {"mode": mode, "completedAfterMs": 0,
                   "progressRegressions": 0, "prematureHundreds": 0}
        payload.update(evidence)
        return self.backend.control("/completion-timing", payload)

    def test_round_trip_and_final_evidence(self):
        chunks = [FIXTURE[start:start + PART_SIZE] for start in range(0, len(FIXTURE), PART_SIZE)]
        self.upload("plain", chunks)
        self.backend.verify("plain")
        self.upload("encrypted", encrypted_fixture(self.scheme))
        self.backend.verify("encrypted")
        with self.assertRaisesRegex(AssertionError, "Missing 100%-to-completion"):
            self.backend.report(final=True)
        for mode in ("plain", "encrypted"):
            self.timing(mode, completedAfterMs=20000)
        self.backend.report(final=True)

    def test_completion_timing_rejects_missing_late_and_invalid_evidence(self):
        self.backend.start({"mode": "plain"})
        upload = self.backend.upload("plain")
        with self.assertRaisesRegex(AssertionError, "requires a completed upload"):
            self.timing("plain")
        upload.completed = True
        for elapsed in (None, -1, 20001, float("nan"), float("inf"), True, "100"):
            with self.subTest(elapsed=elapsed):
                with self.assertRaisesRegex(AssertionError, "within 20 seconds"):
                    self.timing("plain", completedAfterMs=elapsed)
        for evidence, expected in (
            ({"progressRegressions": 1}, "Progress went backwards"),
            ({"prematureHundreds": 1}, "Progress reported 100%"),
            ({"progressRegressions": None}, "Progress went backwards"),
            ({"prematureHundreds": None}, "Progress reported 100%"),
        ):
            with self.subTest(evidence=evidence):
                with self.assertRaisesRegex(AssertionError, expected):
                    self.timing("plain", **evidence)
        self.timing("plain", completedAfterMs=20000)
        self.assertEqual(self.backend.report()["plain"]["completedAfter100PercentMs"], 20000)

    def test_network_fault_is_armed_on_a_paused_upload_and_injected_once(self):
        started = self.backend.start({"mode": "plain"})
        urls = self.backend.urls({**started, "parts": [1, 2]})["urls"]
        with self.assertRaisesRegex(AssertionError, "needs a paused upload"):
            self.backend.control("/fault", {"mode": "plain", "part": 2})
        self.backend.control("/release", {"mode": "plain"})
        snapshot = {"completedParts": 1, "totalParts": 2, "uploadedBytes": PART_SIZE}
        self.backend.control("/paused", {"mode": "plain", "before": snapshot, "after": snapshot})
        for part in (1, None, "2"):
            with self.subTest(part=part):
                with self.assertRaisesRegex(AssertionError, "after the first one"):
                    self.backend.control("/fault", {"mode": "plain", "part": part})
        self.backend.control("/fault", {"mode": "plain", "part": 2})
        with self.assertRaisesRegex(AssertionError, "already armed"):
            self.backend.control("/fault", {"mode": "plain", "part": 2})
        self.backend.control("/resumed", {"mode": "plain"})
        with self.assertRaisesRegex(AssertionError, "before resuming"):
            self.backend.control("/fault", {"mode": "plain", "part": 2})
        with self.assertRaises(Exception):
            urlopen(Request(urls["2"], data=b"dropped", method="PUT"), timeout=10)
        self.assertTrue(self.backend.upload("plain").fault_injected)
        with urlopen(Request(urls["2"], data=b"retried", method="PUT"), timeout=10) as result:
            self.assertEqual(result.status, 200)
        self.assertTrue(self.backend.upload("plain").fault_retried())

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
