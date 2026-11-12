// SPDX-License-Identifier: AGPL-3.0-or-later
// Execute the actual source parser for deterministic, non-date wire fixtures.
// Node's built-in TypeScript stripper avoids an installed web-client toolchain.
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import {spawnSync} from 'node:child_process';
import {stripTypeScriptTypes,createRequire} from 'node:module';
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

// Dates deliberately execute the pinned Luxon implementation rather than a
// mock of its parsing/calendar behavior. Supply its verified CJS artifact.
if (process.argv.includes('--dates')) {
  const index=process.argv.indexOf('--dates');
  const packagePath=process.argv[index+1] || process.env.FLUXER_LUXON_FIXTURE_MODULE;
  if (!packagePath) throw new Error('--dates requires a pinned Luxon CJS path');
  const {DateTime,Settings,VERSION}=createRequire(import.meta.url)(path.resolve(packagePath));
  if (VERSION!=='3.7.2') throw new Error(`Expected pinned Luxon3.7.2, got ${VERSION}`);
  context.DateTime=DateTime;
  const dateQueries=[
    'before:today','after:today','during:today','during:yesterday','during:now',
    'during:week','during:month','during:year','during:monday','during:sunday',
    'during:january','during:november','during:2010','during:2014','during:2015',
    'during:2026','during:2027','during:2030','during:9999','during:0001',
    'during:2026-11','during:202611','during:2026-11-12','during:20261112',
    'during:2026-316','during:2026316','during:2026-W46-4','during:2026W464',
    'during:2026-W46','during:2026W46','during:2026-11-12T14','during:2026-11-12T14Z',
    'during:2026-11-12T1430','during:2026-11-12T1430Z','during:20261112T1430Z',
    'during:2026-11-12T14:30','during:2026-11-12T14:30Z','during:2026-11-12t14:30z',
    'during:2026-11-12_14:30','during:2026-11-12T14+02','during:2026-11-12T14+02:00',
    'during:2026-11-12T14:30+02','during:2026-11-12T14:30+0200','during:2026-11-12T14:30+02:00',
    'during:2026-11-12T14:30:12.123456','during:2026-11-12T14:30:12,123',
    'during:2026-11-12T24:00','during:2026-11-12T24:00:01','during:14:30',
    'during:2026-11-12T14:30[America/Chicago]', 'during:2026-11-12T14:30[Europe/Berlin]',
    'during:2026-03-08T02:30','during:2026-11-01T01:30',
    'during:2026-03-08','during:2026-11-01',
    'during:2026-03-08T01:30-06:00','during:2026-11-01T01:30-05:00',
    'during:2026-11-12..2026-11-14','before:today..now','after:yesterday..today',
    'last:0ms','last:1ms','last:2s','last:3m','last:2h','last:2d','last:2w','last:2H',
    'before:not-a-date','during:2026-13-01','during:2026-11-32','during:2026-02-30',
    'during:11/12/2026','during:Nov-12-2026','during:12-Nov-2026','during:2026/11/12',
    'during:2026-11-12T25:00','during:2026-11-12Z',
    'before:2026-11-12T14:30Z','after:2026-11-12T14:30Z',
    'during:"2026-11-12"','during:2026-11-12 before:2026-11-14',
  ];
  const dateFixtures=[];
  for (const now of ['2026-11-12T16:20:30Z','2026-07-12T16:20:30Z','2026-03-08T16:20:30Z','2026-11-01T16:20:30Z']) {
    Settings.now=()=>Date.parse(now);
    for (const zone of ['UTC','America/Chicago']) for (const locale of ['en-US','en-GB']) {
      process.env.TZ=zone;Settings.defaultZone=zone;Settings.defaultLocale=locale;
      for (const query of dateQueries) {
        const params=context.parseFixture(query,{},{});
        dateFixtures.push({now,zone,locale,query,params});
      }
    }
  }
  const dateOut=path.join(root,'fluxer_native/crates/core/tests/fixtures/search_query_dates.json');
  fs.writeFileSync(dateOut,JSON.stringify({source:sourcePath,luxon_version:VERSION,fixtures:dateFixtures},null,2)+'\n');
  console.log(`Wrote ${dateFixtures.length} source-executed date fixtures.`);
}

if (process.argv.includes('--week-locales')) {
  const weekInfo=locale=> {const value=new Intl.Locale(locale);return (value.weekInfo??value.getWeekInfo()).firstDay;};
  const territories={},languages={};
  const territoryDefaults={};
  for (let a=65;a<=90;a++) for(let b=65;b<=90;b++) {
    const code=String.fromCharCode(a,b);const day=weekInfo(`und-${code}`);
    if(day!==1)(territories[day]??=[]).push(code);
  }
  for (let a=97;a<=122;a++) for(let b=97;b<=122;b++) {
    const code=String.fromCharCode(a,b);const day=weekInfo(code);
    if(day!==1) {(languages[day]??=[]).push(code);territoryDefaults[code]=new Intl.Locale(code).maximize().region;}
  }
  const localeFile='packages/constants/src/Locales.ts';
  const localeText=fs.readFileSync(path.join(root,localeFile),'utf8');
  const locales=[...localeText.matchAll(/^\s*[A-Z_0-9]+: '([^']+)'/gm)].map(m=>m[1]);
  const extra=['en','en-CA','ar-EG','ar-SA','dv-MV','pt','pt-PT','fa','zh-Hant','zh-Hans','xx','und','en-US-u-fw-mon','en-US-u-rg-gbzzzz','en-Latn-CA'];
  const fixtures=[...new Set([...locales,...extra])].map(locale=>({locale,region:new Intl.Locale(locale).maximize().region??null,first_day:weekInfo(locale)}));
  const environments=[{},...['C','C.UTF-8','POSIX','en_US.UTF-8','en_GB.UTF-8','en_CA.UTF-8','ar_EG.UTF-8','ar_SA.UTF-8','dv_MV.UTF-8'].map(LANG=>({LANG})),
    {LANG:'en_US.UTF-8',LC_ALL:'en_GB.UTF-8'}, {LANG:'en_US.UTF-8',LC_MESSAGES:'en_GB.UTF-8'},
    {LANG:'en_GB.UTF-8',LC_ALL:''},{LANG:'en_GB.UTF-8',LC_MESSAGES:''},
    {LANG:'en_US.UTF-8',LC_TIME:'en_GB.UTF-8'},
  ];
  const hostFixtures=environments.map(environment=> {
    const env={...process.env};for(const key of ['LC_ALL','LC_MESSAGES','LC_TIME','LANG','LANGUAGE'])delete env[key];Object.assign(env,environment);
    const script=`const locale=Intl.DateTimeFormat().resolvedOptions().locale;const value=new Intl.Locale(locale);console.log(JSON.stringify({locale,first_day:(value.weekInfo??value.getWeekInfo()).firstDay}));`;
    const child=spawnSync(process.execPath,['-e',script],{env,encoding:'utf8'});
    if(child.error)throw child.error;
    if(child.status!==0)throw new Error(child.stderr);
    return {environment,...JSON.parse(child.stdout)};
  });
  const out=path.join(root,'fluxer_native/crates/core/tests/fixtures/search_query_week.json');
  fs.writeFileSync(out,JSON.stringify({source:'Intl.Locale week info used by Luxon localeWeekStart',locale_source:localeFile,territories,languages,territory_defaults:territoryDefaults,fixtures,host_fixtures:hostFixtures},null,2)+'\n');
  console.log(`Wrote ${fixtures.length} locale and ${hostFixtures.length} host-locale fixtures.`);
}
