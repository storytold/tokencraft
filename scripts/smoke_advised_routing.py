"""Exercise advised media previews, quote admission, and direct requests with two models."""

import argparse
from pathlib import Path
from secrets import token_hex
from tempfile import TemporaryDirectory

from smoke_route_performance import ranking_api, seed_history
from smoke_workflow_contract import call, rehearse


def verify(gateway, directory, settings, session, account, project):
    with ranking_api(gateway, directory, settings, steady_model="steady_image") as (port, connector):
        def key(client):
            path = f"/v1/projects/{project}/service-principals?account_id={account}"
            row, _ = call(port, path, 201, method="POST", headers=session, body={
                "name": client, "client_ref": client, "scopes": ["requests:submit", "requests:read"],
            })
            return {"Authorization": "Bearer " + row["token"]}

        def policy(revision, enabled):
            call(port, f"/v1/routing-policy?account_id={account}", 200, method="PUT", headers=session, body={
                "expected_revision": revision,
                "policy": {"route_ranking": "recent_reliability", "advisor_enabled": enabled},
            })

        auth = key("client-a")
        workload = {"kind": "image", "count": 1, "aspect_ratio": "1:1"}
        original, _ = call(port, "/v1/quotes/scoped", 200, method="POST", headers=auth,
                           body={"model": "auto", "workload": workload})
        media = {"task": "text_to_image", "count": 1, "aspect_ratio": "1:1", "prompt": "Advised routing fixture"}
        donor, _ = call(port, "/v1/requests", 202, method="POST", headers={**auth, "Idempotency-Key": token_hex(16)},
                        body={"quote_id": original["id"], "input": original["input"], "image_prompt": media["prompt"]})
        seed_history(directory, settings, donor["id"])

        def preview(headers=auth, status=200, **fields):
            return call(port, "/v1/image", status, method="POST", headers=headers,
                        body={**media, **fields, "dry_run": True})[0]

        assert "enable Advisor routing" in preview(status=400)["error"]["message"]
        policy(0, True)
        auto, _ = call(port, "/v1/quotes/scoped", 200, method="POST", headers=auth,
                       body={"model": "auto", "workload": workload})
        assert auto["selected_offer_id"] == "steady"
        advised = preview()
        expected = {"model": "steady_image", "source": "policy", "used_advisor": False}
        assert advised["advisor"] == expected, advised["advisor"]
        assert advised["input"]["model"] == expected["model"]
        assert preview(model="advise")["advisor"] == {**expected, "source": "request"}
        assert preview(headers=key("client-b"))["advisor"]["model"] == "example_image"
        assert preview(headers={**auth, "X-TokenCraft-Max-Cost-Nanos": "150"})["advisor"]["model"] == "example_image"
        below_threshold = preview(min_usage_accounts=5)
        assert "advisor" not in below_threshold and all(not row["selected"] for row in below_threshold["candidates"])

        def submit(headers, status=202, **fields):
            return call(port, "/v1/image", status, method="POST", headers=headers, body={**media, **fields})[0]

        direct_key = {**auth, "Idempotency-Key": token_hex(16)}
        direct = submit(direct_key)
        reviewed = submit({**auth, "Idempotency-Key": token_hex(16)}, model=advised["input"]["model"], quote_id=advised["id"])
        for receipt in (direct, reviewed):
            record, _ = call(port, f"/v1/requests/{receipt['id']}", 200, headers=auth)
            view, _ = call(port, receipt["status_url"], 200, headers=auth)
            assert view["model"] == "steady_image" and record["quote"]["selected_offer_id"] == "steady"
            if receipt == direct:
                assert record["quote"]["advisor"] == expected
        policy(1, False)
        assert submit(direct_key, 200) == direct
        submit({**auth, "Idempotency-Key": token_hex(16)}, 400)
        assert preview(model="advise")["advisor"]["model"] == "steady_image"
        assert connector.jobs == {}
    print("PASS advised previews, reviewed quotes and direct requests used scoped ranking and respected policy")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path)
    parser.add_argument("admin", type=Path)
    args = parser.parse_args()
    with TemporaryDirectory(prefix="tokencraft-advised-routing-") as directory:
        rehearse(args.gateway.resolve(strict=True), args.admin.resolve(strict=True), Path(directory), verification=verify)


if __name__ == "__main__":
    main()
