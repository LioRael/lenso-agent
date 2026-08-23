#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
temporary_project="$(mktemp "${repo_root}/.lenso-removal.XXXXXX.json")"
trap 'rm -f "${temporary_project}"' EXIT

node - "${repo_root}/lenso.json" "${temporary_project}" <<'NODE'
const fs = require("node:fs");
const [source, target] = process.argv.slice(2);
const project = JSON.parse(fs.readFileSync(source, "utf8"));
project.composition.modules = project.composition.modules.filter(
  (module) => module.key !== "workspace-read",
);
project.composition.bindings = project.composition.bindings.filter(
  (binding) => binding.provider !== "workspace-read",
);
delete project.packages["lenso.agent.workspace-read"];
fs.writeFileSync(target, `${JSON.stringify(project, null, 2)}\n`);
NODE

lenso check \
  --project "${temporary_project}" \
  --execution-class lenso.native-rust@1
