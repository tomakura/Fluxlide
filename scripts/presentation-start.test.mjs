import { readFileSync } from "node:fs";
import vm from "node:vm";
import assert from "node:assert/strict";
import test from "node:test";
import ts from "typescript";

// Exercise the actual page handlers with delayed IPC. Svelte's lifecycle and
// state wrappers are inert here; the DOM flow is checked separately in Playwright.
const source = readFileSync(new URL("../src/routes/+page.svelte", import.meta.url), "utf8")
  .split('<script lang="ts">')[1].split("</script>")[0].replace(/^\s*import .*$/gm, "");
const handlers = ts.transpileModule(source + `
  globalThis.handlers = { start, leave, state: () => ({ screen, starting, leaving }),
    setEditor: () => { screen = "editor"; } };
`, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None } }).outputText;

for (const delay of ["native", "remember"]) {
  for (const action of ["home", "app"]) {
    test(`leaving for ${action} cancels a delayed ${delay} presentation start`, async () => {
      const calls = [];
      let release;
      const gate = new Promise((resolve) => { release = resolve; });
      const context = vm.createContext({
        $state: (v) => v, $derived: (v) => v, $effect: () => {}, onMount: () => {},
        setTimeout, clearTimeout, exporting: { finished: null },
        open: { state: { session: 1 }, failed: 0, flush: async () => {}, take(v) { this.state = v; } },
        api: {
          setOutputScreen: async () => { calls.push("remember"); await gate; },
          startPresentation: async () => { calls.push("start"); await gate; calls.push("started"); },
          stopPresentation: async () => { calls.push("stop"); },
          closeProject: async () => { calls.push("close"); },
          exitApp: async () => { calls.push("exit"); },
        },
      });
      vm.runInContext(handlers, context);
      const page = context.handlers;
      page.setEditor();
      const start = page.start({ index: 0, name: "test" }, delay === "remember");
      for (let i = 0; i < 10 && calls.length === 0; i++) await Promise.resolve();
      assert.deepEqual(calls, [delay === "native" ? "start" : "remember"]);
      const leave = page.leave(action);
      for (let i = 0; i < 10; i++) await Promise.resolve();
      if (delay === "native") assert.deepEqual(calls, ["start"], "close must wait for native cleanup");
      release();
      await Promise.all([start, leave]);
      const end = action === "home" ? "close" : "exit";
      assert.deepEqual(calls, delay === "native" ? ["start", "started", "stop", end] : ["remember", end]);
      assert.equal(page.state().screen, action === "home" ? "home" : "editor");
      assert.equal(page.state().starting, false);
      assert.equal(page.state().leaving, false);
    });
  }
}
