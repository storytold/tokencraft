"""Check that a mixed native package fails before its services start."""

import json
from pathlib import Path
import tarfile
from tempfile import TemporaryDirectory
import unittest

from native_package import BINARIES, smoke_package, verify_binary_versions


class NativeBinaryVersionsTest(unittest.TestCase):
    def test_rejects_each_mismatched_binary_in_the_extracted_package(self):
        revision = "a" * 40
        version = "0.1.0"
        with TemporaryDirectory() as directory:
            work = Path(directory)
            package = work / "tokencraft-fixture"
            binaries = package / "bin"
            binaries.mkdir(parents=True)
            (package / "VERSION.json").write_text(
                json.dumps({"version": version, "source_revision": revision}), encoding="utf-8",
            )

            def write_binary(name, source):
                path = binaries / name
                path.write_text(
                    '#!/bin/sh\ntest "$1" = --version || exit 64\n'
                    f"printf '%s\\n' '{name} {version} ({source})'\n",
                    encoding="utf-8",
                )
                path.chmod(0o755)

            for name in BINARIES:
                write_binary(name, revision)
            verify_binary_versions(package, version, revision)

            archive = work / "fixture.tar.gz"
            for name in BINARIES:
                with self.subTest(binary=name):
                    write_binary(name, "b" * 40)
                    with tarfile.open(archive, "w:gz") as tar:
                        tar.add(package, arcname=package.name)
                    with self.assertRaisesRegex(RuntimeError, f"packaged {name} version or source revision differs"):
                        smoke_package(archive, package.name, pg_bin=None)
                    write_binary(name, revision)


if __name__ == "__main__":
    unittest.main()
