"""Check the bounded guest result protocol without starting a VM."""

import hashlib
import io
import unittest

from build import HEADER_BYTES, MAX_IMAGE_BYTES, copy_result


class ResultTests(unittest.TestCase):
    def test_incomplete_and_damaged_exports(self):
        digest = hashlib.sha256(b"abc").hexdigest()
        cases = [
            ("", b"", "did not finish"),
            ("RSCTF-PATCH-FAILED build\n", b"", "failed during build"),
            (f"RSCTF-PATCH-OCI 1 {MAX_IMAGE_BYTES + 1} {digest}\n", b"", "output limit"),
            (f"RSCTF-PATCH-OCI 1 3 {digest}\n", b"ab", "truncated"),
            (f"RSCTF-PATCH-OCI 1 3 {digest}\n", b"abd", "digest mismatch"),
        ]
        for header, body, message in cases:
            with self.subTest(message=message):
                disk = io.BytesIO(header.encode().ljust(HEADER_BYTES, b"\0") + body)
                with self.assertRaisesRegex(ValueError, message):
                    copy_result(disk, io.BytesIO())


if __name__ == "__main__":
    unittest.main()
