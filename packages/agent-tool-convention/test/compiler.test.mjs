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
