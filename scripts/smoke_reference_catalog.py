"""Rehearse publishing and importing signed presets with a temporary HTTPS registry."""

import argparse
from contextlib import contextmanager
from hashlib import sha256
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import ssl
import subprocess
from tempfile import TemporaryDirectory
from threading import Thread


ROOT = Path(__file__).resolve().parents[1]
REFERENCE = ROOT / "catalog/artcraft-v1.json"
BASE = ROOT / "examples/connector/catalog.json"
PLUGINS = ROOT / "plugins/providers/inference"


def run(*command):
    result = subprocess.run(command, capture_output=True, timeout=20)
    if result.returncode:
        raise RuntimeError(result.stderr.decode(errors="replace").strip())
    return result.stdout


@contextmanager
def registry(directory):
    ca_key, ca, key, csr, certificate, extensions = (
        directory / name for name in ("ca.key", "ca.pem", "server.key", "server.csr", "server.pem", "server.ext")
    )
    run("openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256",
        "-nodes", "-subj", "/CN=TokenCraft catalog smoke CA", "-days", "1",
        "-addext", "basicConstraints=critical,CA:TRUE", "-out", ca, "-keyout", ca_key)
    run("openssl", "req", "-new", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256",
        "-nodes", "-subj", "/CN=127.0.0.1", "-out", csr, "-keyout", key)
    extensions.write_text("basicConstraints=critical,CA:FALSE\n"
                          "extendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1\n")
    run("openssl", "x509", "-req", "-in", csr, "-CA", ca, "-CAkey", ca_key,
        "-set_serial", "1", "-days", "1", "-extfile", extensions, "-out", certificate)

    class Handler(SimpleHTTPRequestHandler):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, directory=directory / "www", **kwargs)

        def log_message(self, *_):
            pass

    with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
        tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls.load_cert_chain(certificate, key)
        server.socket = tls.wrap_socket(server.socket, server_side=True)
        thread = Thread(target=server.serve_forever)
        thread.start()
        try:
            yield f"https://127.0.0.1:{server.server_port}/releases/", ca
        finally:
            server.shutdown()
            thread.join()


def verify_release(binary, directory):
    def catalog(*args):
        return run(binary, "catalog", *args)

    plugins = []
    for name in ("fal", "kinovi", "openrouter"):
        plugins.extend(("--plugin-catalog", PLUGINS / name / "catalog.example.json"))

    private = directory / "publisher.der"
    public = directory / "publisher-public.der"
    run("openssl", "genpkey", "-algorithm", "ED25519", "-outform", "DER", "-out", private)
    run("openssl", "pkey", "-inform", "DER", "-in", private, "-pubout", "-outform", "DER", "-out", public)
    publisher = sha256(public.read_bytes()).hexdigest()
    signature = directory / "signature.json"
    releases = directory / "www/releases"

    def stage(reference):
        signature.write_bytes(catalog("sign-reference", "--reference", reference, "--private-key", private, *plugins))
        report = json.loads(catalog("stage-reference", "--reference", reference, "--signature", signature,
                                    "--public-key", public, "--directory", releases, *plugins))
        original = reference.read_bytes()
        assert report["reference_sha256"] == sha256(original).hexdigest()
        assert (releases / report["catalog_path"]).read_bytes() == original
        assert {entry["offer_id"] for entry in report["plugin_coverage"]} == {
            offer["id"] for offer in json.loads(original)["offers"]
        }
        return report

    with registry(directory) as (origin, ca):
        def fetch(*args):
            return catalog("fetch-reference", "--registry", origin, "--public-key", public, "--ca", ca, *args)

        initial = stage(REFERENCE)
        first = fetch("--current")
        assert first == (releases / initial["release_path"]).read_bytes()
        source = json.loads(REFERENCE.read_bytes())
        preset = next(offer for offer in source["offers"] if offer["kind"] == "image")
        source["revision"] += "-smoke"
        preset["rates"] = [{"unit": "image", "usd_nanos": 1}]
        updated_reference = directory / "updated.json"
        updated_reference.write_text(json.dumps(source))
        updated = stage(updated_reference)
        assert fetch("--sha256", initial["reference_sha256"]) == first
        second = fetch("--current")
        assert second == fetch("--sha256", updated["reference_sha256"])
        assert second != first

    installed = directory / "installed.json"
    bundle = directory / "reference.bundle.json"
    selection = directory / "selection.json"

    def prepare(report, data, base, choice):
        bundle.write_bytes(data)
        selection.write_text(json.dumps({
            "version": 1,
            "reference_revision": report["reference_revision"],
            "reference_sha256": report["reference_sha256"],
            "reference_public_key_sha256": publisher,
            "offers": [{"id": "studio_preset", "reference": preset["id"], **choice}],
        }))
        return catalog("prepare", "--bundle", bundle, "--public-key", public, "--selection", selection, "--base", base)

    installed.write_bytes(prepare(initial, first, BASE, {
        "model": "studio_image", "binding": "studio-provider", "rates": {"usd_nanos_per_image": 31_876_543},
    }))
    updated_local = prepare(updated, second, installed, {"update_from_sha256": initial["reference_sha256"]})
    old_offers, new_offers = (json.loads(data)["offers"] for data in (installed.read_bytes(), updated_local))
    old_preset, new_preset = (
        next(offer for offer in offers if offer["id"] == "studio_preset")
        for offers in (old_offers, new_offers)
    )
    assert old_preset["local_binding"] == "studio-provider"
    assert old_preset["pricing"]["usd_nanos_per_image"] == 31_876_543
    for offer, report in ((old_preset, initial), (new_preset, updated)):
        assert offer["artcraft_reference"] == {
            "offer_id": preset["id"],
            "revision": report["reference_revision"],
            "sha256": report["reference_sha256"],
            "public_key_sha256": publisher,
        }
    assert any(offer["id"] == "example_image" for offer in old_offers)
    new_preset["artcraft_reference"] = old_preset["artcraft_reference"]
    assert new_offers == old_offers, "adopting the signed revision changed local offer settings"
    print("Signed catalog publication, pinned HTTPS fetch, and self-hosted preset adoption passed.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path, help="Path to the compiled api-gateway binary")
    args = parser.parse_args()
    os.umask(0o077)
    with TemporaryDirectory(prefix="tokencraft-catalog-") as temporary:
        verify_release(args.gateway.resolve(strict=True), Path(temporary))
