import assert from "node:assert/strict";
import test from "node:test";
import { CdpClient, isRetiredInterception } from "./cdp.mjs";

test("a hung renderer fails its browser command instead of waiting forever", async () => {
  const client = new CdpClient("ws://unused");
  client.socket = { send() {}, close() {} };
  await assert.rejects(
    client.send("Runtime.evaluate", {}, 10),
    /CDP timed out/,
  );
  assert.equal(client.pending.size, 0);
  client.close();
});

test("only expired Fetch handles are treated as retired browser requests", () => {
  assert(
    isRetiredInterception(
      new Error("Fetch.fulfillRequest: Invalid InterceptionId."),
    ),
  );
  assert(
    !isRetiredInterception(
      new Error("Fetch.fulfillRequest: Invalid parameters"),
    ),
  );
  assert(
    !isRetiredInterception(
      new Error("Runtime.evaluate: Invalid InterceptionId."),
    ),
  );
});
