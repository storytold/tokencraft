"""Check the interactive design's source references against the checkout."""

import json
from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[1]
DESIGN = ROOT / "frontend/public/design/index.html"


def design_data(html, name):
    declaration = f"\n  const {name} = "
    _, separator, tail = html.partition(declaration)
    if not separator:
        raise ValueError(f"Design is missing the {name} declaration")
    return json.JSONDecoder().raw_decode(tail)[0]


class DesignContractTest(unittest.TestCase):
    def test_source_references(self):
        html = DESIGN.read_text(encoding="utf-8")
        flows = design_data(html, "flows")
        sources = design_data(html, "sources")
        references = set()
        for flow in flows:
            references.update(key for node in flow["nodes"] for key in node[7])
            references.update(key for edge in flow["edges"] for key in edge[5])
        self.assertEqual(references, sources.keys())

        line_counts = {}
        for key, source in sources.items():
            if source["href"]:
                continue
            with self.subTest(reference=key):
                path = ROOT / source["path"]
                self.assertTrue(path.is_file(), source["path"])
                label = source["lineLabel"]
                if not label:
                    continue
                span = re.fullmatch(r":([1-9]\d*)(?:–([1-9]\d*))?", label)
                self.assertIsNotNone(span, label)
                first = int(span[1])
                last = int(span[2] or span[1])
                if path not in line_counts:
                    line_counts[path] = len(path.read_bytes().splitlines())
                self.assertLessEqual(first, last, label)
                self.assertLessEqual(last, line_counts[path], source["path"])


if __name__ == "__main__":
    unittest.main()
