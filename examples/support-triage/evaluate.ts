import {createHash} from 'node:crypto';
import {readFile,writeFile} from 'node:fs/promises';
import type {Receipt} from '../../sdk/typescript/src/index.ts';
import type {Ticket} from './client.ts';
import {fromEnv,triage} from './client.ts';
const [datasetPath,outputPath]=process.argv.slice(2);
if(!datasetPath||!outputPath)throw new Error('Usage: node evaluate.ts <heldout.json> <report.json>');
type Row = {ticket_id:string;group_id:string;partition:string;slice?:string;message:string;expected:{queue:string;urgent:boolean}};
const dataset:{provenance:string;rows:Row[]}=JSON.parse(await readFile(datasetPath,'utf8'));
if(!dataset.provenance||!Array.isArray(dataset.rows)||!dataset.rows.length)throw new Error('Dataset needs provenance and rows');
const seen=new Set(),groups=new Map();
for(const row of dataset.rows){
 if(typeof row.ticket_id!=='string'||!/^[a-zA-Z0-9_.-]{1,64}$/.test(row.ticket_id)||seen.has(row.ticket_id)||typeof row.group_id!=='string'||!row.group_id||!['dev','calibration','test'].includes(row.partition))throw new Error('Invalid row identity/partition');seen.add(row.ticket_id);
 if(groups.has(row.group_id)&&groups.get(row.group_id)!==row.partition)throw new Error('Group leakage across partitions');groups.set(row.group_id,row.partition);
 if(typeof row.message!=='string'||!['billing','technical','account','general'].includes(row.expected?.queue)||typeof row.expected?.urgent!=='boolean')throw new Error('Invalid ticket/label');
}
const rows=dataset.rows.filter(r=>r.partition==='test');if(!rows.length)throw new Error('No held-out test rows');
// A new run uses globally unique scope revisions; saved inputs enable exact replay.
const epoch=Number(process.env.PILOT_EPOCH_MS??Date.now());
if(!Number.isSafeInteger(epoch)||epoch<1||epoch+rows.length>Number.MAX_SAFE_INTEGER)throw new Error('Invalid pilot epoch');
type Success={ticket_id:string;input:Ticket;expected:Row['expected'];receipt:Receipt;queue_correct:boolean;urgent_correct:boolean};
type Failure={ticket_id:string;input:Ticket;error:string};
const client=fromEnv(),results:(Success|Failure)[]=[];
const usageBefore=await client.usage();
for(const [i,row]of rows.entries()){
 const input={ticket_id:row.ticket_id,message:row.message,revision:epoch+i,observed_at_unix_ms:epoch};
 try{
  const receipt=await triage(client,process.env.KERNEL_PRODUCT_ID??'support-suite',input);
  const q=receipt.answers.queue,u=receipt.answers.urgent;
  results.push({ticket_id:row.ticket_id,input,expected:row.expected,receipt,queue_correct:q?.kind==='categorical'&&q.selected_id===row.expected.queue,urgent_correct:u?.kind==='binary'&&u.value===row.expected.urgent});
 }catch(e){results.push({ticket_id:row.ticket_id,input,error:e instanceof Error?e.message:String(e)});}
}
const successes=results.filter((r):r is Success=>'receipt' in r),correct=successes.filter(r=>r.queue_correct&&r.urgent_correct).length;
const costs=successes.map(r=>r.receipt.cost_nano_usd),known=costs.filter(c=>c!==null);
const reportedCosts=successes.map(r=>r.receipt.provider_reported_cost_nano_usd).filter((c):c is number=>c!==null);
const usageAfter=await client.usage();
const urgentRows=rows.filter(r=>r.expected.urgent);
const urgentHit=successes.filter(r=>r.expected.urgent&&r.urgent_correct).length;
const slices=Object.fromEntries([...new Set(rows.map(r=>r.slice??'unspecified'))].map(slice=>{const selected=rows.filter(r=>(r.slice??'unspecified')===slice),ids=new Set(selected.map(r=>r.ticket_id)),done=successes.filter(r=>ids.has(r.ticket_id));return [slice,{total:selected.length,completed:done.length,joint_correct:done.filter(r=>r.queue_correct&&r.urgent_correct).length,joint_accuracy_all:done.filter(r=>r.queue_correct&&r.urgent_correct).length/selected.length,review_count:done.filter(r=>r.receipt.disposition==='review').length}];}));
const confusion=Object.fromEntries(['billing','technical','account','general'].map(queue=>[queue,Object.fromEntries(['billing','technical','account','general','unresolved'].map(predicted=>[predicted,rows.filter(row=>row.expected.queue===queue && (()=>{const result=successes.find(r=>r.ticket_id===row.ticket_id);const answer=result?.receipt.answers.queue;return (answer?.kind==='categorical'?answer.selected_id:'unresolved')===predicted;})()).length]))]));
const summary={urgent_test_count:urgentRows.length,urgent_missed_or_unresolved:urgentRows.length-urgentHit,urgent_recall_all:urgentRows.length?urgentHit/urgentRows.length:null,provider_reported_cost_nano_usd:reportedCosts.reduce((a,b)=>a+b,0),provider_reported_unknown_count:successes.length-reportedCosts.length,total:rows.length,completed:successes.length,failed:rows.length-successes.length,joint_accuracy_all:correct/rows.length,queue_accuracy_all:successes.filter(r=>r.queue_correct).length/rows.length,urgent_accuracy_all:successes.filter(r=>r.urgent_correct).length/rows.length,review_count:successes.filter(r=>r.receipt.disposition==='review').length,review_rate_completed:successes.length?successes.filter(r=>r.receipt.disposition==='review').length/successes.length:null,non_accept_count:successes.filter(r=>r.receipt.disposition!=='accept').length,known_cost_nano_usd:known.reduce((a,b)=>a+b,0),unknown_cost_count:costs.length-known.length,average_known_cost_nano_usd:known.length?known.reduce((a,b)=>a+b,0)/known.length:null,attempts:successes.reduce((a,r)=>a+r.receipt.attempts,0)};
await writeFile(outputPath,JSON.stringify({dataset:datasetPath,dataset_sha256:createHash('sha256').update(await readFile(datasetPath)).digest('hex'),provenance:dataset.provenance,epoch,summary,slices,queue_confusion:confusion,usage_before:usageBefore,usage_after:usageAfter,results},null,2)+'\n');console.log(JSON.stringify(summary,null,2));
if(summary.failed)process.exitCode=1;
