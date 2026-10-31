import { test } from "node:test";
import assert from "node:assert/strict";
import { checkVersions } from "./check-release.mjs";

const sources = () => ({
  package: { version: "1.0.1" }, tauri: { version: "1.0.1" },
  npmLock: { version: "1.0.1", packages: { "": { version: "1.0.1" } } },
  cargo: '[workspace.package]\nversion = "1.0.1"\n',
  cargoLock: '[[package]]\nname = "fluxlide"\nversion = "1.0.1"\n[[package]]\nname = "fluxlide-engine"\nversion = "1.0.1"\n',
});
test("all manifests and lockfiles must match the tag", () => {
  assert.equal(checkVersions("v1.0.1", sources()), "1.0.1");
  assert.throws(() => checkVersions("v1.0.0", sources()));
  const mismatch = sources();
  mismatch.npmLock.packages[""].version = "1.0.0";
  assert.throws(() => checkVersions("v1.0.1", mismatch));
  const missing = sources();
  missing.cargoLock = "";
  assert.throws(() => checkVersions("v1.0.1", missing));
});
