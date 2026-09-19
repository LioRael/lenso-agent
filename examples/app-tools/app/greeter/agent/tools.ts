import { tool, tools } from "@lenso/agent-tool-sdk";
import * as schema from "@lenso/agent-tool-sdk/schema";
export default tools([
  tool({name:"greet",description:"Greet a person.",input:schema.object({name:schema.string()}),output:schema.string()},({name})=>({ok:true,value:`Hello, ${name}!`})),
]);
