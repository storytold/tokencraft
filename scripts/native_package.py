"""Build, verify, and package TokenCraft for the current host."""

import argparse
from contextlib import ExitStack, contextmanager
import gzip
from hashlib import file_digest
import io
import json
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import socket
import subprocess
import sys
import tarfile
from tempfile import TemporaryDirectory
from time import monotonic, sleep
import tomllib
from urllib.error import URLError
from urllib.parse import quote
from urllib.request import ProxyHandler, Request, build_opener

from postgres_tools import postgres_bin
from smoke_native_backup import defer_workflow, prepare_workflow, verify_backup


ROOT = Path(__file__).resolve().parents[1]
BINARIES = ("tokencraft", "api-gateway", "admin-cli")
MEDIA_WORKER = "plugins/tools/media"
MEDIA_FILES = (
    "README.md", "contract.py", "delivery.manifest.json", "delivery.py",
    "documents.manifest.json", "documents.py", "engine.py", "manifest.json",
    "mesh.manifest.json", "meshes.py", "queue_store.py", "recipes.py", "server.py",
    "speech.manifest.json", "speech.py", "studio.manifest.json", "studio.py",
)
OPERATIONS = "deploy/monitoring"
OPERATIONS_FILES = (
    "README.md", "autoscaling_snapshot.py", "history.py", "install.sql", "monitor.py",
    "plugin_traffic.py", "prometheus.example.yml", "settings.compute.example.json",
    "settings.example.json", "sources.py", "tokencraft-monitor.rules.yml",
    "tokencraft-monitor.service", "web/console.css", "web/console.js", "web/index.html",
    "web/tokencraft-mark.png",
)
SUPPORT_FILES = (
    "LICENSE", "README.md", "catalog/artcraft-v1.json", "contracts/openapi.json",
    "docs/setup.md", "docs/deployment.md", "docs/workflows.md", "docs/media-tools.md",
    "docs/provider-connectors.md", "docs/customer-data.md", "docs/event-archives.md",
    "docs/chat-history.md", "examples/connector/catalog.json",
    "examples/connector/bindings.json", "examples/connector/smoke.sh",
    "examples/postgres/export_to_postgres.py", "examples/postgres/postgres_usage_views.sql",
    "examples/postgres/grant_sync_role.sql", "examples/postgres/smoke.py",
    "deploy/postgres/grant-runtime.sql", "docs/capacity.md",
    "deploy/autoscaling/collect.py", "deploy/autoscaling/controller.py", "deploy/autoscaling/signals.sql",
    "plugins/providers/compute/examples/two-target-chat/catalog.json",
    "plugins/providers/compute/examples/two-target-chat/chat-bindings.json",
    *(f"{OPERATIONS}/{name}" for name in OPERATIONS_FILES),
    *(f"{MEDIA_WORKER}/{name}" for name in MEDIA_FILES),
)


def run(args, **options):
    return subprocess.run(args, check=True, **options)


def build_binaries(pg_bin, target, cargo_profile, offline, *, source=ROOT, source_revision=None):
    """Compile SQLx queries against a migrated cluster on a private Unix socket."""
    cargo = ["cargo", "build", "--locked", "--profile", cargo_profile]
    if offline:
        cargo.append("--offline")
    env = {**os.environ, "CARGO_TARGET_DIR": str(target), "SQLX_OFFLINE": "false"}
    if source_revision is not None:
        env["TOKENCRAFT_SOURCE_REVISION"] = source_revision
    directory = target / ("debug" if cargo_profile == "dev" else cargo_profile)
    with TemporaryDirectory(prefix="tc-build-", dir="/tmp") as temp:
        work = Path(temp)
        db = work / "postgres"
        log = work / "postgres.log"
        settings = [str(pg_bin / "pg_ctl"), "-D", str(db)]
        run([pg_bin / "initdb", "-D", db, "--username=tokencraft", "--locale=C",
             "--encoding=UTF8", "--auth-local=trust", "--auth-host=reject"],
            stdout=subprocess.DEVNULL)
        options = shlex.join(["-k", str(work), "-c", "listen_addresses="])
        try:
            run([*settings, "-l", log, "-o", options, "-w", "start"])
            build_env = {**env, "DATABASE_URL": f"postgresql://tokencraft@localhost/postgres?host={quote(temp, safe='')}"}
            run([*cargo, "-p", "tokencraft", "-p", "admin_cli"], cwd=source, env=build_env)
            run([directory / "admin-cli", "migrate"], cwd=work, env=build_env)
            run([*cargo, "-p", "api_gateway", "--bin", "api-gateway"], cwd=source, env=build_env)
        finally:
            if (db / "postmaster.pid").exists():
                run([*settings, "-m", "fast", "-w", "stop"])
    return directory


def host_identifier():
    os_name = platform.system().lower()
    arch = {"aarch64": "arm64", "arm64": "arm64", "x86_64": "x86_64"}.get(platform.machine())
    if os_name not in ("darwin", "linux") or arch is None:
        raise SystemExit("Native packaging supports Linux and macOS on arm64 or x86_64.")
    return f"{os_name}-{arch}"


def archive_package(binary_dir, stage, name, metadata, console_dir=None):
    """Archive only the distribution files, with fixed tar and gzip metadata."""
    files = {f"bin/{name}": (binary_dir / name, 0o755) for name in BINARIES}
    for binary, _ in files.values():
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise SystemExit(f"Missing executable {binary}")
    files.update({relative: (ROOT / relative, 0o755 if relative.endswith(".sh") else 0o644)
                  for relative in SUPPORT_FILES})
    if console_dir is not None:
        for path in console_dir.rglob("*"):
            relative = path.relative_to(console_dir)
            if any(part.startswith(".") for part in relative.parts):
                continue
            if path.is_symlink():
                raise SystemExit(f"Console bundle cannot include a symbolic link at {relative}")
            if path.is_file():
                files[f"console/{relative.as_posix()}"] = (path, 0o644)
        if "console/index.html" not in files:
            raise SystemExit("Pass a compiled frontend directory with index.html to --console-dir.")
    manifest = json.dumps(metadata, indent=2, sort_keys=True).encode() + b"\n"
    archive = stage / f"{name}.tar.gz"
    with archive.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as gz:
        with tarfile.open(fileobj=gz, mode="w", format=tarfile.PAX_FORMAT) as tar:
            info = tarfile.TarInfo(f"{name}/VERSION.json")
            info.mode, info.size = 0o644, len(manifest)
            tar.addfile(info, io.BytesIO(manifest))
            for relative, (path, mode) in sorted(files.items()):
                info = tarfile.TarInfo(f"{name}/{relative}")
                info.mode, info.size = mode, path.stat().st_size
                with path.open("rb") as file:
                    tar.addfile(info, file)
    return archive


def smoke_package(archive, name, pg_bin):
    """Run the extracted launcher with an explicit tool PATH and disposable state."""
    with TemporaryDirectory(prefix="tc-package-") as temp:
        work = Path(temp)
        with tarfile.open(archive, "r:gz") as tar:
            tar.extractall(work, filter="data")
        package = work / name
        for entry_point in (package / MEDIA_WORKER / "server.py", package / OPERATIONS / "monitor.py"):
            run([sys.executable, "-B", entry_point, "--help"],
                cwd=package, env={"LC_ALL": "C"}, stdout=subprocess.DEVNULL, timeout=10)
        tool_dir = work / "tools"
        tool_dir.mkdir()
        required = ["bash", "curl", "jq", "openssl", "mktemp", "rm", "rmdir", "cmp", "cut", "od", "tr", "sleep"]
        required.append("shasum" if shutil.which("shasum") else "sha256sum")
        # macOS shasum uses Perl through /usr/bin/env.
        if required[-1] == "shasum":
            required.append("perl")
        for tool in required:
            path = shutil.which(tool)
            if path is None:
                raise SystemExit(f"Install {tool} to verify the native package.")
            (tool_dir / tool).symlink_to(Path(path).resolve())
        env = {"PATH": os.pathsep.join(map(str, (tool_dir, pg_bin))), "LC_ALL": "C", "TMPDIR": temp}
        with ExitStack() as ports:
            listeners = [ports.enter_context(socket.socket()) for _ in range(4)]
            for listener in listeners:
                listener.bind(("127.0.0.1", 0))
            api_port, db_port, connector_port, web_port = [sock.getsockname()[1] for sock in listeners]
        api = f"http://localhost:{api_port}"
        launcher, state = package / "bin/tokencraft", work / "state"
        command = [launcher, "dev",
                   "--api-port", str(api_port), "--db-port", str(db_port),
                   "--connector-port", str(connector_port), "--web-port", str(web_port)]
        console = package / "console"
        if console.is_dir():
            command += ["--console-dir", console]
        opener = build_opener(ProxyHandler({}))

        def launch(state_path):
            return packaged_gateway([*command, "--state-dir", state_path], state_path, env, opener, api)

        with launch(state):
            revision = json.loads((package / "VERSION.json").read_bytes()).get("source_revision")
            if revision is not None:
                with opener.open(api + "/ready", timeout=3) as readiness:
                    if readiness.headers.get_all("TokenCraft-Source-Revision") != [revision]:
                        raise RuntimeError("The packaged gateway source revision differs from VERSION.json.")
            with opener.open(api + "/openapi.json", timeout=3) as running_contract:
                if json.load(running_contract) != json.loads((package / "contracts/openapi.json").read_bytes()):
                    raise RuntimeError("The packaged API contract differs from the running gateway.")
            if console.is_dir():
                route = Request(f"http://localhost:{web_port}/pricing", headers={"Accept": "text/html"})
                with opener.open(route, timeout=3) as result:
                    marker = f'<meta name="tokencraft-api-port" content="{api_port}">'.encode()
                    if marker not in result.read():
                        raise RuntimeError("The packaged console did not receive the active API port.")
            run([tool_dir / "bash", package / "examples/connector/smoke.sh", api],
                cwd=package, env=env, timeout=90)
            workflow = prepare_workflow(api_port, web_port)
            parked = defer_workflow(work, state, pg_bin, db_port, env, workflow.receipt)
        verify_backup(launcher, api_port, env, state, launch, workflow, parked)


@contextmanager
def packaged_gateway(command, state, env, opener, api):
    work = state.parent
    log = work / "gateway.log"
    with log.open("wb") as output, subprocess.Popen(
        command, cwd=work, env=env, stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.STDOUT,
    ) as gateway:
        try:
            deadline = monotonic() + 45
            while gateway.poll() is None and monotonic() < deadline:
                try:
                    with opener.open(api + "/ready", timeout=1) as ready:
                        if json.load(ready).get("ready"):
                            break
                except (URLError, TimeoutError):
                    pass
                sleep(.2)
            else:
                raise RuntimeError("The packaged gateway did not start.")
            yield
        except Exception:
            print(log.read_text(errors="replace"), flush=True)
            postgres_log = state / "postgres.log"
            if postgres_log.is_file():
                print(postgres_log.read_text(errors="replace")[-8000:], flush=True)
            raise
        finally:
            if gateway.poll() is None:
                gateway.terminate()
                try:
                    gateway.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    gateway.kill()
                    gateway.wait()
                    raise RuntimeError("The packaged launcher did not stop cleanly.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--source-revision", help="Source Git commit to record in VERSION.json")
    parser.add_argument("--bin-dir", type=Path, help="Package matching executables from a previous native build")
    parser.add_argument("--console-dir", type=Path, help="Add a compiled TokenCraft frontend to the package")
    parser.add_argument("--profile", choices=("dev", "release"), default="release")
    parser.add_argument("--offline", action="store_true", help="Use cached Rust dependencies")
    args = parser.parse_args()
    if args.source_revision and not re.fullmatch(r"[0-9a-f]{40}", args.source_revision):
        parser.error("--source-revision must be a full Git commit SHA")
    pg_bin = postgres_bin()
    version = tomllib.loads((ROOT / "crates/bin/tokencraft/Cargo.toml").read_text())["package"]["version"]
    host = host_identifier()
    suffix = "-console" if args.console_dir is not None else ""
    name = f"tokencraft-{version}-{host}{suffix}"
    meta = {"version": version, "platform": host, "source_revision": args.source_revision,
            "build_profile": args.profile if args.bin_dir is None else None, "console": args.console_dir is not None}
    binary_dir = args.bin_dir
    if binary_dir is None:
        target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")).resolve()
        binary_dir = build_binaries(pg_bin, target, args.profile, args.offline, source_revision=args.source_revision)

    args.output.mkdir(parents=True, exist_ok=True)
    with TemporaryDirectory(prefix=".package-", dir=args.output) as temp:
        stage = Path(temp)
        archive = archive_package(binary_dir, stage, name, meta, args.console_dir)
        smoke_package(archive, name, pg_bin)
        with archive.open("rb") as file:
            digest = file_digest(file, "sha256").hexdigest()
        checksum = archive.with_name(archive.name + ".sha256")
        checksum.write_text(f"{digest}  {archive.name}\n")
        archive.replace(args.output / archive.name)
        checksum.replace(args.output / checksum.name)
    print(f"Verified {args.output / archive.name}")


if __name__ == "__main__":
    main()
