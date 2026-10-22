// Writes the license notices of the Rust crates and npm packages that go
// into a Fluxlide release to target/notices/THIRD_PARTY_NOTICES.txt, which
// the release bundle carries next to THIRD_PARTY_LICENSES.md (FFmpeg and the
// font are described there by hand).
//
//   node scripts/third-party-notices.mjs
//
// Each package's own license files are copied, as MIT, BSD and Apache-2.0
// ask: packages with the same text share one copy. A package that ships no
// license file is listed with its license name.

import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const LICENSE_FILE = /^(licen[cs]e|copying|notice|unlicense)([-._].*)?$/i;

/** A package, with where its files are. */
function entry(name, version, license, dir, source) {
  const files = existsSync(dir)
    ? readdirSync(dir)
        .filter((file) => LICENSE_FILE.test(file))
        .sort()
        .map((file) => readFileSync(join(dir, file), "utf8").replace(/\r\n/g, "\n").trim())
    : [];
  return { name: `${name} ${version}`, license: license || "(not stated)", texts: files, source };
}

function rustPackages() {
  const run = (args) =>
    execFileSync("cargo", args, { cwd: root, encoding: "utf8", maxBuffer: 1 << 28 });
  // What the app links on any of its platforms, without build and dev tools.
  const shipped = new Set(
    run(["tree", "-p", "fluxlide", "-e", "normal", "--target", "all", "--prefix", "none", "-f", "{p}"])
      .split("\n")
      .map((line) => line.replace(/ \(.*$/, "").replace(/ \(\*\)$/, "").trim())
      .filter(Boolean),
  );
  const metadata = JSON.parse(run(["metadata", "--format-version", "1", "--locked"]));
  const ours = new Set(metadata.workspace_members);
  return metadata.packages
    .filter((p) => !ours.has(p.id) && shipped.has(`${p.name} v${p.version}`))
    .map((p) =>
      entry(p.name, p.version, p.license, dirname(p.manifest_path), `https://crates.io/crates/${p.name}/${p.version}`),
    );
}

function npmPackages() {
  // The runtime dependencies, and the parts of Svelte and SvelteKit that the
  // build puts into the page.
  const seen = new Map();
  const visit = (name, from) => {
    // As Node finds it: node_modules/<name> in this folder or the nearest
    // one above (a scoped package's folder is two levels down).
    let dir = join(from, "node_modules", name);
    while (!existsSync(dir)) {
      const parent = dirname(from);
      if (parent === from) throw new Error(`${name} (needed by ${from}) is not installed`);
      from = parent;
      dir = join(from, "node_modules", name);
    }
    const manifest = JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
    const key = `${manifest.name}@${manifest.version}`;
    if (seen.has(key)) return;
    seen.set(
      key,
      entry(
        manifest.name,
        manifest.version,
        manifest.license,
        dir,
        `https://www.npmjs.com/package/${manifest.name}/v/${manifest.version}`,
      ),
    );
    for (const dependency of Object.keys(manifest.dependencies ?? {})) visit(dependency, dir);
  };
  const app = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
  for (const name of [...Object.keys(app.dependencies ?? {}), "svelte", "@sveltejs/kit"]) {
    visit(name, root);
  }
  return [...seen.values()];
}

function render(title, packages) {
  packages.sort((a, b) => a.name.localeCompare(b.name));
  // Packages with the same license texts share them.
  const groups = new Map();
  for (const p of packages) {
    const key = p.texts.join("\n\n----\n\n") || `(${p.license})`;
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(p);
  }
  let out = `${"=".repeat(78)}\n${title}\n${"=".repeat(78)}\n\n`;
  for (const [text, members] of groups) {
    out += members.map((p) => `${p.name} (${p.license}) ${p.source}`).join("\n");
    out += `\n\n${text.startsWith("(") ? `No license file is included in the package; its license is ${text}.` : text}\n\n${"-".repeat(78)}\n\n`;
  }
  return out;
}

const rust = rustPackages();
const npm = npmPackages();
const out = join(root, "target", "notices");
mkdirSync(out, { recursive: true });
writeFileSync(
  join(out, "THIRD_PARTY_NOTICES.txt"),
  "Fluxlide includes the following third-party software. FFmpeg and the\n" +
    "Noto Sans JP font are described in THIRD_PARTY_LICENSES.md.\n\n" +
    "Every package is unmodified; its source code is at the address after its\n" +
    "name. The standard texts of the licenses named for packages that ship no\n" +
    "license file are at https://spdx.org/licenses/.\n\n" +
    render("Rust crates", rust) +
    render("npm packages", npm),
);
console.log(`${rust.length} Rust crates and ${npm.length} npm packages written to ${join(out, "THIRD_PARTY_NOTICES.txt")}`);
