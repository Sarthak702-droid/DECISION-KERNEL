import {KernelClient,evidenceDigest} from '../../sdk/typescript/src/index.ts';
import type {Receipt} from '../../sdk/typescript/src/index.ts';
export interface Ticket {ticket_id:string;message:string;revision:number;observed_at_unix_ms:number;egress_denied?:boolean}
export function fromEnv() {
  const required=(name:string)=>{const value=process.env[name];if(!value)throw new Error(`Missing ${name}`);return value;};
  return new KernelClient({baseUrl:required('KERNEL_URL'),token:required('KERNEL_TOKEN'),adminToken:process.env.KERNEL_ADMIN_TOKEN,reviewerToken:process.env.KERNEL_REVIEWER_TOKEN});
}
export async function triage(client:KernelClient,product_id:string,ticket:Ticket):Promise<Receipt> {
  const value={body:ticket.message};
  await client.evidence({evidence_id:'latest_message',revision:ticket.revision,observed_at_unix_ms:ticket.observed_at_unix_ms,digest_sha256:await evidenceDigest(value),sensitivity:'internal',value});
  return client.decide({schema_version:'1.1',product_id,decision_type:'support.triage',release:'active',idempotency_key:`ticket-${ticket.ticket_id}-v${ticket.revision}`,mode:'evaluate',execution_profile:'fast',state:{egress_denied:ticket.egress_denied??false},evidence_refs:[{id:'latest_message',revision:ticket.revision}],constraints:{deadline_ms:1000,max_billable_attempts:1,max_cost_nano_usd:1000000}});
}
// This adapter is where a real ticket application consumes the immutable receipt.
export function consumeReceipt(receipt:Receipt) {
  if(receipt.disposition==='deny')return {action:'blocked',receipt_id:receipt.receipt_id};
  if(receipt.disposition!=='accept')return {action:'human_review',receipt_id:receipt.receipt_id,reasons:receipt.reason_codes};
  const queue=receipt.answers.queue,urgent=receipt.answers.urgent;
  if(queue?.kind!=='categorical'||!['billing','technical','account','general'].includes(queue.selected_id)||urgent?.kind!=='binary'||typeof urgent.value!=='boolean')throw new Error('Accepted receipt has unexpected output contract');
  return {action:'route_ticket',queue:queue.selected_id,urgent:urgent.value,receipt_id:receipt.receipt_id};
}
