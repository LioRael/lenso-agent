import fs from "node:fs";
import path from "node:path";
import ts from "typescript-ast";
const request = JSON.parse(await Bun.stdin.text());
if(request.schema !== "lenso.convention-compile.v1") throw new Error("Unsupported convention request");
const root=request.entry, output=request.output;
const entries=["tools.ts","tools.rs"].filter(name=>fs.existsSync(path.join(root,name)));
if(entries.length>1) throw new Error("agent/ cannot contain both tools.ts and tools.rs");
const hasTools=entries.length===1;
const entry=hasTools ? path.join(root,entries[0]) : null;
if(entry&&!fs.lstatSync(entry).isFile()) throw new Error("Tool entry must be a regular file");
const deployment=readDeployment(root,request,hasTools);
fs.writeFileSync(path.join(output,"lenso-agent-deployment.json"),JSON.stringify(deployment,null,2));
if(!hasTools) {
 // A Profile-only composition is data, not a disabled or empty Tool Provider.
 // The Engine's generic resource-only result keeps it out of the App's Plugin
 // resolver, Bundles inventory, and language dependency installation path.
 fs.writeFileSync(path.join(output,"lenso.convention-resources.json"),JSON.stringify({
  schema:"lenso.convention-resources.v1",
  resources:[{path:"lenso-agent-deployment.json",schema:"lenso.agent.deployment@2"}],
 },null,2));
} else if(entries[0]==="tools.rs") {
 // Rust authors retain the ordinary Plugin/Tool Provider macros. The portable
 // facade lets the Engine lower the same source to both Wasm and Process
 // bundles, so an App build never needs to link an authored Tool into its Host.
 // Private dependencies are declared in agent/Cargo.toml and read only after
 // this convention has been selected.
 const metadata=fs.existsSync(path.join(root,"Cargo.toml")) ? Bun.TOML.parse(fs.readFileSync(path.join(root,"Cargo.toml"),"utf8")) : {};
 const deps=rustDependencies(root,metadata.dependencies);
 const packageName=request.plugin_id.replaceAll(".","-");
 fs.mkdirSync(path.join(output,"src"));
 fs.writeFileSync(path.join(output,"Cargo.toml"),`[package]\nname=${JSON.stringify(packageName)}\nversion=${JSON.stringify(request.release_version)}\nedition="2024"\n[workspace]\n[package.metadata.lenso]\nplugin-id=${JSON.stringify(request.plugin_id)}\nroot-slot="tool-providers"\n[package.metadata.lenso-cli]\noutputs=["wasm","process"]\npublished_resources=[{path="lenso-agent-deployment.json",schema="lenso.agent.deployment@1"}]\n[lib]\ncrate-type=["cdylib"]\n[dependencies]\n${deps.join("\n")}\n[[bin]]\nname=${JSON.stringify(packageName)}\npath="src/main.rs"\n`);
 fs.writeFileSync(path.join(output,"src/lib.rs"),`include!(${JSON.stringify(entry)});\n`);
 fs.writeFileSync(path.join(output,"src/main.rs"),'// Cargo Process entrypoint; the SDK supplies main and protocol lowering.\ninclude!("lib.rs");\n');
} else {
 const manifest=fs.existsSync(path.join(root,"package.json")) ? JSON.parse(fs.readFileSync(path.join(root,"package.json"),"utf8")) : {};
 if(fs.existsSync(path.join(root,"package.json"))) {
   const installed=Bun.spawnSync(["bun","install","--ignore-scripts","--no-save"],{cwd:root,stdout:"pipe",stderr:"inherit"});
   process.stderr.write(installed.stdout);
   if(installed.exitCode!==0) throw new Error("Agent surface installation failed");
 }
 const source = ts.createSourceFile(entry, fs.readFileSync(entry,"utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
 const transformed = ts.transform(source, [context => tree => {
   function visit(node) {
     if (ts.isStringLiteral(node) && node.text.startsWith(".") &&
         (ts.isImportDeclaration(node.parent) || ts.isExportDeclaration(node.parent))) {
       return ts.factory.createStringLiteral(path.resolve(root,node.text));
     }
     return ts.visitEachChild(node,visit,context);
   }
   return ts.visitNode(tree,visit);
 }]);
 const normalized = transformed.transformed[0];
 let binding="__lenso_convention_tools";
 while(source.text.includes(binding)) binding+="_";
 const statements=normalized.statements.flatMap(statement=>ts.isExportAssignment(statement) && !statement.isExportEquals ? [
   ts.factory.createVariableStatement(undefined,ts.factory.createVariableDeclarationList([ts.factory.createVariableDeclaration(binding,undefined,undefined,statement.expression)],ts.NodeFlags.Const)),
   ts.factory.createExportAssignment(undefined,false,ts.factory.createIdentifier(binding)),
 ] : [statement]);
 fs.writeFileSync(path.join(output,"tools.ts"),ts.createPrinter().printFile(ts.factory.updateSourceFile(normalized,statements)));
 transformed.dispose();
 fs.writeFileSync(path.join(output,"plugin.ts"),`import {definePlugin} from "@lenso/bun-plugin";\nimport declaration from ${JSON.stringify("./tools.ts")};\nexport default definePlugin({providers:[declaration]});\n`);
 const dependencies = Object.fromEntries(Object.entries(manifest.dependencies ?? {}).map(([name,version]) => [name,
 typeof version === "string" && /^(file|link):/.test(version) ? `file:${path.resolve(root,version.slice(version.indexOf(":")+1))}` : version]));
 fs.writeFileSync(path.join(output,"package.json"),JSON.stringify({name:request.plugin_id,version:request.release_version,private:true,type:"module",dependencies:{...dependencies,"@lenso/bun-plugin":"0.4.1","@lenso/agent-tool-sdk":"0.1.0"},devDependencies:{typescript:"7.0.2","@types/bun":"1.4.0"},scripts:{check:"tsc --noEmit"},lenso:{pluginId:request.plugin_id,rootSlot:"tool-providers",runtime:"bun",source:"plugin.ts",published_resources:[{path:"lenso-agent-deployment.json",schema:"lenso.agent.deployment@1"}]}},null,2));
 fs.writeFileSync(path.join(output,"tsconfig.json"),JSON.stringify({compilerOptions:{strict:true,noEmit:true,module:"Preserve",moduleResolution:"bundler",allowImportingTsExtensions:true,types:["bun"],paths:{"@lenso/agent-tool-sdk":[path.join(output,"node_modules/@lenso/agent-tool-sdk/dist/index.d.ts")],"@lenso/agent-tool-sdk/schema":[path.join(output,"node_modules/@lenso/agent-tool-sdk/dist/schema.d.ts")]}},include:["plugin.ts"]}));
}
process.stdout.write(JSON.stringify({schema:"lenso.convention-compiled.v1"}));

function readDeployment(root,request,hasTools) {
 const profilePath=path.join(root,"profile.toml");
 const instructionsPath=path.join(root,"instructions.md");
 const hasProfile=fs.existsSync(profilePath),hasInstructions=fs.existsSync(instructionsPath);
 if(hasInstructions&&!hasProfile) throw new Error("agent/instructions.md requires agent/profile.toml");
 let profile=null;
 if(hasProfile) {
  const stat=fs.lstatSync(profilePath);
  if(!stat.isFile()||stat.size>256*1024) throw new Error("Agent Profile source must be a regular TOML file no larger than 256 KiB");
  const source_toml=fs.readFileSync(profilePath,"utf8");
  const document=Bun.TOML.parse(source_toml);
  if(!isRecord(document.lenso)||Object.keys(document.lenso).length!==1||typeof document.lenso.profile!=="string"||!validProfileName(document.lenso.profile)) {
   throw new Error("agent/profile.toml requires [lenso] with one valid profile name");
  }
  if(!Array.isArray(document.instances)||document.instances.some(value=>typeof value!=="string"||value.trim()==="")) throw new Error("Agent Profile source requires instances = [..] of nonempty strings");
  if(!Array.isArray(document.allowed_tools)) throw new Error("Agent Profile source must declare allowed_tools explicitly");
  if(hasInstructions&&typeof document.instructions==="string"&&document.instructions.trim()!=="") throw new Error("Agent Profile source cannot set instructions when instructions.md is present");
  profile={name:document.lenso.profile,source_toml,instructions:null};
  if(hasInstructions) {
   const stat=fs.lstatSync(instructionsPath);
   if(!stat.isFile()||stat.size===0||stat.size>65536) throw new Error("Agent instructions must be a regular Markdown file containing 1 through 65536 bytes");
   const instructions=fs.readFileSync(instructionsPath,"utf8");
   if(instructions.trim()==="") throw new Error("Agent instructions must not be blank");
   profile.instructions=instructions;
  }
 }
 if(!hasTools) {
  if(!profile) throw new Error("agent/ requires tools.ts, tools.rs, or profile.toml");
  return {schema:"lenso.agent.deployment@2",contribution:{id:request.plugin_id},profile};
 }
 return {schema:"lenso.agent.deployment@1",plugin:{id:request.plugin_id,instance:"default"},profile};
}

function isRecord(value) { return typeof value==="object"&&value!==null&&!Array.isArray(value); }
function validProfileName(value) { return /^[a-z0-9][a-z0-9_-]{0,63}$/.test(value); }

function rustDependencies(root, authored) {
 const defaults={
  lenso:{package:"lenso-plugin-sdk",version:"0.4.5"},
  "lenso-agent-tool-sdk":"0.3.3",
  schemars:"1",
  serde:{version:"1",features:["derive"]},
 };
 if(authored!==undefined&&!isRecord(authored)) throw new Error("agent/Cargo.toml dependencies must be a table");
 const dependencies={...defaults,...(authored??{})};
 const lenso=dependencies.lenso;
 if(!isRecord(lenso)||lenso.package!=="lenso-plugin-sdk") {
  throw new Error("Rust Agent surfaces must use lenso-plugin-sdk through lenso = { package = \\\"lenso-plugin-sdk\\\", ... }");
 }
 return Object.entries(dependencies).map(([name,spec])=> {
  if(typeof spec==="string") return `${tomlKey(name)} = ${JSON.stringify(spec)}`;
  if(!isRecord(spec)) throw new Error(`dependency ${name} must be a version string or table`);
  if(spec.workspace) throw new Error("Agent surface dependencies must be independently resolvable");
 const normalized={...spec};
 if(typeof normalized.path==="string") normalized.path=path.resolve(root,normalized.path);
  return `${tomlKey(name)} = ${tomlInline(normalized)}`;
 });
}

function tomlInline(value) {
 if(typeof value==="string"||typeof value==="number"||typeof value==="boolean") return JSON.stringify(value);
 if(Array.isArray(value)) return `[${value.map(tomlInline).join(", ")}]`;
 if(isRecord(value)) return `{ ${Object.entries(value).map(([key,item])=>`${tomlKey(key)} = ${tomlInline(item)}`).join(", ")} }`;
 throw new Error("Agent surface dependency values must be TOML scalars, arrays, or tables");
}

function tomlKey(key) { return /^[A-Za-z0-9_-]+$/.test(key) ? key : JSON.stringify(key); }
