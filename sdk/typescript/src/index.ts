export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type Disposition = 'accept' | 'review' | 'deny' | 'abstain';
export type Answer =
  | {kind:'categorical'; selected_id:string; probabilities?:Record<string,number>|null; probability_provenance?:Json}
  | {kind:'binary'; proposition:string; value:boolean|null; probability_true?:number|null; probability_provenance?:Json}
  | {kind:'ordinal'; scale_id:string; selected_anchor:string; distribution?:number[]|null; probability_provenance?:Json}
  | {kind:'not_evaluated'; reason:string};
export interface DecisionRequest {
  schema_version:'1.1'; product_id:string; decision_type:string; release:string;
  idempotency_key:string; mode:'evaluate'; execution_profile:'fast'|'deferred';
  state:Record<string,Json>; evidence_refs:{id:string;revision:number}[];
  constraints:{deadline_ms?:number;max_billable_attempts?:number;max_cost_nano_usd?:number};
}
export interface Receipt {
  receipt_id:string; scope:{tenant_id:string;project_id:string;environment:string;product_id:string};
  release:string; release_digest_sha256:string; request_digest_sha256:string;
  answers:Record<string,Answer>; disposition:Disposition; reason_codes:string[];
  evidence_revisions:Record<string,number>; provider_id:string|null; model_id:string|null; returned_model_id:string|null;
  attempts:number; prediction_cache_hit:boolean; cost_nano_usd:number|null;
  provider_reported_cost_nano_usd:number|null; provider_usage?:Json;
  created_at_unix_ms:number; kernel_compute_ms:number; db_claim_ms:number;
  provider_latency_ms:number|null; db_commit_ms:number|null; total_latency_ms:number|null;
}
export interface ConnectionWrite {
  connection_id:string;tenant_level?:boolean;expected_revision?:number;api_key:string;
  config:{adapter:'compatible'|'system-one';endpoint:string;provider_id:string;model_id:string;
    allowed_returned_models?:string[];max_output_tokens:number;max_dispatch_nano_usd:number};
}
export interface Review { disposition:Disposition;reason_code:string;notes?:string;corrections?:Record<string,Answer> }
export interface Outcome {outcome:string;status:'pending'|'verified'|'disputed';verification_ref?:string}
export interface ConnectionMetadata {connection_id:string;project_id:string;config:ConnectionWrite['config'];revision:number;deleted:boolean}
export interface ConnectionProbe {connected:boolean;capabilities:Json;requested_model:string|null;served_model:string|null;receipt_id:string;idempotency_key:string;reason_codes:string[]}
export interface Pack {decision_type:string;version:string;digest:string;revoked:boolean;active:boolean}
export interface Usage {purpose:string;cap_nano_usd:number;spent_nano_usd:number;held_nano_usd:number;attempts_dispatch_intent:number;attempts_uncertain:number;attempts_reconciled:number}
export interface ReviewRecord extends Omit<Review,'reason_code'|'notes'> {reviewer_id:string;created_at:string;reason_code:string|null;notes:string|null}
export interface OutcomeRecord extends Omit<Outcome,'verification_ref'> {reviewer_id:string|null;created_at:string;verification_ref:string|null}
export class KernelError extends Error {
  status:number; code:string; retryable:boolean; requestId?:string;
  constructor(status:number, error:{code?:string;message?:string;retryable?:boolean;request_id?:string}) {
    super(error.message ?? `Kernel HTTP ${status}`); this.name='KernelError';this.status=status;
    this.code=error.code??'INVALID_RESPONSE';this.retryable=error.retryable??false;this.requestId=error.request_id;
  }
}
export class KernelClient {
  baseUrl:string; token:string; adminToken?:string; reviewerToken?:string; transport:typeof fetch;
  constructor(options:{baseUrl:string;token:string;adminToken?:string;reviewerToken?:string;fetch?:typeof fetch}) {
    const url=new URL(options.baseUrl);
    if(url.username||url.password||url.search||url.hash || (url.protocol!=='https:' && !(url.protocol==='http:' && ['localhost','127.0.0.1','[::1]'].includes(url.hostname)))) throw new Error('Use HTTPS or local HTTP without URL credentials');
    this.baseUrl=options.baseUrl.replace(/\/$/,'');this.token=options.token;this.adminToken=options.adminToken;this.reviewerToken=options.reviewerToken;this.transport=options.fetch??fetch;
  }
  // No automatic retries: callers retain the same key/body after uncertain failures.
  async request<T=Json>(method:string,path:string,body?:unknown,options:{admin?:boolean;reviewer?:boolean;signal?:AbortSignal;key?:string}={}):Promise<T> {
    if(!path.startsWith('/v1/')) throw new Error('Expected /v1/ API path');
    const headers:Record<string,string>={'Authorization':`Bearer ${this.token}`,'Content-Type':'application/json'};
    if(options.key) headers['Idempotency-Key']=options.key;
    if(options.admin) {if(!this.adminToken) throw new Error('Admin token required');headers['X-Kernel-Admin-Token']=this.adminToken;}
    if(options.reviewer) {if(!this.reviewerToken) throw new Error('Reviewer token required');headers['X-Kernel-Reviewer-Token']=this.reviewerToken;}
    const response=await this.transport(this.baseUrl+path,{method,headers,body:body===undefined?undefined:JSON.stringify(body),signal:options.signal,redirect:'error'});
    if(!response.ok) {let data;try{data=await response.json();}catch{data={};}throw new KernelError(response.status,data.error??{});}
    if(response.status===204||response.status===201) return undefined as T;
    const text=await response.text();return (text?JSON.parse(text):undefined) as T;
  }
  decide(body:DecisionRequest,signal?:AbortSignal) {return this.request<Receipt>('POST','/v1/decisions',body,{key:body.idempotency_key,signal});}
  receipt(key:string) {return this.request<Receipt>('GET',`/v1/decisions/${encodeURIComponent(key)}`);}
  evidence(body:{evidence_id:string;revision:number;observed_at_unix_ms:number;expires_at_unix_ms?:number;digest_sha256:string;sensitivity:'public'|'internal'|'confidential'|'restricted';value:Json}) {return this.request<void>('POST','/v1/evidence',body);}
  connections() {return this.request<ConnectionMetadata[]>('GET','/v1/provider-connections',undefined,{admin:true});}
  connect(body:ConnectionWrite) {return this.request<void>('POST','/v1/provider-connections',body,{admin:true});}
  testConnection(id:string,key:string,tenant_level=false) {return this.request<ConnectionProbe>('POST',`/v1/provider-connections/${encodeURIComponent(id)}/test`,{idempotency_key:key,tenant_level},{admin:true});}
  deleteConnection(id:string,expected_revision:number,tenant_level=false) {return this.request<void>('DELETE',`/v1/provider-connections/${encodeURIComponent(id)}`,{expected_revision,tenant_level},{admin:true});}
  unbind(decision:string) {return this.request<void>('DELETE',`/v1/provider-bindings/${encodeURIComponent(decision)}`,undefined,{admin:true});}
  bind(decision:string,connection_id:string,tenant_level=false) {return this.request<void>('PUT',`/v1/provider-bindings/${encodeURIComponent(decision)}`,{connection_id,tenant_level},{admin:true});}
  packs() {return this.request<Pack[]>('GET','/v1/packs');}
  usage() {return this.request<Usage[]>('GET','/v1/usage');}
  review(key:string,body:Review) {return this.request<void>('POST',`/v1/decisions/${encodeURIComponent(key)}/reviews`,body,{reviewer:true});}
  outcome(key:string,body:Outcome) {return this.request<void>('POST',`/v1/decisions/${encodeURIComponent(key)}/outcomes`,body,{reviewer:body.status!=='pending'});}
  reviews(key:string) {return this.request<ReviewRecord[]>('GET',`/v1/decisions/${encodeURIComponent(key)}/reviews`);}
  outcomes(key:string) {return this.request<OutcomeRecord[]>('GET',`/v1/decisions/${encodeURIComponent(key)}/outcomes`);}
  enqueue(body:DecisionRequest) {return this.request<{job_id:string;status:string}>('POST','/v1/decision-jobs',body,{key:body.idempotency_key});}
  job(id:string) {return this.request<{job_id:string;status:string;receipt:Receipt|null;attempt_count:number;last_error:string|null}>('GET',`/v1/decision-jobs/${encodeURIComponent(id)}`);}
}
// Matches serde_json's sorted object keys. Use safe integer JSON numbers for evidence.
export function canonicalJson(value:Json):string {
  if(Array.isArray(value)) return '['+value.map(canonicalJson).join(',')+']';
  if(value!==null && typeof value==='object') return '{'+Object.keys(value).sort((a,b)=>{const x=Array.from(a,c=>c.codePointAt(0)!),y=Array.from(b,c=>c.codePointAt(0)!);for(let i=0;i<Math.min(x.length,y.length);i++)if(x[i]!==y[i])return x[i]-y[i];return x.length-y.length;}).map(k=>JSON.stringify(k)+':'+canonicalJson(value[k])).join(',')+'}';
  if(typeof value==='number' && (!Number.isSafeInteger(value)||Object.is(value,-0))) throw new Error('Evidence numbers must be safe integers');
  return JSON.stringify(value);
}
export async function evidenceDigest(value:Json):Promise<string> {
  const bytes=await crypto.subtle.digest('SHA-256',new TextEncoder().encode(canonicalJson(value)));
  return Array.from(new Uint8Array(bytes),b=>b.toString(16).padStart(2,'0')).join('');
}
