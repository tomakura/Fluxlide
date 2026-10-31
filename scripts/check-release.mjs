import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

export function checkVersions(tag, sources) {
  const version = sources.package.version;
  if (!/^\d+\.\d+\.\d+$/.test(version) || tag !== `v${version}`) {
    throw new Error(`Release tag ${tag} does not match version ${version}`);
  }
  const cargoVersion = sources.cargo.match(/\[workspace\.package\][\s\S]*?^version\s*=\s*"([^"]+)"/m)?.[1];
  const versions = [sources.tauri.version, sources.npmLock.version, sources.npmLock.packages[""].version, cargoVersion];
  for (const name of ["fluxlide", "fluxlide-engine"]) {
    versions.push(sources.cargoLock.split("[[package]]").find((part) => part.match(new RegExp(`^name = "${name}"$`, "m")))?.match(/^version = "([^"]+)"/m)?.[1]);
  }
  if (versions.some((value) => value !== version)) throw new Error(`Versions disagree: ${[version, ...versions].join(", ")}`);
  return version;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const json = (file) => JSON.parse(readFileSync(file, "utf8"));
  const pkg = json("package.json");
  const version = checkVersions(process.argv[2] ?? `v${pkg.version}`, {
    package: pkg, tauri: json("src-tauri/tauri.conf.json"), npmLock: json("package-lock.json"),
    cargo: readFileSync("Cargo.toml", "utf8"), cargoLock: readFileSync("Cargo.lock", "utf8"),
  });
  console.log(version);
}
