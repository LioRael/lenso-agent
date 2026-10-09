import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { copyFile, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const manifest = JSON.parse(await readFile(join(root, "package.json"), "utf8"));
const ordinary = Bun.argv.includes("--ordinary");
const npm = ordinary && Bun.argv.includes("--npm");
assert(manifest.exports?.["."] && manifest.exports?.["./pi"] && manifest.exports?.["./sqlite"] &&
  manifest.exports?.["./manage"] && manifest.exports?.["./plugin"] && manifest.exports?.["./fetch"],
  "Integrate the public exports before running the packed consumer.");
const source = JSON.parse(await readFile(join(root, "vendor/source.json"), "utf8"));
assert(/^[0-9a-f]{40}$/i.test(source.revision) && source.dirtySource === false,
  "Vendor source metadata must pin a clean framework source commit.");

async function run(command: string[], cwd: string) {
  const process = Bun.spawn(command, { cwd, stdout: "inherit", stderr: "inherit" });
  assert.equal(await process.exited, 0, `failed: ${command.join(" ")}`);
}

const directory = await mkdtemp(join(tmpdir(), "lenso-agent-consumer-"));
try {
  await run(["bun", "run", "build"], root);
  const packs = join(directory, "packs");
  const consumer = join(directory, "consumer");
  const vendor = join(consumer, "vendor");
  await mkdir(packs);
  await mkdir(consumer);
  await run(["bun", "pm", "pack", "--destination", packs], root);
  const tarballs = (await readdir(packs)).filter(file => file.endsWith(".tgz"));
  assert.equal(tarballs.length, 1);
  await copyFile(join(packs, tarballs[0]), join(consumer, "agent.tgz"));
  const overrides: Record<string, string> = {};
  const dependencies: Record<string, string> = { "@lenso/agent": "file:./agent.tgz" };
  if (!ordinary) {
    await mkdir(vendor);
    await copyFile(join(root, "vendor/source.json"), join(vendor, "source.json"));
    for (const name of ["@lenso/core", "@lenso/engine", "@lenso/manage", "@lenso/auth"]) {
      const reference = manifest.dependencies?.[name] ?? manifest.devDependencies?.[name];
      assert(typeof reference === "string" && /^\d+\.\d+\.\d+$/.test(reference), `No pinned version for ${name}`);
      const relative = `vendor/lenso-${name.slice("@lenso/".length)}-${reference}.tgz`;
      const expected = source.sha256?.[relative.slice("vendor/".length)];
      assert(typeof expected === "string" && /^[0-9a-f]{64}$/.test(expected), `Missing source digest for ${name}`);
      const actual = createHash("sha256").update(await readFile(join(root, relative))).digest("hex");
      assert.equal(actual, expected, `Vendor source digest mismatch for ${name}`);
      await copyFile(join(root, relative), join(consumer, relative));
      // This mode deliberately tests retained framework artifacts, not ordinary installation.
      dependencies[name] = `file:./${relative}`;
      overrides[name] = `file:./${relative}`;
    }
  } else {
    assert(!manifest.overrides, "Ordinary candidate must not contain author overrides.");
    assert(Object.values(manifest.dependencies).every(value => typeof value === "string" && !/^(file:|link:|workspace:|\/)/.test(value)),
      "Ordinary candidate must depend on registry packages only.");
    dependencies["@lenso/auth"] = manifest.devDependencies["@lenso/auth"];
  }
  for (const name of ["@earendil-works/pi-ai", "@earendil-works/pi-agent-core", "zod"])
    dependencies[name] = manifest.dependencies?.[name] ?? manifest.devDependencies?.[name];
  const install = npm
    ? ["npm", "install", "--registry=https://registry.npmjs.org", "--no-audit", "--no-fund"]
    : ["bun", "install"];
  const publicImports = Object.keys(manifest.exports).map(key => key === "." ? "@lenso/agent" : `@lenso/agent/${key.slice(2)}`);
  if (ordinary) {
    await writeFile(join(consumer, "package.json"), JSON.stringify({
      name: "minimal-agent-consumer", private: true, type: "module",
      dependencies: { "@lenso/agent": "file:./agent.tgz" },
    }, null, 2));
    await run(install, consumer);
    await writeFile(join(consumer, "minimal.mjs"), publicImports
      .map(name => `await import(${JSON.stringify(name)});`).join("\n"));
    await run(["bun", "minimal.mjs"], consumer);
    console.log("Minimal ordinary consumer installed and imported all entries with only the candidate dependency.");
  }
  await writeFile(join(consumer, "package.json"), JSON.stringify({
    name: "packed-agent-consumer", private: true, type: "module", dependencies,
    ...(!ordinary ? { overrides } : {}),
    devDependencies: { typescript: manifest.devDependencies.typescript, "@types/bun": manifest.devDependencies["@types/bun"] },
  }, null, 2));
  await run(install, consumer);
  const lockFile = npm ? "package-lock.json" : "bun.lock";
  const lockBefore = await readFile(join(consumer, lockFile), "utf8");
  if (ordinary) {
    assert(!lockBefore.includes("vendor/"), "Ordinary install resolved a vendor snapshot.");
    assert(!lockBefore.includes(root), "Ordinary install leaked the author checkout path.");
  }
  for (const name of Object.keys(overrides)) {
    const entry = lockBefore.split("\n").find(line => line.trimStart().startsWith(`"${name}": [`));
    assert(entry?.includes("vendor/") && entry.includes(".tgz"), `Registry fallback detected for ${name}`);
  }
  await rm(join(consumer, "node_modules"), { recursive: true, force: true });
  await run(npm
    ? ["npm", "ci", "--registry=https://registry.npmjs.org", "--no-audit", "--no-fund"]
    : ["bun", "install", "--frozen-lockfile"], consumer);
  assert.equal(await readFile(join(consumer, lockFile), "utf8"), lockBefore, "Frozen reinstall changed consumer lockfile.");

  await writeFile(join(consumer, "imports.ts"), [
    'import assert from "node:assert/strict";',
    ...publicImports.map((name, index) => `import * as export${index} from ${JSON.stringify(name)};`),
    ...publicImports.map((_name, index) => `assert(export${index} !== undefined);`),
    'console.log("Imported every packed public export.");',
  ].join("\n"));
  await writeFile(join(consumer, "consumer.ts"), `
import { createAgentService, createMemoryStore, type AgentOptions, type AgentService } from "@lenso/agent";
import { type PiExtensions } from "@lenso/agent/pi";
import { createSqliteStore, migrateAgentDatabase } from "@lenso/agent/sqlite";
import { createManageToolSource } from "@lenso/agent/manage";
import { createAgentPlugin } from "@lenso/agent/plugin";
import { createAgentFetchHandler } from "@lenso/agent/fetch";
import { createFauxCore } from "@earendil-works/pi-ai/providers/faux";
const core = createFauxCore({ provider: "consumer-types" });
const pi: PiExtensions = { streamFn: core.streamSimple };
const options: AgentOptions<{}> = {
  store: createMemoryStore(), model: core.models[0],
  identity: async () => ({ subject: "alice", scope: "owner:alice", application: "consumer", target: "fixture" }),
  profiles: [{ id: "read", instructions: "Use only selected read tools.", tools: [] }],
  tools: { catalog: async () => [], prepare: async () => { throw new Error("no-tools"); }, invoke: async () => { throw new Error("no-tools"); } },
};
const agent: AgentService<{}> = createAgentService(options, pi);
const handler: (request: Request) => Promise<Response | undefined> = createAgentFetchHandler({ agent, authenticate: async () => ({}) });
const plugin = createAgentPlugin({ id: "agent", requires: [], options: () => options, pi });
void [handler, plugin, createManageToolSource, createSqliteStore, migrateAgentDatabase];
await agent.close();
`);
  await writeFile(join(consumer, "tsconfig.json"), JSON.stringify({
    compilerOptions: {
      target: "ES2023", module: "ESNext", moduleResolution: "Bundler", strict: true,
      skipLibCheck: !Bun.argv.includes("--check-dependency-types"), noEmit: true, types: ["bun"],
    },
    include: ["*.ts", "examples/*.ts"],
  }, null, 2));
  await mkdir(join(consumer, "examples"));
  // Reuse the actual real-Pi/Auth/Manage fixture against packed public entry
  // points. Only imports in temporary copies change; no source path escapes.
  const replacements: Record<string, string> = {
    "../src/runtime": "@lenso/agent", "../src/types": "@lenso/agent",
    "../src/sqlite": "@lenso/agent/sqlite", "../src/manage": "@lenso/agent/manage",
    "../src/pi": "@lenso/agent/pi",
  };
  for (const file of ["notes.ts", "console-profile.ts", "missing-receipt.ts", "offline.ts", "provider-config.ts"]) {
    let contents = await readFile(join(root, "examples", file), "utf8");
    for (const [from, to] of Object.entries(replacements))
      contents = contents.replaceAll(`"${from}"`, `"${to}"`);
    assert(!contents.includes("../src/"), `Non-public import in ${file}`);
    await writeFile(join(consumer, "examples", file), contents);
  }
  await run(["bun", "run", "imports.ts"], consumer);
  await run(["bun", "x", "--no-install", "tsc", "--noEmit"], consumer);
  await run(["bun", "run", "consumer.ts"], consumer);
  await run(["bun", "run", "examples/offline.ts"], consumer);
  console.log(`${ordinary ? `Ordinary ${npm ? "npm" : "Bun"} consumer (no overrides)` : "Framework snapshot integration (explicit overrides)"}: frozen reinstall, all exports, TypeScript and real offline Pi/Auth/Manage workflow passed.`);
} finally {
  await rm(directory, { recursive: true, force: true });
}
