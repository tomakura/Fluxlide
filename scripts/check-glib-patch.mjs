import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";

assert.match(readFileSync("Cargo.toml", "utf8"), /\[patch\.crates-io\][\s\S]*?glib = \{ path = "vendor\/glib" \}/);
const block = readFileSync("Cargo.lock", "utf8").split("[[package]]").find((part) => /^name = "glib"$/m.test(part));
assert.ok(block && !/^source =/m.test(block), "GLib must resolve to the local backport, not the registry");
const hash = createHash("sha256").update(readFileSync("vendor/glib/src/variant_iter.rs", "utf8").replace(/\r\n/g, "\n")).digest("hex");
assert.equal(hash, "a0f5ee8acb8faa089bcdfbc9a57372609fce7654026ccef7d9a224d05a654ccc", "The reviewed GLib safety backport changed");
console.log("GLib resolves to the reviewed upstream safety backport");
