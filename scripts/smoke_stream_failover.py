"""Verify live DNS discovery and chat stream recovery in a disposable cell."""

import argparse
from contextlib import ExitStack, closing, contextmanager
from csv import DictReader
from http.client import HTTPConnection, IncompleteRead
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from io import StringIO
import json
from pathlib import Path
import socket
from socketserver import BaseRequestHandler, ThreadingUDPServer
import struct
from tempfile import TemporaryDirectory
from threading import Event, Lock

from smoke_cell_faults import FORWARDED, eventually, serve
from smoke_workflow_contract import (
    HOST, ROOT, assert_private_response, call, loopback_port, ready, rehearse, service,
)


CHAT = "/v1/chat/completions"
MODEL = "fixture-stream"
SERVICE = "_tokencraft._tcp.cell.test"
FIRST, PEER = "first.cell.test", "peer.cell.test"
IPV4 = socket.inet_aton(HOST)


def dns_name(name):
    return b"".join(bytes([len(label)]) + label.encode("ascii") for label in name.split(".")) + b"\0"


def dns_record(name, kind, data):
    return dns_name(name) + struct.pack("!HHIH", kind, 1, 1, len(data)) + data


class Discovery(ThreadingUDPServer):
    def __init__(self, primary_port, peer_port):
        super().__init__((HOST, 0), DnsQuery)
        self.ports = {FIRST: primary_port, PEER: peer_port}
        self.lock = Lock()
        self.publish(FIRST)

    def publish(self, *members):
        with self.lock:
            self.members = members


class DnsQuery(BaseRequestHandler):
    def handle(self):
        packet, transport = self.request
        if len(packet) < 17 or struct.unpack_from("!H", packet, 4)[0] != 1:
            return
        end = packet.find(b"\0", 12)
        if end < 0 or end + 5 > len(packet):
            return
        question = packet[12:end + 1]
        kind, dns_class = struct.unpack_from("!HH", packet, end + 1)
        if dns_class != 1:
            return

        answers, addresses = [], []
        ports = self.server.ports
        with self.server.lock:
            members = self.server.members
        if question == dns_name(SERVICE) and kind == 33:
            for member in members:
                records = struct.pack("!HHH", 0, 10, ports[member]) + dns_name(member)
                answers.append(dns_record(SERVICE, 33, records))
                addresses.append(dns_record(member, 1, IPV4))
        elif kind == 1:
            answers = [dns_record(name, 1, IPV4) for name in members if question == dns_name(name)]

        flags = 0x8480 | (struct.unpack_from("!H", packet, 2)[0] & 0x0100)
        header = packet[:2] + struct.pack("!HHHHH", flags, 1, len(answers), 0, len(addresses))
        transport.sendto(header + packet[12:end + 5] + b"".join(answers + addresses), self.client_address)


class Inference(ThreadingHTTPServer):
    def __init__(self):
        super().__init__((HOST, 0), InferenceRequest)
        self.release = Event()
        self.lock = Lock()
        self.submissions = 0


class InferenceRequest(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if self.path != CHAT or request["model"] != MODEL or request["stream"] is not True:
            self.send_error(400)
            return
        with self.server.lock:
            self.server.submissions += 1
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        self.frame("first")
        if self.server.release.wait(30):
            self.frame("second", "stop", {"prompt_tokens": 2, "completion_tokens": 2})
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()

    def frame(self, content, finish=None, usage=None):
        payload = {
            "id": "upstream-fixture", "model": MODEL, "usage": usage,
            "choices": [{"index": 0, "delta": {"content": content}, "finish_reason": finish}],
        }
        self.wfile.write(b"data: " + json.dumps(payload).encode() + b"\n\n")
        self.wfile.flush()


def configure_chat(directory, model_port):
    catalog, bindings = directory / "stream-catalog.json", directory / "stream-bindings.json"
    catalog.write_text(json.dumps({"version": 1, "offers": [{
        "id": MODEL, "provider": "self_hosted", "model": MODEL, "upstream_model": MODEL,
        "adapter": "openai_compatible_chat", "local_binding": MODEL,
        "pricing": {"kind": "text", "usd_nanos_per_1000_input_tokens": 0, "usd_nanos_per_1000_output_tokens": 0},
    }]}))
    bindings.write_text(json.dumps({"version": 1, "bindings": [{
        "name": MODEL, "endpoint": f"http://{HOST}:{model_port}{CHAT}",
    }]}))
    return {"TOKENCRAFT_CORE_CATALOG": str(catalog), "TOKENCRAFT_LOCAL_CHAT_BINDINGS": str(bindings)}


def local_proxy(directory, dns, port, path):
    config = (ROOT / "deploy/haproxy/cell-api.cfg").read_text()
    assert config.count("    parse-resolv-conf") == 1
    config = config.replace("    parse-resolv-conf", f"    nameserver fixture {HOST}:{dns.server_address[1]}")
    config = config.replace("global\n", f"global\n    stats socket {path} mode 600 level user\n", 1)
    filename = directory / "stream-haproxy.cfg"
    filename.write_text(config)
    trusted = directory / "stream-trust.txt"
    trusted.write_text(f"{HOST}/32\n")
    return filename, {
        "TOKENCRAFT_LB_BIND": f"{HOST}:{port}", "TOKENCRAFT_LB_TRUSTED_INGRESS_FILE": str(trusted),
        "TOKENCRAFT_LB_MAX_CONNECTIONS": "64", "TOKENCRAFT_LB_CONNECTIONS_PER_API": "16",
        "TOKENCRAFT_LB_API_SLOTS": "2", "TOKENCRAFT_LB_API_ADDRESS": SERVICE,
        "TOKENCRAFT_LB_ENABLE_IP_RATE_LIMIT": "false",
    }


def proxy_stats(path, command):
    with closing(socket.socket(socket.AF_UNIX)) as client:
        client.settimeout(2)
        client.connect(str(path))
        client.sendall((command + "\n").encode())
        with client.makefile("r") as response:
            return response.read()


def api_states(path):
    _, header, *rows = proxy_stats(path, "show servers state").splitlines()
    columns = header.lstrip("# ").split()
    stats = proxy_stats(path, "show stat").removeprefix("# ")
    status = {row["svname"]: row["status"] for row in DictReader(StringIO(stats))
              if row["pxname"] == "cell_api"}
    servers = (dict(zip(columns, row.split())) for row in rows if row.strip())
    return {int(row["srv_port"]): status[row["srv_name"]] for row in servers
            if row["be_name"] == "cell_api" and row["srv_addr"] == HOST}


@contextmanager
def stream(port, headers, body):
    with closing(HTTPConnection(HOST, port, timeout=8)) as client:
        client.request("POST", CHAT, body, headers)
        response = client.getresponse()
        assert response.status == 200, response.status
        assert response.headers.get_content_type() == "text/event-stream"
        assert_private_response(response.headers)
        yield response


def frames(response):
    while line := response.readline():
        if line.startswith(b"data: "):
            data = line[6:].rstrip()
            yield data.decode() if data == b"[DONE]" else json.loads(data)


def run(gateway, directory, settings, session, account, project):
    api = int(settings["API_GATEWAY_PORT"])
    peer, worker, ingress = (loopback_port() for _ in range(3))
    with ExitStack() as stack:
        supplier = stack.enter_context(serve(Inference()))
        stack.callback(supplier.release.set)
        dns = stack.enter_context(serve(Discovery(api, peer)))
        settings = dict(settings, **configure_chat(directory, supplier.server_port))

        def start_gateway(label, port, role):
            env = dict(settings, TOKENCRAFT_PROCESS_ROLE=role, API_GATEWAY_PORT=str(port))
            return stack.enter_context(service(label, [gateway], directory, env, lambda: ready(port)))

        primary = start_gateway("stream-primary", api, "api")
        start_gateway("stream-peer", peer, "api")
        start_gateway("stream-worker", worker, "worker")
        stats_path = directory / "stats"
        config, proxy_env = local_proxy(directory, dns, ingress, stats_path)
        stack.enter_context(service("stream-ingress", ["haproxy", "-db", "-f", config], directory,
            dict(proxy_env, PATH=settings["PATH"], LANG="C"),
            lambda: call(ingress, "/_tokencraft/ingress-ready", 200)[0] == b""))
        assert api_states(stats_path) == {api: "UP"}

        def project_key(client_ref):
            grant, _ = call(api, f"/v1/projects/{project}/service-principals?account_id={account}", 201,
                headers=session, method="POST", body={"name": client_ref, "client_ref": client_ref,
                    "scopes": ["requests:submit", "requests:read"]})
            return {**FORWARDED, "Authorization": f"Bearer {grant['token']}"}

        auth = project_key("stream-client")
        other = project_key("sibling-client")
        submission = {**auth, "Content-Type": "application/json", "Idempotency-Key": "sse-recovery"}
        body = json.dumps({"model": MODEL, "stream": True, "stream_options": {"include_usage": True},
                           "messages": [{"role": "user", "content": "Two fixture chunks"}]}).encode()

        with stream(ingress, submission, body) as original:
            request_id = original.headers["TokenCraft-Request-Id"]
            assert request_id
            events = frames(original)
            preamble, first = next(events), next(events)
            assert preamble["choices"][0]["delta"] == {"role": "assistant"}
            assert first["choices"][0]["delta"] == {"content": "first"}
            assert not supplier.release.is_set()

            dns.publish(FIRST, PEER)
            eventually(lambda: api_states(stats_path) == {api: "UP", peer: "UP"},
                       "peer discovered without proxy restart")
            primary.kill()
            primary.wait(timeout=5)
            try:
                remaining = original.read()
            except IncompleteRead as error:
                remaining = error.partial
            except ConnectionResetError:
                remaining = b""
            assert b"[DONE]" not in remaining and b"second" not in remaining

        def primary_withdrawn():
            states = api_states(stats_path)
            return states.get(api, "").startswith("DOWN") and states.get(peer) == "UP"

        eventually(primary_withdrawn, "failed primary excluded while still in DNS")
        with stream(ingress, submission, body) as replay:
            assert replay.headers["TokenCraft-Request-Id"] == request_id
            events = frames(replay)
            assert [next(events), next(events)] == [preamble, first]
            supplier.release.set()
            second, finish, usage, done = list(events)
            assert second["choices"][0]["delta"] == {"content": "second"}
            assert finish["choices"][0]["finish_reason"] == "stop"
            assert usage["usage"] == {"prompt_tokens": 2, "completion_tokens": 2, "total_tokens": 4}
            assert done == "[DONE]"
        path = f"/v1/requests/{request_id}"
        receipt, _ = call(ingress, path, 200, headers=auth)
        assert receipt["status"] == "succeeded"
        call(ingress, path, 404, headers=other)

        dns.publish(PEER)
        eventually(lambda: api_states(stats_path) == {peer: "UP"}, "obsolete SRV entry removed from the proxy")
        assert call(ingress, path, 200, headers=auth)[0]["id"] == request_id
        assert supplier.submissions == 1
        print("PASS SRV discovery added and removed replicas while the original scoped stream recovered "
              "with complete output, exact usage, and one inference call", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path)
    parser.add_argument("admin", type=Path)
    args = parser.parse_args()
    gateway, admin = args.gateway.resolve(strict=True), args.admin.resolve(strict=True)
    with TemporaryDirectory(prefix="tc-stream-") as directory:
        rehearse(gateway, admin, Path(directory), verification=run)
