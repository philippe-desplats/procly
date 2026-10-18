#!/usr/bin/env node
// Checks the assumption of ADR 0007: AdonisJS, Next.js and dotenv-cli keep a variable that is already present in
// the environment over the value of the same name in a `.env` file.
//
// It installs the tools at pinned versions in a temporary directory, writes every env file each one loads in
// development, sets `FOO=from_procly` in the environment and reads `FOO` back through the loader of each tool.
// It needs the network, so it is not part of `make check`. Run it with `node scripts/check-env-precedence.mjs`,
// and record the versions and the outcome in ADR 0007 when a version below changes.
//
// Nothing is written outside the system temporary directory, no server is started and no port is opened.

import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const VERSIONS = {
  "dotenv-cli": "11.0.0",
  next: "16.4.2",
  "@adonisjs/core": "7.6.0",
};

const FROM_PROCLY = "from_procly";
const FROM_FILE = "from_file";

const root = mkdtempSync(join(tmpdir(), "procly-env-check-"));
const results = [];

function record(tool, scenario, actual) {
  results.push({ tool, scenario, ok: actual === FROM_PROCLY, actual });
}

/** An app directory holding the given env files, each one defining FOO and an `ONLY_IN_<file>` variable. */
function app(name, files) {
  const dir = join(root, name);
  mkdirSync(dir, { recursive: true });
  for (const file of files) {
    const only = `ONLY_IN_${file.replace(/\W/g, "_").toUpperCase()}`;
    writeFileSync(join(dir, file), `FOO=${FROM_FILE}\n${only}=1\n`);
  }
  return dir;
}

/** The environment of the tool: a clean one, so that nothing of the developer's leaks in, plus FOO. */
function environment(extra = {}) {
  return { PATH: process.env.PATH, FOO: FROM_PROCLY, ...extra };
}

try {
  writeFileSync(join(root, "package.json"), JSON.stringify({ name: "env-check", private: true }));
  const install = Object.entries(VERSIONS).map(([name, version]) => `${name}@${version}`);
  execFileSync("npm", ["install", "--ignore-scripts", "--no-audit", "--no-fund", "--save-exact", ...install, "react", "react-dom"], {
    cwd: root,
    env: { ...environment(), npm_config_cache: join(root, "npm-cache"), HOME: root },
    stdio: "inherit",
  });

  const installed = (name) => JSON.parse(readFileSync(join(root, "node_modules", name, "package.json"), "utf8"));
  const versions = {
    "dotenv-cli": installed("dotenv-cli").version,
    next: installed("next").version,
    "@next/env": installed("@next/env").version,
    "@adonisjs/core": installed("@adonisjs/core").version,
    "@adonisjs/env": installed("@adonisjs/env").version,
  };

  // dotenv-cli: its own command, with and without the cascade of `-c`.
  const dotenv = join(root, "node_modules/.bin/dotenv");
  const read = ["node", "-p", "process.env.FOO"];
  const cli = app("dotenv-cli", [".env", ".env.local", ".env.development", ".env.development.local"]);
  record("dotenv-cli", "dotenv -- (.env)", execFileSync(dotenv, ["--", ...read], { cwd: cli, env: environment() }).toString().trim());
  record("dotenv-cli", "dotenv -c development -- (.env, .env.local, .env.development, .env.development.local)", execFileSync(dotenv, ["-c", "development", "--", ...read], { cwd: cli, env: environment() }).toString().trim());

  // Next.js: the loader `next dev` calls, `loadEnvConfig(dir, dev = true)`, over the 4 files it reads in development.
  const next = app("next", [".env", ".env.local", ".env.development", ".env.development.local"]);
  const nextScript = `const { loadEnvConfig } = require('@next/env'); loadEnvConfig(${JSON.stringify(next)}, true, { info() {}, error() {} }); console.log(process.env.FOO)`;
  record("next", "loadEnvConfig(dev) (.env, .env.local, .env.development, .env.development.local)", execFileSync("node", ["-e", nextScript], { cwd: root, env: environment() }).toString().trim());

  // AdonisJS: the `Env` class of `start/env.ts`, with and without NODE_ENV so that both sets of files are read.
  const adonis = app("adonis", [".env", ".env.local", ".env.development", ".env.development.local"]);
  for (const [scenario, extra] of [
    ["Env.create (.env, .env.local)", {}],
    ["Env.create, NODE_ENV=development (.env, .env.local, .env.development, .env.development.local)", { NODE_ENV: "development" }],
  ]) {
    const script = `import { Env } from '@adonisjs/env'; const env = await Env.create(${JSON.stringify(adonis + "/")}, { FOO: Env.schema.string() }); console.log(env.get('FOO'))`;
    record("adonis", scenario, execFileSync("node", ["--input-type=module", "-e", script], { cwd: root, env: environment(extra) }).toString().trim());
  }

  // Not a failure, a limit worth knowing: what each loader does with a variable that is set to the empty string.
  const empty = { ...environment(), FOO: "" };
  const emptyResults = {
    "dotenv-cli": execFileSync(dotenv, ["--", ...read], { cwd: cli, env: empty }).toString().trim(),
    next: execFileSync("node", ["-e", nextScript], { cwd: root, env: empty }).toString().trim(),
    adonis: execFileSync("node", ["--input-type=module", "-e", `import { Env } from '@adonisjs/env'; const env = await Env.create(${JSON.stringify(adonis + "/")}, { FOO: Env.schema.string.optional() }); console.log(env.get('FOO'))`], { cwd: root, env: empty }).toString().trim(),
  };

  console.log(`\nversions: ${JSON.stringify(versions)}`);
  console.log(`FOO set to the empty string, the file value is ${FROM_FILE}: ${JSON.stringify(emptyResults)}`);
  for (const { tool, scenario, ok, actual } of results) {
    console.log(`${ok ? "keeps the environment" : "OVERRIDES the environment"}  ${tool}  ${scenario}  FOO=${actual}`);
  }
  process.exitCode = results.every(({ ok }) => ok) ? 0 : 1;
} finally {
  rmSync(root, { recursive: true, force: true });
}
