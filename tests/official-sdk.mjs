// Usage: node official-sdk.mjs ABSOLUTE_BINARY HOME WORKSPACE EXISTING_SESSION
// Uses a previously exercised saved session; no paid model calls.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {Readable,Writable} from 'node:stream';
import * as acp from '@agentclientprotocol/sdk';
const [binary,home,cwd,id]=process.argv.slice(2);
const child=spawn(binary,['--home',home,'acp'],{stdio:['pipe','pipe','inherit']});
const timeout=setTimeout(()=>child.kill('SIGKILL'),30000);
let updates=[];
try {
 const report=await acp.client({name:'native-runtime-contract-check'})
   .onRequest(acp.methods.client.session.requestPermission,()=>({outcome:{outcome:'cancelled'}}))
   .onNotification(acp.methods.client.session.update,({params})=>updates.push(params.update))
   .connectWith(acp.ndJsonStream(Writable.toWeb(child.stdin),Readable.toWeb(child.stdout)),async ctx=>{
     const init=await ctx.request(acp.methods.agent.initialize,{protocolVersion:acp.PROTOCOL_VERSION,clientInfo:{name:'official-sdk',version:'2.2.0'},clientCapabilities:{}});
     assert.equal(init.agentInfo.name,'agent-runtime');assert.equal(init.agentCapabilities.loadSession,true);
     assert.equal(init.agentCapabilities.mcpCapabilities.http,true);
     const listing=await ctx.request(acp.methods.agent.session.list,{cwd});assert.ok(listing.sessions.some(s=>s.sessionId===id));
     await ctx.request(acp.methods.agent.session.load,{sessionId:id,cwd,mcpServers:[]});
     assert.ok(updates.some(v=>v.sessionUpdate==='user_message_chunk'));
     assert.ok(updates.some(v=>v.sessionUpdate==='agent_message_chunk'));
     assert.ok(updates.some(v=>v.sessionUpdate==='tool_call'));
     assert.ok(updates.some(v=>v.sessionUpdate==='tool_call_update'));
     await ctx.request(acp.methods.agent.session.setMode,{sessionId:id,modeId:'read_only'});
     return {protocol:init.protocolVersion,session:id,updates:updates.length,history_replayed:true,modes:true};
   });
 console.log(JSON.stringify({sdk:'@agentclientprotocol/sdk@2.2.0',success:true,...report}));
} finally {clearTimeout(timeout);child.kill();}
