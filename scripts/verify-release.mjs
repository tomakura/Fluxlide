import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync, writeFileSync, statSync } from "node:fs";

const version = JSON.parse(readFileSync("package.json", "utf8")).version;
const dir = "target/release-verify";
const ffmpeg = readFileSync("scripts/build-ffmpeg.sh", "utf8").match(/^FFMPEG_VERSION=(\S+)$/m)[1];
const expected = [
  `Fluxlide_${version}_x64-setup.exe`, `Fluxlide_${version}_x64_en-US.msi`,
  `Fluxlide_${version}_aarch64.dmg`, `Fluxlide_${version}_aarch64.app.tar.gz`,
  `Fluxlide_${version}_x64.dmg`, `Fluxlide_${version}_x64.app.tar.gz`,
  `Fluxlide_${version}_amd64.deb`, `Fluxlide_${version}_amd64.AppImage`, `Fluxlide-${version}-1.x86_64.rpm`,
  `Fluxlide-${version}-source.tar.gz`, `ffmpeg-${ffmpeg}.tar.xz`, "build-ffmpeg.sh", "THIRD_PARTY_LICENSES.md",
];
for (const name of expected) if (!statSync(`${dir}/${name}`).size) throw new Error(`Empty release asset: ${name}`);
const ffmpegHash = readFileSync("scripts/build-ffmpeg.sh", "utf8").match(/^FFMPEG_SHA256=([a-f0-9]{64})$/m)[1];
if (createHash("sha256").update(readFileSync(`${dir}/ffmpeg-${ffmpeg}.tar.xz`)).digest("hex") !== ffmpegHash)
  throw new Error("Attached FFmpeg source does not match the bundled build");
writeFileSync(`${dir}/release-provenance.json`, JSON.stringify({
  version, commit: execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim(),
  workflowRun: process.env.GITHUB_RUN_ID ?? null, ffmpeg,
}, null, 2) + "\n");
const files = readdirSync(dir).filter((name) => name !== "SHA256SUMS").sort();
writeFileSync(`${dir}/SHA256SUMS`, files.map((name) => `${createHash("sha256").update(readFileSync(`${dir}/${name}`)).digest("hex")}  ${name}`).join("\n") + "\n");
console.log(`Verified ${expected.length} required assets; wrote checksums and source provenance`);
