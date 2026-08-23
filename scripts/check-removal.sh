#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
temporary_project="$(mktemp "${repo_root}/.lenso-removal.XXXXXX.json")"
temporary_openai_project="$(mktemp "${repo_root}/.lenso-openai-removal.XXXXXX.json")"
trap 'rm -f "${temporary_project}" "${temporary_openai_project}"' EXIT

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

node - \
  "${repo_root}/lenso.openai.json" \
  "${repo_root}/lenso.json" \
  "${temporary_openai_project}" <<'NODE'
const fs = require("node:fs");
const [openaiSource, fixtureSource, target] = process.argv.slice(2);
const project = JSON.parse(fs.readFileSync(openaiSource, "utf8"));
const fixture = JSON.parse(fs.readFileSync(fixtureSource, "utf8"));
const fixtureModel = fixture.composition.modules.find(
  (module) => module.key === "model",
);
project.composition.modules = project.composition.modules.filter(
  (module) => module.key !== "model" && module.key !== "secrets",
);
project.composition.modules.push(fixtureModel);
project.composition.bindings = project.composition.bindings.filter(
  (binding) => binding.consumer !== "model",
);
delete project.packages["lenso.agent.model.openai-compatible"];
delete project.packages["lenso.secrets.env"];
project.packages["lenso.agent.model.fixture"] =
  fixture.packages["lenso.agent.model.fixture"];
project.contracts = project.contracts.filter(
  (contract) => contract.capability_id !== "lenso.secrets@1",
);
fs.writeFileSync(target, `${JSON.stringify(project, null, 2)}\n`);
NODE

lenso check \
  --project "${temporary_openai_project}" \
  --execution-class lenso.native-rust@1
