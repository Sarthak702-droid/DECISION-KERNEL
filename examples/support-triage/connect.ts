import {fromEnv} from './client.ts';
const client=fromEnv();
const get=(name:string)=>{const v=process.env[name];if(!v)throw new Error(`Missing ${name}`);return v;};
await client.connect({connection_id:process.env.CONNECTION_ID??'support-primary',api_key:get('PROVIDER_API_KEY'),config:{adapter:'compatible',endpoint:get('PROVIDER_BASE_URL'),provider_id:'support-byok',model_id:get('PROVIDER_MODEL'),allowed_returned_models:(process.env.PROVIDER_RETURNED_MODELS??'').split(',').filter(Boolean),max_output_tokens:512,max_dispatch_nano_usd:Number(process.env.PROVIDER_MAX_DISPATCH_NANO_USD??100000)}});
const probe=await client.testConnection(process.env.CONNECTION_ID??'support-primary','onboarding-'+crypto.randomUUID());
if(!probe||typeof probe!=='object'||Array.isArray(probe)||!probe.connected)throw new Error('Provider probe failed; inspect connection-test receipt and usage');
await client.bind('support.triage',process.env.CONNECTION_ID??'support-primary');
console.log('Provider connected, probed and bound to support.triage');
