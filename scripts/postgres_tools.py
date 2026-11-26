"""Locate a matching suite of PostgreSQL server and client executables."""

import os
from pathlib import Path
import shutil
import subprocess


SERVER_TOOLS = ("postgres", "initdb", "pg_ctl", "pg_isready", "createdb", "psql")


def postgres_bin():
    def complete(folder):
        return all((folder / tool).is_file() and os.access(folder / tool, os.X_OK) for tool in SERVER_TOOLS)

    for folder in map(Path, os.get_exec_path()):
        if complete(folder):
            return folder.resolve()

    pg_config = shutil.which("pg_config")
    if pg_config:
        folder = Path(subprocess.check_output([pg_config, "--bindir"], text=True, timeout=5).strip())
        if complete(folder):
            return folder.resolve()
    raise SystemExit("Put a complete PostgreSQL 17 installation or its pg_config on PATH.")
