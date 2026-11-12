// SPDX-License-Identifier: AGPL-3.0-or-later
// Execute the actual source parser for deterministic, non-date wire fixtures.
// Node's built-in TypeScript stripper avoids an installed web-client toolchain.
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import {stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const sourcePath = 'fluxer_app/src/features/search/utils/SearchQueryParser.ts';
const parser = stripTypeScriptTypes(fs.readFileSync(path.join(root, sourcePath), 'utf8'))
  .replace(/^import[\s\S]*?;\n/gm, '').replace(/^export /gm, '');
const utils = stripTypeScriptTypes(fs.readFileSync(path.join(root, 'fluxer_app/src/features/search/utils/SearchUtils.ts'), 'utf8'));
const functionStart = utils.indexOf('function toApiParams(');
let functionEnd = utils.indexOf('{', functionStart), depth = 1;
while (depth && ++functionEnd < utils.length) {
  if (utils[functionEnd] === '{') depth++;
  else if (utils[functionEnd] === '}') depth--;
}
const users = [
  {id:'100',username:'alice',discriminator:'0001'},
  {id:'101',username:'Bob',discriminator:''},
];
const channels = [
  {id:'200',name:'general',guildId:'10'},
  {id:'201',name:'general-chat',guildId:'10'},
  {id:'202',name:'photos',guildId:'10'},
  {id:'203',name:'other',guildId:'11'},
  {id:'204',name:'',guildId:null,isDM:()=>true,getRecipientId:()=> '101'},
];
const runtime = {usesUniqueUsernames:false};
const context = vm.createContext({
  RuntimeConfig:runtime,
  Users:{getCurrentUser:()=>users[0],getUser:id=>users.find(user=>user.id===id),
    getUserByTag:tag=>users.find(user=>`${user.username}#${user.discriminator}`===tag || (runtime.usesUniqueUsernames&&user.username.toLowerCase()===tag.toLowerCase()))},
  Channels:{getGuildChannels:id=>channels.filter(channel=>channel.guildId===id),getPrivateChannels:()=>channels.filter(channel=>channel.guildId===null)},
  isProbablyAValidSnowflake:value=>{try{return BigInt(value)>0n;}catch{return false;}},
  fromTimestamp:ms=>(BigInt(Math.max(0,ms-1420070400000))<<22n).toString(),
  DateTime:{},fuzzySearch:()=>false,
  MS_PER_DAY:86400000,MS_PER_HOUR:3600000,MS_PER_MINUTE:60000,MS_PER_SECOND:1000,
});
for (const user of users) Object.defineProperty(user,'tag',{get:()=>runtime.usesUniqueUsernames?user.username:`${user.username}#${user.discriminator}`});
vm.runInContext(`${parser}\n${utils.slice(functionStart,functionEnd+1)}\nglobalThis.parseFixture=(query,hints,ctx)=>{const body=toApiParams(parseQuery(query,hints,ctx));delete body.hits_per_page;delete body.page;return body;};`,context);
const queries = [
  '', '  ', 'ordinary words', '  whitespace\t and\ntext  ', '\u0085word\ufeff',
  '"exact phrase" loose words', '«curly phrase» “two words”', '"unterminated phrase',
  '"escaped \\"quote\\""', 'unknown:value tail', 'begin:general tail',
  'from:100 from:100,101 -from:101', 'from:@me mentions:here -mentions:everyone',
  'from:alice#0001 from:ALICE#0001 from:Bob', 'from:+100 from:0x64 from:+0x64 from:000100',
  'from:0 from:-100 from:not-a-user', 'from: arbitrary from:100', 'from: has:image text',
  'in:general in:photos -in:general-chat', 'in:other in:unknown', 'in:general-ch',
  'has:image,file,forward -has:link,video,forward', 'has:IMAGE,unknown -author-type:bot author-type:USER,webhook',
  'has:"image,file" pinned:"true"', 'pinned:yes pinned:no mature:true mature:false',
  'pinned:0 pinned:1 -pinned:false mature:yes', 'sort:relevance order:asc sort:timestamp order:desc',
  'sort:invalid order:random', 'scope:unknown,all_dms scope:all_guilds',
  'scope:current scope:"all_dms"', 'embed-type:IMAGE,article -embed-type:sound',
  'embed-provider:YouTube,Twitch -embed-provider:"Other Provider"',
  'link:example.com -link:spam.example filename:"my photo.png" -filename:secret',
  'ext:.png,.WEBP -ext:txt any:"first phrase",second any:second,third',
  'beforeid:9007199254740993123 afterid:9007199254740993000',
  'beforeid:abc afterid:invalid', '-any:one,two -scope:all',
  'from:100 in:general "quoted phrase" loose has:forward pinned:false sort:relevance',
  'any:"escaped\\,comma",tail filename:"a\\"b.png"',
];
const fixtures=[];
for (const unique of [false,true]) for (const query of queries) {
  runtime.usesUniqueUsernames=unique;
  const hints={usersByTag:{'Chosen Person':'101'},channelsByName:{'chosen-channel':'202'}};
  const params=context.parseFixture(query,hints,{guildId:'10'});
  fixtures.push({query,unique,params});
}
const out=path.join(root,'fluxer_native/crates/core/tests/fixtures/search_query.json');
fs.mkdirSync(path.dirname(out),{recursive:true});
fs.writeFileSync(out,JSON.stringify({source:sourcePath,fixtures},null,2)+'\n');
console.log(`Wrote ${fixtures.length} source-executed query fixtures.`);
