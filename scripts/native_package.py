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
from urllib.parse import quote, unquote, urlsplit
from urllib.request import ProxyHandler, Request, build_opener

from postgres_tools import postgres_bin
from smoke_native_backup import defer_workflow, packaged_delivery_tool, prepare_webhook, prepare_workflow, verify_backup
from smoke_workflow_contract import provision, webhook_receiver


ROOT = Path(__file__).resolve().parents[1]
BINARIES = ("tokencraft", "api-gateway", "admin-cli")
REFERENCE_CATALOG = "catalog/artcraft-v1.json"
PRIMITIVE_SOURCE = "crates/core/provider-mapping/schemas/v1.json"
PRIMITIVE_CONTRACT = "contracts/connector-primitives.json"
INFERENCE = "plugins/providers/inference"
PLUGIN_COMMON = ("LICENSE", "README.md", "bindings.example.json", "catalog.example.json",
                 "index.mjs", "package.json", "service-auth.mjs", "service-auth.test.mjs")
PLUGIN_FILES = {
    "fal": ("fal-webhooks.mjs", "models.mjs", "provider.mjs", "provider.test.mjs", "queue.mjs",
            "tenant-keys.mjs", "tenant-keys.test.mjs"),
    "kinovi": ("provider.mjs", "provider.test.mjs", "queue.mjs", "tenant-keys.mjs", "tenant-keys.test.mjs"),
    "openrouter": ("proxy.mjs", "proxy.test.mjs"),
}
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
    "LICENSE", "SECURITY.md", REFERENCE_CATALOG, "contracts/openapi.json", PRIMITIVE_SOURCE,
    "docs/README.md", "docs/api-contracts.md", "docs/api-schema.md", "docs/capacity.md",
    "docs/setup.md", "docs/deployment.md", "docs/workflows.md", "docs/media-tools.md",
    "docs/provider-connectors.md", "docs/customer-data.md", "docs/event-archives.md",
    "docs/chat-history.md", "docs/request-diagnostics.md", "docs/provider-mapping.md",
    "examples/connector/catalog.json", "examples/connector/bindings.json", "examples/connector/smoke.sh",
    "examples/mapping/example-image.map.json", "examples/mapping/request.json", "examples/mapping/response.json",
    "examples/mapping/custom-chat.map.json", "examples/mapping/chat-request.json", "examples/mapping/chat-response.json",
    "examples/postgres/export_to_postgres.py", "examples/postgres/postgres_usage_views.sql",
    "examples/postgres/grant_sync_role.sql", "examples/postgres/smoke.py",
    "frontend/public/design/index.html", "sdk/rust/README.md", "sdk/typescript/README.md",
    "scripts/smoke_hosted_console.sh", "scripts/smoke_authenticated_gateway.py",
    "deploy/postgres/grant-runtime.sql", "deploy/haproxy/cell-api.cfg",
    "deploy/autoscaling/collect.py", "deploy/autoscaling/controller.py", "deploy/autoscaling/signals.sql",
    "plugins/providers/README.md", f"{INFERENCE}/README.md", "plugins/providers/compute/README.md",
    "plugins/providers/compute/examples/two-target-chat/catalog.json",
    "plugins/providers/compute/examples/two-target-chat/chat-bindings.json",
    *(f"{INFERENCE}/{provider}/{name}" for provider, files in PLUGIN_FILES.items() for name in (*PLUGIN_COMMON, *files)),
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
    if source_revision is None:
        env.pop("TOKENCRAFT_SOURCE_REVISION", None)
    else:
        env["TOKENCRAFT_SOURCE_REVISION"] = source_revision
    directory = target / ("debug" if cargo_profile == "dev" else cargo_profile)
    # Keep the PostgreSQL socket path short while letting the build database use TMPDIR.
    with (
        TemporaryDirectory(prefix="tc-build-") as temp,
        TemporaryDirectory(prefix="tc-pg-", dir="/tmp") as socket_dir,
    ):
        work = Path(temp)
        db = work / "postgres"
        log = work / "postgres.log"
        settings = [str(pg_bin / "pg_ctl"), "-D", str(db)]
        run([pg_bin / "initdb", "-D", db, "--username=tokencraft", "--locale=C",
             "--encoding=UTF8", "--auth-local=trust", "--auth-host=reject"],
            stdout=subprocess.DEVNULL)
        options = shlex.join(["-k", socket_dir, "-c", "listen_addresses="])
        try:
            run([*settings, "-l", log, "-o", options, "-w", "start"])
            build_env = {**env, "DATABASE_URL": f"postgresql://tokencraft@localhost/postgres?host={quote(socket_dir, safe='')}"}
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
    files["README.md"] = (ROOT / "docs/native-package.md", 0o644)
    files[PRIMITIVE_CONTRACT] = (ROOT / PRIMITIVE_SOURCE, 0o644)
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


def verify_console(opener, origin, package, api_port, revision):
    route = Request(origin + "/pricing", headers={"Accept": "text/html"})
    with opener.open(route, timeout=3) as result:
        marker = f'<meta name="tokencraft-api-port" content="{api_port}">'.encode()
        if marker not in result.read():
            raise RuntimeError("The packaged console did not receive the active API port.")
    for route, expected in (("docs/openapi.json", "contracts/openapi.json"),
                            (REFERENCE_CATALOG, REFERENCE_CATALOG)):
        asset = (package / expected).read_bytes()
        with opener.open(f"{origin}/{route}", timeout=3) as result:
            if result.read(len(asset) + 1) != asset:
                raise RuntimeError(f"The packaged console {route} differs from the release asset.")
    if revision is not None:
        with opener.open(origin + "/release.json", timeout=3) as result:
            manifest = json.load(result)
            if not isinstance(manifest, dict) or manifest.get("source_revision") != revision:
                raise RuntimeError("The packaged console source revision differs from VERSION.json.")


def verify_migrations(package, state, pg_bin, db_port, env):
    """Compare a fresh migration by the packaged CLI with the launcher's database."""
    password = json.loads((state / "dev-keys.json").read_bytes())["database"]
    db_env = dict(env, PGHOST="127.0.0.1", PGPORT=str(db_port), PGUSER="tokencraft", PGPASSWORD=password)
    database = "tokencraft_package_check"
    run([pg_bin / "createdb", "--maintenance-db=postgres", "-w", database],
        cwd=package, env=db_env, timeout=10)
    url = f"postgresql://tokencraft:{quote(password, safe='')}@127.0.0.1:{db_port}/{database}"
    cli = package / "bin/admin-cli"
    cli_env = dict(env, FOUNDATION_ENV="development", DATABASE_URL=url)
    run([cli, "migrate"], cwd=package, env=cli_env, stdout=subprocess.DEVNULL, timeout=45)

    def migration_ledger(name):
        return run(
            [pg_bin / "psql", "-XqAtw", "-v", "ON_ERROR_STOP=1", "-d", name, "-c",
             "SELECT version, success, encode(checksum, 'hex') FROM public._sqlx_migrations ORDER BY version"],
            cwd=package, env=db_env, stdout=subprocess.PIPE, timeout=10,
        ).stdout

    expected = migration_ledger("tokencraft_dev")
    if not expected or migration_ledger(database) != expected:
        raise RuntimeError("The packaged admin-cli and launcher disagree on the migration ledger.")
    report = run([cli, "schema-status"], cwd=package, env=cli_env,
                 stdout=subprocess.PIPE, text=True, timeout=10)
    if not report.stdout.startswith("schema status: current\n"):
        raise RuntimeError("The packaged admin-cli did not report a current migration ledger.")
    print("PASS packaged admin-cli migrated a fresh database and matched the launcher migration checksums")


def verify_documentation(package):
    """Check relative inline links in the guides shipped with the native archive."""
    documents = ("README.md", *(path for path in SUPPORT_FILES if path.endswith(".md")))
    for path in documents:
        document = package / path
        for link in re.findall(r"\]\(([^()\s]+)\)", document.read_text(encoding="utf-8")):
            url = urlsplit(link)
            if url.scheme or url.netloc or not url.path or url.path.startswith("/"):
                continue
            target = (document.parent / unquote(url.path)).resolve()
            if not target.is_relative_to(package) or not target.exists():
                raise RuntimeError(f"Broken package link in {path}: {link}")
    print("PASS packaged guides link to files in the extracted archive")


def verify_mapping_example(package, env):
    result = run(
        [package / "bin/tokencraft", "mapping", "preview", "--file", "examples/mapping/example-image.map.json",
         "--direction", "request", "--input", "examples/mapping/request.json"],
        cwd=package, env=env, stdout=subprocess.PIPE, timeout=10,
    )
    if json.loads(result.stdout) != {"description": "Two colored samples", "layout": "Landscape", "outputs": 2}:
        raise RuntimeError("The packaged image mapping preview did not match the documented fixture.")
    print("PASS packaged CLI previewed the bundled image mapping")


def verify_binary_versions(package, version, revision):
    source = revision if revision is not None else "unstamped"
    for name in BINARIES:
        try:
            result = run([package / "bin" / name, "--version"], cwd=package, env={"LC_ALL": "C"},
                         stdin=subprocess.DEVNULL, capture_output=True, timeout=10)
        except (OSError, subprocess.SubprocessError) as error:
            raise RuntimeError(f"The packaged {name} could not report its build version.") from error
        if result.stdout != f"{name} {version} ({source})\n".encode():
            raise RuntimeError(f"The packaged {name} version or source revision differs from VERSION.json.")
    print("PASS packaged launcher, gateway and admin-cli match VERSION.json")


def smoke_package(archive, name, pg_bin):
    """Run the extracted launcher with an explicit tool PATH and disposable state."""
    with TemporaryDirectory(prefix="tc-package-") as temp, ExitStack() as fixtures:
        work = Path(temp)
        with tarfile.open(archive, "r:gz") as tar:
            tar.extractall(work, filter="data")
        package = work / name
        manifest = json.loads((package / "VERSION.json").read_bytes())
        revision = manifest["source_revision"]
        verify_binary_versions(package, manifest["version"], revision)
        verify_documentation(package.resolve())
        plugin_args = [argument for provider in PLUGIN_FILES
                       for argument in ("--plugin-catalog", package / INFERENCE / provider / "catalog.example.json")]
        run([package / "bin/api-gateway", "catalog", "check-reference", "--reference", package / REFERENCE_CATALOG,
             *plugin_args], cwd=package, env={"LC_ALL": "C"}, stdout=subprocess.DEVNULL, timeout=10)
        for entry_point in (package / MEDIA_WORKER / "server.py", package / OPERATIONS / "monitor.py",
                            package / "scripts/smoke_authenticated_gateway.py"):
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
        verify_mapping_example(package, env)
        receiver = fixtures.enter_context(webhook_receiver())
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
        webhook_config = work / "webhooks.json"
        webhook_config.write_text(json.dumps({
            "destinations": {"native": f"http://127.0.0.1:{receiver.server_port}/events"},
        }), encoding="utf-8")
        command += ["--webhook-config", webhook_config]
        console = package / "console"
        if console.is_dir():
            command += ["--console-dir", console]
        opener = build_opener(ProxyHandler({}))
        tool_registry = None
        tool_env = {}

        def launch(state_path):
            args = [*command, "--state-dir", state_path]
            if tool_registry is not None:
                args += ["--workflow-tools", tool_registry]
            return packaged_gateway(args, state_path, {**env, **tool_env}, opener, api)

        with launch(state):
            verify_migrations(package, state, pg_bin, db_port, env)
            if revision is not None:
                with opener.open(api + "/ready", timeout=3) as readiness:
                    if readiness.headers.get_all("TokenCraft-Source-Revision") != [revision]:
                        raise RuntimeError("The packaged gateway source revision differs from VERSION.json.")
            with opener.open(api + "/openapi.json", timeout=3) as running_contract:
                if json.load(running_contract) != json.loads((package / "contracts/openapi.json").read_bytes()):
                    raise RuntimeError("The packaged API contract differs from the running gateway.")
            with opener.open(api + "/v1/schemas/primitives", timeout=3) as running_registry:
                if running_registry.read() != (package / PRIMITIVE_CONTRACT).read_bytes():
                    raise RuntimeError("The packaged connector primitive registry differs from the running gateway.")
            if console.is_dir():
                verify_console(opener, f"http://localhost:{web_port}", package, api_port, revision)
            run([tool_dir / "bash", package / "examples/connector/smoke.sh", api],
                cwd=package, env=env, timeout=90)
            verify_webhook = prepare_webhook(api_port, web_port, receiver)
            verify_webhook()
            print("PASS packaged launcher delivered and retried a signed localhost webhook")
            session, account, project = provision(api_port, origin=f"http://localhost:{web_port}")
        tool_registry, tool_env = fixtures.enter_context(
            packaged_delivery_tool(work, package / MEDIA_WORKER, env, account, project))
        with launch(state):
            workflow = prepare_workflow(api_port, session, account, project)
            parked = defer_workflow(work, state, pg_bin, db_port, env, workflow.receipt)
        verify_backup(launcher, api_port, env, state, launch, workflow, parked, verify_webhook)


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
    if args.source_revision is not None and not re.fullmatch(r"[0-9a-f]{40}", args.source_revision):
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
