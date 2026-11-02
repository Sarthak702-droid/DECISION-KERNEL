import {createServer} from 'node:http';
import {fromEnv,triage,consumeReceipt} from './client.ts';
import {KernelError} from '../../sdk/typescript/src/index.ts';
const client=fromEnv(),product=process.env.KERNEL_PRODUCT_ID??'support-suite';
// Local application adapter. The hosted product should use its normal session auth.
const appToken=process.env.SUPPORT_APP_TOKEN;
if(!appToken||appToken.length<32)throw new Error('Set SUPPORT_APP_TOKEN to at least 32 characters');
createServer(async(req,res)=>{
 res.setHeader('Content-Type','application/json');
 if(req.url!=='/tickets/triage'||req.method!=='POST'){res.writeHead(404).end('{}');return;}
 if(req.headers.authorization!==`Bearer ${appToken}`){res.writeHead(401).end('{}');return;}
 try {
  const chunks=[];let size=0;
  for await(const chunk of req){size+=chunk.length;if(size>16384){res.writeHead(413).end('{}');return;}chunks.push(chunk);}
  const body=JSON.parse(Buffer.concat(chunks).toString());
  if(typeof body.ticket_id!=='string'||!/^[a-zA-Z0-9_.-]{1,64}$/.test(body.ticket_id)||typeof body.message!=='string'||!body.message.length||body.message.length>8000||!Number.isSafeInteger(body.revision)||body.revision<1||!Number.isSafeInteger(body.observed_at_unix_ms)||body.observed_at_unix_ms<1||(body.egress_denied!==undefined&&typeof body.egress_denied!=='boolean')){res.writeHead(400).end('{}');return;}
  const receipt=await triage(client,product,body);
  res.end(JSON.stringify({receipt,application:consumeReceipt(receipt)}));
 }catch(error){res.writeHead(error instanceof KernelError?error.status:500).end(JSON.stringify({error:error instanceof KernelError?error.code:'APPLICATION_FAILED'}));}
}).listen(Number(process.env.SUPPORT_PORT??8090),'127.0.0.1',()=>console.log('Support integration listening on localhost'));
