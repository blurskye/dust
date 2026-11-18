import { test } from "node:test";
import assert from "node:assert/strict";

test("a greeting says hello by name", () => {
  const name = "Dusty";
  assert.equal(`Hello, ${name}!`, "Hello, Dusty!");
});
