"""Check the bounded guest result protocol without starting a VM."""

import hashlib
import io
import unittest

from build import HEADER_BYTES, MAX_IMAGE_BYTES, ImageReceipt, copy_result


class ResultTests(unittest.TestCase):
    def test_image_identity_receipts_and_older_guests(self):
        digest = hashlib.sha256(b"abc").hexdigest()
        manifest, config = ("sha256:" + c * 64 for c in "ab")
        for version, suffix, expected in (
            (1, "", ImageReceipt(digest, None, None)),
            (2, f" {manifest} {config}", ImageReceipt(digest, manifest, config)),
        ):
            header = f"RSCTF-PATCH-OCI {version} 3 {digest}{suffix}\n".encode()
            output = io.BytesIO()
            self.assertEqual(copy_result(io.BytesIO(header.ljust(HEADER_BYTES, b"\0") + b"abc"), output), expected)
            self.assertEqual(output.getvalue(), b"abc")

    def test_incomplete_and_damaged_exports(self):
        digest = hashlib.sha256(b"abc").hexdigest()
        cases = [
            ("", b"", "did not finish"),
            ("RSCTF-PATCH-FAILED build\n", b"", "failed during build"),
            (f"RSCTF-PATCH-OCI 1 {MAX_IMAGE_BYTES + 1} {digest}\n", b"", "output limit"),
            (f"RSCTF-PATCH-OCI 1 3 {digest}\n", b"ab", "truncated"),
            (f"RSCTF-PATCH-OCI 1 3 {digest}\n", b"abd", "digest mismatch"),
            (f"RSCTF-PATCH-OCI 2 3 {digest} sha256:{digest}\n", b"abc", "did not finish"),
            (f"RSCTF-PATCH-OCI 2 3 {digest} sha256:{digest} latest\n", b"abc", "did not finish"),
        ]
        for header, body, message in cases:
            with self.subTest(message=message):
                disk = io.BytesIO(header.encode().ljust(HEADER_BYTES, b"\0") + body)
                with self.assertRaisesRegex(ValueError, message):
                    copy_result(disk, io.BytesIO())


if __name__ == "__main__":
    unittest.main()
