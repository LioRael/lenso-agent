import {test,expect} from "bun:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
const compiler=path.resolve(import.meta.dir,"../compiler.mjs");
// Authored modules must not run during compile/discovery. Ambiguous languages
// must fail rather than selecting a different implementation implicitly.
test("tool lowering is static and rejects an ambiguous language",()=>{
 const root=fs.mkdtempSync(path.join(os.tmpdir(),"tool-convention-"));
 try {
 const entry=path.join(root,"agent"),output=path.join(root,"out");fs.mkdirSync(entry);fs.mkdirSync(output);
 fs.writeFileSync(path.join(entry,"tools.ts"),'throw new Error("must not execute while compiling"); export default tools([]);');
 const request={schema:"lenso.convention-compile.v1",entry,output,plugin_id:"example.tools",release_version:"1.0.0"};
 const run=()=>Bun.spawnSync(["bun",compiler],{stdin:Buffer.from(JSON.stringify(request))});
 expect(run().exitCode).toBe(0);
 fs.writeFileSync(path.join(entry,"tools.rs"),"invalid Rust must not be silently selected");
 expect(run().exitCode).not.toBe(0);
 } finally {fs.rmSync(root,{recursive:true,force:true});}
});

test("profile and Markdown instructions become a deployment resource without running tools",()=>{
 const root=fs.mkdtempSync(path.join(os.tmpdir(),"agent-deployment-"));
 try {
  const entry=path.join(root,"agent"),output=path.join(root,"out");fs.mkdirSync(entry);fs.mkdirSync(output);
  fs.writeFileSync(path.join(entry,"tools.ts"),'throw new Error("must not execute while compiling"); export default tools([]);');
  fs.writeFileSync(path.join(entry,"profile.toml"),'instances = []\nallowed_tools = ["greet"]\n\n[lenso]\nprofile = "orders"\n');
  fs.writeFileSync(path.join(entry,"instructions.md"),"Follow the order rules.\n");
  const request={schema:"lenso.convention-compile.v1",entry,output,plugin_id:"example.orders",release_version:"1.0.0"};
  const run=()=>Bun.spawnSync(["bun",compiler],{stdin:Buffer.from(JSON.stringify(request))});
  expect(run().exitCode).toBe(0);
  expect(JSON.parse(fs.readFileSync(path.join(output,"lenso-agent-deployment.json"),"utf8"))).toEqual({
   schema:"lenso.agent.deployment@1",plugin:{id:"example.orders",instance:"default"},
   profile:{name:"orders",source_toml:'instances = []\nallowed_tools = ["greet"]\n\n[lenso]\nprofile = "orders"\n',instructions:"Follow the order rules.\n"}
  });
  fs.unlinkSync(path.join(entry,"profile.toml"));
  expect(run().exitCode).not.toBe(0);
 } finally {fs.rmSync(root,{recursive:true,force:true});}
});

test("Rust tools lower to one portable Wasm and Process Plugin",()=>{
 const root=fs.mkdtempSync(path.join(os.tmpdir(),"rust-tool-convention-"));
 try {
  const entry=path.join(root,"agent"),output=path.join(root,"out");fs.mkdirSync(entry);fs.mkdirSync(output);
  const source=path.join(entry,"tools.rs");
  fs.writeFileSync(source,"// The compiler must not need to parse or execute this source.\n");
  const request={schema:"lenso.convention-compile.v1",entry,output,plugin_id:"example.rust-tools",release_version:"1.0.0"};
  const run=()=>Bun.spawnSync(["bun",compiler],{stdin:Buffer.from(JSON.stringify(request))});
  expect(run().exitCode).toBe(0);
  const manifest=fs.readFileSync(path.join(output,"Cargo.toml"),"utf8");
  expect(manifest).toContain('lenso = { package = "lenso-plugin-sdk", version = "0.4.5" }');
  expect(manifest).toContain('outputs=["wasm","process"]');
  expect(manifest).toContain('crate-type=["cdylib"]');
  expect(fs.readFileSync(path.join(output,"src/lib.rs"),"utf8")).toBe(`include!(${JSON.stringify(source)});\n`);
  expect(fs.readFileSync(path.join(output,"src/main.rs"),"utf8")).toContain('include!("lib.rs")');
  fs.writeFileSync(path.join(entry,"Cargo.toml"),'[dependencies]\nlenso = "0.5.23"\n');
  expect(run().exitCode).not.toBe(0);
 } finally {fs.rmSync(root,{recursive:true,force:true});}
});
