// Exercise the actual example Chromium driver against two loopback origins.
import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import {spawn} from 'node:child_process';
import {createInterface} from 'node:readline';
import {once} from 'node:events';
const listen=async server=>{server.listen(0,'127.0.0.1');await once(server,'listening');return `http://127.0.0.1:${server.address().port}`};
test('computer browser refuses redirects and sockets before contacting other origins', {timeout:60000}, async()=>{
 const hits=[];const allowed=[];
 const target=http.createServer((req,res)=>{hits.push(req.url);res.end('outside')});
 target.on('upgrade',(req,socket)=>{hits.push(req.url);socket.destroy()});
 const outside=await listen(target);
 const start=http.createServer((req,res)=>{
  allowed.push(req.url);
  if(req.url==='/redirect'){res.writeHead(302,{Location:outside+'/redirected'});res.end();return}
  if(req.url==='/resource'){res.end('allowed');return}
  res.setHeader('Content-Type','text/html');
  res.end(`<script>fetch('/resource');fetch('${outside}/fetch').catch(()=>{});new WebSocket('${outside.replace('http:','ws:')}/socket');fetch('/redirect').catch(()=>{});</script>`);
 });
 const origin=await listen(start);
 let child;
 try{
  child=spawn(process.execPath,['examples/computer/browser.mjs','--url',origin,'--width','64','--height','64'],{stdio:['pipe','pipe','pipe']});
  let error='';child.stderr.on('data',s=>error+=s);
  const lines=createInterface({input:child.stdout});
  const reply=new Promise((resolve,reject)=>{lines.once('line',s=>resolve(JSON.parse(s)));child.once('exit',code=>reject(Error(`Driver exited ${code}: ${error}`)))});
  child.stdin.write(JSON.stringify({jsonrpc:'2.0',id:1,method:'tools/call',params:{name:'computer',arguments:{actions:[{type:'wait'},{type:'screenshot'}],display_width:64,display_height:64}}})+'\n');
  const result=await reply;assert.equal(result.result.content[0].mimeType,'image/png');
  assert.ok(allowed.includes('/resource'),'ordinary allowed-origin requests still work');
  assert.ok(allowed.includes('/redirect'),'redirect fixture exercised');
  assert.deepEqual(hits,[],'neither redirect, WebSocket, nor fetch may contact the disallowed server');
  const exited=once(child,'exit');child.stdin.end();assert.equal((await exited)[0],0,error);
 }finally{
  if(child&&child.exitCode===null)child.kill('SIGKILL');
  for(const server of [start,target]){server.closeAllConnections();await new Promise(resolve=>server.close(resolve))}
 }
});
