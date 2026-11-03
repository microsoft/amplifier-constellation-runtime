// Optional MCP environment for native computer calls. Install Playwright in this
// folder and its Chromium browser before use. No human browser profile is reused.
import {createInterface} from 'node:readline';
import {chromium} from 'playwright';
const args=process.argv.slice(2);const option=n=>args.includes(n)?args[args.indexOf(n)+1]:undefined;
if(!args.includes('--url'))throw Error('Provide --url and optionally repeat --allow-origin');
const start=new URL(option('--url'));const origins=new Set([start.origin]);
for(let i=0;i<args.length;i++)if(args[i]==='--allow-origin')origins.add(new URL(args[++i]).origin);
for(const origin of origins){const u=new URL(origin);if(u.protocol!=='https:'&&!(u.protocol==='http:'&&['localhost','127.0.0.1','[::1]'].includes(u.hostname)))throw Error('Use HTTPS origins or loopback HTTP for local fixtures');}
const width=Number(option('--width')||1280),height=Number(option('--height')||720);
if(!Number.isInteger(width)||!Number.isInteger(height)||width<64||width>4096||height<64||height>4096)throw Error('Invalid viewport');
const browser=await chromium.launch({headless:true});
const context=await browser.newContext({viewport:{width,height},acceptDownloads:false,serviceWorkers:'block',permissions:[]});
// Playwright does not intercept every redirect hop. Fetch exactly one hop and
// refuse redirects before Chromium can contact their destination.
await context.route('**/*',async route=>{
 let response;
 try{
  if(!origins.has(new URL(route.request().url()).origin))return await route.abort();
  response=await route.fetch({maxRedirects:0,timeout:20000});
  if(response.status()>=300&&response.status()<400)return await route.abort();
  await route.fulfill({response});
 }catch{await route.abort().catch(()=>{});}
 finally{await response?.dispose();}
});
// WebSocket handshakes bypass HTTP routing. This bounded example does not need
// sockets: never connect a routed socket to its server (even on allowed origins).
await context.routeWebSocket('**/*',socket=>socket.close());
const page=await context.newPage();context.on('page',p=>{if(p!==page)p.close().catch(()=>{});});
page.on('dialog',dialog=>dialog.dismiss());await page.goto(start.href,{waitUntil:'domcontentloaded',timeout:20000});
const send=v=>process.stdout.write(JSON.stringify({jsonrpc:'2.0',...v})+'\n');
const keys={ENTER:'Enter',RETURN:'Enter',ESC:'Escape',ESCAPE:'Escape',TAB:'Tab',BACKSPACE:'Backspace',DELETE:'Delete',SPACE:' ',UP:'ArrowUp',DOWN:'ArrowDown',LEFT:'ArrowLeft',RIGHT:'ArrowRight',CTRL:'Control',CONTROL:'Control',CMD:'Meta',META:'Meta',ALT:'Alt',SHIFT:'Shift',HOME:'Home',END:'End',PAGEUP:'PageUp',PAGEDOWN:'PageDown'};
async function act(a){const modifiers=a.type==='keypress'?[]:(a.keys||[]).map(k=>keys[k.toUpperCase()]||k);for(const key of modifiers)await page.keyboard.down(key);try{switch(a.type){
 case 'click':if(!['left','right'].includes(a.button))throw Error('This browser supports left/right click only');await page.mouse.click(a.x,a.y,{button:a.button});break;
 case 'double_click':await page.mouse.dblclick(a.x,a.y);break;
 case 'move':await page.mouse.move(a.x,a.y);break;
 case 'scroll':await page.mouse.move(a.x,a.y);await page.mouse.wheel(a.scroll_x,a.scroll_y);break;
 case 'drag':await page.mouse.move(a.path[0].x,a.path[0].y);await page.mouse.down();try{for(const p of a.path.slice(1))await page.mouse.move(p.x,p.y);}finally{await page.mouse.up();}break;
 case 'keypress':await page.keyboard.press(a.keys.map(k=>keys[k.toUpperCase()]||k).join('+'));break;
 case 'type':await page.keyboard.insertText(a.text);break;
 case 'wait':await page.waitForTimeout(1000);break;
 case 'screenshot':break;
 default:throw Error('Unsupported browser action');
}}finally{for(const key of modifiers.reverse())await page.keyboard.up(key);}}
try{for await(const line of createInterface({input:process.stdin,crlfDelay:Infinity})){
 if(line.length>128*1024)throw Error('MCP request exceeds limit');const r=JSON.parse(line);if(r.id===undefined)continue;
 try{let result;
  if(r.method==='initialize')result={protocolVersion:'2025-06-18',capabilities:{tools:{}},serverInfo:{name:'bounded-browser',version:'1'}};
  else if(r.method==='tools/list')result={tools:[{name:'computer',description:'Execute an approved native computer batch in this isolated browser; return one PNG screenshot.',inputSchema:{type:'object',properties:{actions:{type:'array'},display_width:{type:'integer'},display_height:{type:'integer'}},required:['actions','display_width','display_height'],additionalProperties:false}}]};
  else if(r.method==='tools/call'){
   if(r.params.name!=='computer')throw Error('Unknown tool');const a=r.params.arguments;
   if(a.display_width!==width||a.display_height!==height||!Array.isArray(a.actions)||!a.actions.length||a.actions.length>32)throw Error('Invalid action batch');
   for(const action of a.actions)await act(action);await page.waitForTimeout(100);
   result={content:[{type:'image',mimeType:'image/png',data:(await page.screenshot({type:'png',timeout:10000})).toString('base64')}]};
  }else throw Error('Method not supported');send({id:r.id,result});
 }catch{send({id:r.id,error:{code:-32603,message:'Browser operation failed; outcome may be partial. Inspect before retrying.'}});}
}}finally{await context.close();await browser.close();}
