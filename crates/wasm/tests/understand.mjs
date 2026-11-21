// The whole pipeline as WebAssembly (pkg/, scripts/build-wasm.sh), with a host
// function standing in for the Worker's fetch: what it's asked to post, and
// what comes back from a canned Anthropic reply and from a failing one. Run
// from the repository's root:
//
//   scripts/build-wasm.sh && node crates/wasm/tests/understand.mjs

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { initSync, understand } from "../pkg/spendyard_core.js";

initSync({ module: readFileSync("crates/wasm/pkg/spendyard_core_bg.wasm") });

const receipt = readFileSync("crates/core/tests/fixtures/receipt.eml");
const reply = readFileSync("crates/wasm/tests/fixtures/anthropic-reply.json", "utf8");

const options = JSON.stringify({
  extractor: { provider: "anthropic", model: "claude-haiku-5-5", apiKey: "sk-ant-test", baseUrl: "https://gateway.example/anthropic" },
  received: "2026-11-19T09:31:00Z",
});

/** A host function that remembers what it was asked to post, and answers with `status` and `body`. */
function answering(status, body, asked) {
  return async (url, headers, payload) => {
    asked.push({ url, headers: Object.fromEntries(JSON.parse(headers)), body: JSON.parse(payload) });

    return JSON.stringify({ status, body });
  };
}

// Read with a canned reply: the request the CLI's Anthropic provider makes, and the result and rows from its answer.
{
  const asked = [];
  const answer = JSON.parse(await understand(receipt, options, answering(200, reply, asked)));

  assert.equal(answer.error, undefined, answer.error);
  assert.equal(asked.length, 1, "one call: the extractor, no first model");
  assert.equal(asked[0].url, "https://gateway.example/anthropic/v1/messages");
  assert.equal(asked[0].headers["x-api-key"], "sk-ant-test");
  assert.ok(asked[0].headers["anthropic-version"]);
  assert.equal(asked[0].body.model, "claude-haiku-5-5");

  assert.equal(answer.document.messageId, "receipt-2291-0007@acme.example");
  assert.equal(answer.result.vendor.name, "Vercel");
  assert.equal(answer.result.meta.extractor, "anthropic/claude-haiku-5-5");
  assert.equal(answer.rows.receipt.total, "20.00");
  assert.equal(answer.rows.receipt.card_last4, "3776");
  assert.equal(answer.rows.receipt.seats, 1);
  assert.equal(answer.rows.lines.length, 1);
  assert.equal(answer.rows.charges, null);

  console.log("read with a canned reply: Vercel, 20.00 USD, rows for the store");
}

// The provider failing: an error the host can show, and from which stage.
{
  const asked = [];
  const overloaded = JSON.stringify({ type: "error", error: { type: "overloaded_error", message: "Overloaded" } });
  const answer = JSON.parse(await understand(receipt, options, answering(529, overloaded, asked)));

  assert.equal(answer.stage, "understand");
  assert.match(answer.error, /529.*Overloaded/);

  console.log(`a failing provider: "${answer.error}"`);
}

// The host's function throwing: no response came.
{
  const answer = JSON.parse(await understand(receipt, options, async () => { throw new Error("network down"); }));

  assert.equal(answer.stage, "understand");
  assert.match(answer.error, /network down/);

  console.log(`a failing host: "${answer.error}"`);
}

// Options it can't use.
{
  const answer = JSON.parse(await understand(receipt, JSON.stringify({ extractor: { provider: "nope" } }), answering(200, reply, [])));

  assert.equal(answer.stage, "options");

  console.log(`unknown options: "${answer.error}"`);
}
