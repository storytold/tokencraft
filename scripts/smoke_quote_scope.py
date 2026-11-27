"""Verify scoped quote binding through native, catalog, and media entry points."""

import argparse
from pathlib import Path
from secrets import token_hex
from tempfile import TemporaryDirectory

from smoke_route_performance import ranking_api
from smoke_workflow_contract import call, rehearse


def verify(gateway, directory, settings, session, account, project):
    with ranking_api(gateway, directory, settings) as (port, connector):
        def key(name, client=None):
            path = f"/v1/projects/{project}/service-principals?account_id={account}"
            principal, _ = call(port, path, 201, method="POST", headers=session, body={
                "name": name, "client_ref": client, "scopes": ["requests:submit", "requests:read"],
            })
            return {"Authorization": "Bearer " + principal["token"]}

        shared = key("Shared project key")
        client_a = key("Client A", "client-a")
        client_b = key("Client B", "client-b")
        workload = {"model": "example_image", "workload": {"kind": "image", "count": 1, "aspect_ratio": "1:1"}}
        prompt = "Quote scope fixture"

        def quote(path, headers=shared):
            return call(port, path, 200, method="POST", headers=headers, body=workload)[0]

        def keyed(headers):
            return {**headers, "Idempotency-Key": token_hex(16)}

        def submit(offer, headers, status, client=None):
            result, _ = call(port, "/v1/requests", status, method="POST", headers=headers, body={
                "quote_id": offer["id"], "input": offer["input"], "client_ref": client, "image_prompt": prompt,
            })
            if status == 409:
                assert result["error"]["code"] == "quote_stale"
            return result

        path = "/v1/quotes/scoped"
        scoped = quote(path + "?client_ref=client-a")
        shared_request = keyed(shared)
        submit(scoped, shared_request, 409, "client-b")
        submit(scoped, shared_request, 409)
        receipt = submit(scoped, shared_request, 202, "client-a")
        assert submit(scoped, shared_request, 200, "client-a") == receipt
        submit(scoped, keyed(client_b), 409)
        submit(scoped, keyed(client_a), 202)

        project_quote = quote(path)
        assert project_quote["id"] != scoped["id"]
        project_request = keyed(shared)
        submit(project_quote, project_request, 409, "client-a")
        submit(project_quote, project_request, 202)

        catalog_quote = quote("/v1/catalog/quotes/scoped", client_a)
        submit(catalog_quote, keyed(client_b), 409)
        submit(catalog_quote, keyed(client_a), 202)

        media = {"model": "example_image", "task": "text_to_image", "aspect_ratio": "1:1", "prompt": prompt}
        header = "X-TokenCraft-Client-Ref"
        preview, _ = call(port, "/v1/image", 200, method="POST", headers={**shared, header: "client-a"},
                          body={**media, "dry_run": True})
        flat = {**media, "quote_id": preview["id"]}
        flat_request = keyed(shared)
        stale, _ = call(port, "/v1/image", 409, method="POST", headers={**flat_request, header: "client-b"}, body=flat)
        assert stale["error"]["code"] == "quote_stale"
        media_receipt, _ = call(port, "/v1/image", 202, method="POST",
                                headers={**flat_request, header: "client-a"}, body=flat)
        call(port, media_receipt["status_url"], 200, headers=client_a)
        call(port, media_receipt["status_url"], 404, headers=client_b)
        assert connector.jobs == {}
    print("PASS native, catalog, and media quotes bind the effective client and preserve accepted replays")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path)
    parser.add_argument("admin", type=Path)
    args = parser.parse_args()
    with TemporaryDirectory(prefix="tokencraft-quote-scope-") as directory:
        rehearse(args.gateway.resolve(strict=True), args.admin.resolve(strict=True), Path(directory), verification=verify)


if __name__ == "__main__":
    main()
