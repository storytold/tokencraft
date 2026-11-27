import assert from "node:assert/strict";
import test from "node:test";
import { defaultImageAspectRatio, validAspectRatio } from "../frontend/src/surfaces/requests/modelOptions.ts";

test("image defaults choose square only when the catalog offers it", () => {
  assert.equal(defaultImageAspectRatio(["4:3", "1:1"]), "1:1");
  assert.equal(defaultImageAspectRatio(["2:3", "9:16"]), "2:3");
  assert.equal(defaultImageAspectRatio(["16:9"]), "16:9");
  assert.equal(defaultImageAspectRatio([]), "1:1");
});

test("ratio syntax matches the gateway and accepts valid custom ratios", () => {
  for (const ratio of ["1:1", "7:5", "08:006", "100:100"]) {
    assert.equal(validAspectRatio(ratio), true, ratio);
  }
  for (const ratio of ["", "1:", "1:1:1", "1:1\n", "0:1", "101:1", "0001:1", "-2:1", " 1:1", "1.5:2"]) {
    assert.equal(validAspectRatio(ratio), false, ratio);
  }
});
