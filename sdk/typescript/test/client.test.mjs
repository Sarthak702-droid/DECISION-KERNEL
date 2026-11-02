import {test} from 'node:test';
import assert from 'node:assert/strict';
import {KernelClient,KernelError,evidenceDigest,canonicalJson} from '../src/index.ts';
test('headers, encoded keys, identity separation, empty success and no retry',async()=>{
 const calls=[];const client=new KernelClient({baseUrl:'http://127.0.0.1:8080',token:'service',adminToken:'admin',reviewerToken:'reviewer',fetch:async(url,init)=>{calls.push({url,...init});return new Response(null,{status:204});}});
 await client.connect({});await client.review('ticket/a',{disposition:'review',reason_code:'check'});
 assert.equal(calls[0].headers['X-Kernel-Admin-Token'],'admin');assert.equal(calls[0].headers['X-Kernel-Reviewer-Token'],undefined);
 assert.ok(calls[1].url.endsWith('ticket%2Fa/reviews'));assert.equal(calls[1].headers['X-Kernel-Reviewer-Token'],'reviewer');
 let count=0;client.transport=async()=>{count++;return Response.json({error:{code:'IDEMPOTENCY_CONFLICT',message:'conflict',retryable:false,request_id:'r1'}},{status:409});};
 await assert.rejects(client.decide({idempotency_key:'stable'}),e=>e instanceof KernelError&&e.code==='IDEMPOTENCY_CONFLICT'&&e.requestId==='r1');assert.equal(count,1);
});
test('canonical evidence and unsafe endpoint rejection',async()=>{
 assert.equal(canonicalJson({'𐀀':1,'':2}),'{"":2,"𐀀":1}');
 assert.throws(()=>canonicalJson(-0));
 assert.equal(await evidenceDigest({b:2,a:1}),await evidenceDigest({a:1,b:2}));
 assert.throws(()=>new KernelClient({baseUrl:'http://provider.example',token:'x'}));
 assert.throws(()=>new KernelClient({baseUrl:'https://user:secret@example.com',token:'x'}));
});
