const el=id=>document.getElementById(id);
async function api(method,path,body,admin=false,reviewer=false){
 const headers={'Authorization':'Bearer '+el('token').value,'Content-Type':'application/json'};
 if(admin)headers['X-Kernel-Admin-Token']=el('admin').value;
 if(reviewer)headers['X-Kernel-Reviewer-Token']=el('reviewer').value;
 if(path==='/v1/decisions'&&body)headers['Idempotency-Key']=body.idempotency_key;
 const r=await fetch(path,{method,headers,body:body===undefined?undefined:JSON.stringify(body),redirect:'error'});
 const text=await r.text();const data=text?JSON.parse(text):{status:r.status};
 if(!r.ok)throw new Error(JSON.stringify(data));return data;
}
const read=id=>JSON.parse(el(id).value),key=()=>encodeURIComponent(el('receiptKey').value);
const actions={connections:()=>api('GET','/v1/provider-connections',undefined,true),connect:async()=>{const b=read('connection');try{return await api('POST','/v1/provider-connections',b,true);}finally{el('connection').value='';}},probe:()=>api('POST',`/v1/provider-connections/${encodeURIComponent(el('connectionId').value)}/test`,{idempotency_key:crypto.randomUUID(),tenant_level:false},true),bind:()=>api('PUT',`/v1/provider-bindings/${encodeURIComponent(el('decision').value)}`,{connection_id:el('connectionId').value,tenant_level:false},true),packs:()=>api('GET','/v1/packs'),usage:()=>api('GET','/v1/usage'),run:()=>api('POST','/v1/decisions',read('request')),receipt:()=>api('GET','/v1/decisions/'+key()),history:async()=>({reviews:await api('GET',`/v1/decisions/${key()}/reviews`),outcomes:await api('GET',`/v1/decisions/${key()}/outcomes`)}),review:()=>api('POST',`/v1/decisions/${key()}/reviews`,read('feedback'),false,true),outcome:()=>{const b=read('feedback');return api('POST',`/v1/decisions/${key()}/outcomes`,b,false,b.status!=='pending');}};
for(const [id,action]of Object.entries(actions))el(id).onclick=async()=>{el(id).disabled=true;el('status').textContent='Working…';try{el('result').textContent=JSON.stringify(await action(),null,2);el('status').textContent='Completed';}catch(e){el('status').textContent='Request failed';el('result').textContent=e.message;}finally{el(id).disabled=false;}};
el('clear').onclick=()=>{for(const id of ['token','admin','reviewer','connection'])el(id).value='';el('result').textContent='Session cleared';};
