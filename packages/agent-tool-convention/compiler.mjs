import fs from "node:fs";
import path from "node:path";
import ts from "typescript-ast";
const request = JSON.parse(await Bun.stdin.text());
if(request.schema !== "lenso.convention-compile.v1") throw new Error("Unsupported convention request");
const root=request.entry, output=request.output;
const entries=["tools.ts","tools.rs"].filter(name=>fs.existsSync(path.join(root,name)));
if(entries.length!==1) throw new Error("agent/ requires exactly one tools.ts or tools.rs");
const entry=path.join(root,entries[0]);
if(!fs.lstatSync(entry).isFile()) throw new Error("Tool entry must be a regular file");
if(entries[0]==="tools.rs") {
 // Native Rust authors retain the ordinary Plugin/Tool Provider macros. Private
 // dependencies are declared in agent/Cargo.toml, read only after selection.
 const metadata=fs.existsSync(path.join(root,"Cargo.toml")) ? Bun.TOML.parse(fs.readFileSync(path.join(root,"Cargo.toml"),"utf8")) : {};
 const deps=metadata.dependencies ?? {lenso:"0.5.23","lenso-agent-tool-sdk":"0.3.0",schemars:"1",serde:{version:"1",features:["derive"]}};
 // Preserve Cargo's manifest interpretation and resolve relative private paths
 // against the author directory instead of the generated staging directory.
 const lines=Object.entries(deps).map(([name,spec])=> {
   if(typeof spec==="string") return `${JSON.stringify(name)} = ${JSON.stringify(spec)}`;
   if(spec.workspace) throw new Error("Agent surface dependencies must be independently resolvable");
   if(spec.path) spec.path=path.resolve(root,spec.path);
   return `${JSON.stringify(name)} = { ${Object.entries(spec).map(([key,value])=>`${JSON.stringify(key)} = ${JSON.stringify(value)}`).join(", ")} }`;
 });
 fs.mkdirSync(path.join(output,"src"));
 fs.writeFileSync(path.join(output,"Cargo.toml"),`[package]\nname=${JSON.stringify(request.plugin_id.replaceAll(".","-"))}\nversion=${JSON.stringify(request.release_version)}\nedition="2024"\n[workspace]\n[package.metadata.lenso]\nplugin-id=${JSON.stringify(request.plugin_id)}\nroot-slot="tool-providers"\n[package.metadata.lenso-cli]\nruntime="native-linked"\n[dependencies]\n${lines.join("\n")}\n`);
 fs.writeFileSync(path.join(output,"src/lib.rs"),`#[path = ${JSON.stringify(entry)}] mod tool_entry;\npub use tool_entry::link_plugin;\n`);
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
 fs.writeFileSync(path.join(output,"package.json"),JSON.stringify({name:request.plugin_id,version:request.release_version,private:true,type:"module",dependencies:{...dependencies,"@lenso/bun-plugin":"0.4.1","@lenso/agent-tool-sdk":"0.1.0"},devDependencies:{typescript:"7.0.2","@types/bun":"1.4.0"},scripts:{check:"tsc --noEmit"},lenso:{pluginId:request.plugin_id,rootSlot:"tool-providers",runtime:"bun",source:"plugin.ts"}},null,2));
 fs.writeFileSync(path.join(output,"tsconfig.json"),JSON.stringify({compilerOptions:{strict:true,noEmit:true,module:"Preserve",moduleResolution:"bundler",allowImportingTsExtensions:true,types:["bun"],paths:{"@lenso/agent-tool-sdk":[path.join(output,"node_modules/@lenso/agent-tool-sdk/dist/index.d.ts")],"@lenso/agent-tool-sdk/schema":[path.join(output,"node_modules/@lenso/agent-tool-sdk/dist/schema.d.ts")]}},include:["plugin.ts"]}));
}
process.stdout.write(JSON.stringify({schema:"lenso.convention-compiled.v1"}));
