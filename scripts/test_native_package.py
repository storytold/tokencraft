"""Check the native package before its services start."""

from contextlib import redirect_stderr
from io import BytesIO, StringIO
import json
from pathlib import Path
import sys
import tarfile
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch
from urllib.parse import urlsplit

import native_package
from native_package import (
    BINARIES, archive_package, smoke_package, verify_binary_versions, verify_console, verify_console_manifest, verify_documentation,
)


class NativePackageInputsTest(unittest.TestCase):
    def test_container_context_keeps_package_inputs_and_limits_directory_includes(self):
        root = native_package.ROOT
        rules = [line for line in (root / ".dockerignore").read_text(encoding="utf-8").splitlines()
                 if line and not line.startswith("#")]
        self.assertEqual(rules[0], "**")
        includes = {rule[1:] for rule in rules if rule.startswith("!")}
        source_trees = {rule for rule in ("crates/**", "sdk/rust/**") if rule in includes}
        paths = includes - source_trees
        unexpected = sorted(path for path in paths if not (root / path).is_file())
        self.assertEqual(unexpected, [], "Use named files outside the Rust source trees.")

        inputs = {path.relative_to(root).as_posix() for path in native_package.package_assets().values()}
        missing = sorted(path for path in inputs - paths
                         if not any(path.startswith(tree.removesuffix("**")) for tree in source_trees))
        self.assertEqual(missing, [], "Include all native package inputs in the Docker context.")

    def test_reports_all_missing_release_inputs_before_looking_up_build_tools(self):
        missing = {"SECURITY.md", "frontend/public/design/index.html"}
        with TemporaryDirectory() as directory, patch.object(native_package, "ROOT", Path(directory)):
            for source in native_package.package_assets().values():
                if source.relative_to(directory).as_posix() not in missing:
                    source.parent.mkdir(parents=True, exist_ok=True)
                    source.touch()
            output = Path(directory) / "packages"
            with (patch.object(sys, "argv", ["native_package.py", "--output", str(output)]),
                  patch.object(native_package, "postgres_bin") as postgres_bin,
                  self.assertRaises(SystemExit) as failure):
                native_package.main()

            self.assertEqual(str(failure.exception).splitlines(),
                             ["Missing native package inputs:", *sorted(missing)])
            postgres_bin.assert_not_called()
            self.assertFalse(output.exists())


class NativePackageDocumentationTest(unittest.TestCase):
    def test_shipped_guides_resolve_from_the_extracted_archive(self):
        with TemporaryDirectory() as directory:
            work = Path(directory)
            binaries = work / "binaries"
            binaries.mkdir()
            for name in BINARIES:
                binary = binaries / name
                binary.touch(mode=0o755)
            package = "tokencraft-fixture"
            archive = archive_package(binaries, work, package, metadata={})
            with tarfile.open(archive, "r:gz") as tar:
                tar.extractall(work, filter="data")
            verify_documentation((work / package).resolve())


class NativeConsoleBuildTest(unittest.TestCase):
    def test_rejects_a_hosted_console_with_the_correct_revision_before_building(self):
        revision = "a" * 40
        with TemporaryDirectory() as directory:
            console = Path(directory)
            (console / "release.json").write_text(json.dumps({"source_revision": revision}), encoding="utf-8")
            argv = ["native_package.py", "--console-dir", str(console),
                    "--source-revision", revision, "--output", str(console / "packages")]
            messages = StringIO()
            with (patch.object(sys, "argv", argv),
                  patch.object(native_package, "postgres_bin") as postgres_bin,
                  redirect_stderr(messages), self.assertRaises(SystemExit) as failure):
                native_package.main()
            self.assertEqual(failure.exception.code, 2)
            self.assertIn("VITE_TOKENCRAFT_PACKAGED=true", messages.getvalue())
            postgres_bin.assert_not_called()
            self.assertFalse((console / "packages").exists())

    def test_requires_launcher_api_and_validates_stamped_and_unstamped_builds(self):
        revision = "a" * 40
        unstamped = b'{"launcher_api":true}'
        verify_console_manifest(unstamped, None)
        verify_console_manifest(json.dumps({"launcher_api": True, "source_revision": revision}), revision)
        with self.assertRaisesRegex(RuntimeError, "source revision differs"):
            verify_console_manifest(unstamped, revision)
        with self.assertRaisesRegex(RuntimeError, "manifest is invalid"):
            verify_console_manifest(b"<html>", None)

    def test_mounted_console_serves_its_compiled_entry_module(self):
        revision = "a" * 40
        marker = b'<meta name="tokencraft-api-port" content="4000">'
        index = b'<html><head><script type="module" src="/assets/index-fixture.js"></script></head></html>'
        javascript = b"console.log('matching console')"
        manifest = json.dumps({"launcher_api": True, "source_revision": revision}).encode()
        with TemporaryDirectory() as directory:
            package = Path(directory)
            files = {
                "console/index.html": index,
                "console/assets/index-fixture.js": javascript,
                "contracts/openapi.json": b"{}",
                "catalog/artcraft-v1.json": b"{}",
            }
            for name, contents in files.items():
                path = package / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(contents)

            served = {
                "/pricing": index.replace(b"<head>", b"<head>" + marker),
                "/assets/index-fixture.js": javascript,
                "/docs/openapi.json": files["contracts/openapi.json"],
                "/catalog/artcraft-v1.json": files["catalog/artcraft-v1.json"],
                "/release.json": manifest,
            }

            class Opener:
                def open(self, request, *, timeout):
                    url = request if isinstance(request, str) else request.full_url
                    return BytesIO(served[urlsplit(url).path])

            verify_console(Opener(), "http://localhost:3000", package, 4000, revision)
            served["/assets/index-fixture.js"] = served["/pricing"]
            with self.assertRaisesRegex(RuntimeError, "assets/index-fixture.js differs from the release asset"):
                verify_console(Opener(), "http://localhost:3000", package, 4000, revision)


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
