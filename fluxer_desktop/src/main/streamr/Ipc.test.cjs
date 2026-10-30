// SPDX-License-Identifier: AGPL-3.0-or-later
'use strict';
const {test} = require('node:test');
const assert = require('node:assert/strict');
const {EventEmitter} = require('node:events');
const {MessageChannel} = require('node:worker_threads');
const fs = require('node:fs/promises');
const path = require('node:path');
const {pathToFileURL} = require('node:url');
const {registerHooks} = require('node:module');

const desktop = path.resolve(__dirname, '../../..');
const routes = new Map(), app = new EventEmitter();
const state = {app, routes, portListeners: new Map(), createNativeStream: null,
 MessageChannelMain: class {
  constructor(){const {port1,port2}=new MessageChannel();const wrap=port=>({postMessage:data=>port.postMessage(data),close:()=>port.close(),start:()=>port.start(),on:(name,fn)=>port.on(name,name==='message'?data=>fn({data}):fn)});this.port1=wrap(port1);this.port2=port2;}
 }
};
globalThis.__streamrIpcTest = state;
const hooks = registerHooks({
	resolve(specifier, context, next) {
		if (specifier === 'electron') return {url: 'streamr-test:electron', shortCircuit: true};
		if (specifier === '@electron/main/streamr/Native.cjs') return {url: 'streamr-test:native', shortCircuit: true};
		if (specifier === '@electron/main/PrivilegedRendererDocuments') return {url: 'streamr-test:policy', shortCircuit: true};
		if (specifier.startsWith('@electron/')) return {url: pathToFileURL(path.join(desktop, 'src', `${specifier.slice(10)}.ts`)).href, shortCircuit: true};
		if (specifier.startsWith('@fluxer/desktop_ipc/')) return {url: pathToFileURL(path.resolve(desktop, '../packages/desktop_ipc', `${specifier.slice(20)}.ts`)).href, shortCircuit: true};
		return next(specifier, context);
	},
	load(url, context, next) {
		const sources = {
			'streamr-test:electron': `const s=globalThis.__streamrIpcTest; export const MessageChannelMain=s.MessageChannelMain; export const app=s.app; export const ipcMain={handle:(name,handler)=>s.routes.set(name,handler)}; export const ipcRenderer={on:(name,fn)=>s.portListeners.set(name,fn),invoke:(name,...args)=>Promise.resolve().then(()=>s.routes.get(name)(s.event,...args))}; export const webContents={fromFrame:frame=>frame.sender};`,
			'streamr-test:native': `export const createNativeStream=(options)=>globalThis.__streamrIpcTest.createNativeStream(options);`,
			'streamr-test:policy': `import {RendererDocumentOwnerFactory} from '@electron/main/RendererDocumentOwner'; export function createPrivilegedRendererDocumentOwners() { const factory=new RendererDocumentOwnerFactory({policy:{isPrivilegedRendererDocument:({sender})=>sender.trusted},onWatcherFailure:error=>{throw error}}); const capture=factory.capture.bind(factory); factory.capture=(...args)=>{const owner=capture(...args); if(globalThis.__streamrIpcTest.failWatcher) owner.watchInvalidation=()=>{throw new Error('watcher unavailable')}; return owner;}; return factory; }`,
		};
		if (sources[url]) return {format: 'module', source: sources[url], shortCircuit: true};
		return next(url, context);
	},
});
const loaded = import(pathToFileURL(path.join(desktop, 'src/main/Streamr.ts')));

function event() {
	const sender = new EventEmitter();
	sender.trusted = true; sender.isDestroyed = () => false; sender.isLoadingMainFrame = () => false;
	const frame = {postMessage(channel,id,ports){sender.emit('port',{channel,id,ports});}, sender, detached: false, parent: null, url: 'https://trusted.example'};
	frame.top = frame; sender.mainFrame = frame;
	return {sender, senderFrame: frame};
}
async function fixture(t) {
	const base = path.join(desktop, 'native/streamr/artifact'); await fs.mkdir(base, {recursive: true});
	const root = await fs.mkdtemp(path.join(base, 'ipc-test-'));
	t.after(async () => {app.emit('before-quit'); await fs.rm(root, {recursive: true, force: true});});
	routes.clear(); state.portListeners.clear(); app.removeAllListeners(); app.isPackaged = false;
	state.failWatcher = false;
	app.getAppPath = () => root; app.getPath = () => path.join(root, 'user');
	await fs.mkdir(path.join(root, 'native/streamr/artifact'), {recursive: true});
	await fs.writeFile(path.join(root, 'native/streamr/artifact', process.platform === 'win32' ? 'streamr-native.exe' : 'streamr-native'), 'fixture');
	await fs.mkdir(path.join(root, 'streamr-models'));
	for (const name of ['reuse.safetensors', 'resampler.f64', 'vox_cache_names.json', 'vox_enroll_fp32.nnef', 'vox_cache_fp32.nnef', 'vox_chunk_fp32.nnef', 'vox_chunk_fp32.safetensors']) {
		await fs.writeFile(path.join(root, 'streamr-models', name), 'fixture');
	}
	const natives = [];
	state.createNativeStream = async ({signal}) => {
		const native = {backend: 'cpu', closed: false, close() {this.closed = true;}, async push(audio) {if (this.closed) throw Error('closed'); return audio;}};
		signal.addEventListener('abort', () => native.close(), {once: true}); natives.push(native); return native;
	};
	(await loaded).registerStreamrIpcHandlers();
	const owner = event();
	state.event = owner;
	const invoke = async (name, ...args) => routes.get(`streamr:${name}`)(owner, ...args);
	return {root, owner, natives, invoke, other: (name, sender, ...args) => routes.get(`streamr:${name}`)(sender, ...args)};
}

test('enrollment persists locally and document ownership restricts audio and stop', async t => {
	const f = await fixture(t);
	await assert.rejects(f.invoke('start'), /Record a Streamr voice sample/);
	await assert.rejects(f.invoke('save-sample', new Float32Array(144000)), /too quiet/);
	const sample = new Float32Array(144000).fill(0.25); await f.invoke('save-sample', sample);
	assert.deepEqual(await f.invoke('status'), {enrolled: true, modelsAvailable: true});
	const saved = await fs.readFile(path.join(f.root, 'user/streamr/voice.f32'));
	assert.deepEqual(saved, Buffer.from(sample.buffer));
	const {id, backend} = await f.invoke('start'); assert.equal(backend, 'cpu');
	const stranger = event();
	await assert.rejects(Promise.resolve().then(() => f.other('push', stranger, id, new Float32Array(960))), /not owned/);
	await assert.rejects(Promise.resolve().then(() => f.other('stop', stranger, id)), /not owned/);
	assert.equal(f.natives[0].closed, false);
	const audio = new Float32Array(960).fill(0.1); assert.deepEqual(await f.invoke('push', id, audio), audio);
	await f.invoke('stop', id); await f.invoke('stop', id);
	assert.equal(f.natives[0].closed, true);
	await assert.rejects(f.invoke('push', id, audio), /not owned/);
});

test('navigation closes sessions and an untrusted document cannot enroll', async t => {
	const f = await fixture(t);
	await f.invoke('save-sample', new Float32Array(144000).fill(0.25));
	const {id} = await f.invoke('start');
	f.owner.sender.emit('did-start-navigation', {isMainFrame: false, isSameDocument: false});
	assert.equal(f.natives[0].closed, false);
	f.owner.sender.emit('did-start-navigation', {isMainFrame: true, isSameDocument: false});
	assert.equal(f.natives[0].closed, true);
	await assert.rejects(f.invoke('push', id, new Float32Array(960)), /not owned/);
	f.owner.sender.trusted = false;
	await assert.rejects(f.invoke('save-sample', new Float32Array(144000).fill(0.25)), /privileged/);
});

test('clearing enrollment stops all active sessions and removes the recording', async t => {
	const f = await fixture(t); await f.invoke('save-sample', new Float32Array(144000).fill(0.25));
	await f.invoke('start'); await f.invoke('start');
	await assert.rejects(f.invoke('start'), /Too many/);
	await f.invoke('clear-sample');
	assert.ok(f.natives.every(x => x.closed)); assert.equal((await f.invoke('status')).enrolled, false);
	await assert.rejects(fs.access(path.join(f.root, 'user/streamr/voice.f32')), {code: 'ENOENT'});
});

test('navigation during startup aborts the native initialization', async t => {
	const f = await fixture(t); await f.invoke('save-sample', new Float32Array(144000).fill(0.25));
	let entered;
	const ready = new Promise(resolve => {entered = resolve;});
	state.createNativeStream = ({signal}) => new Promise((_resolve, reject) => {
		signal.addEventListener('abort', () => reject(new Error('startup cancelled')), {once: true}); entered();
	});
	const starting = f.invoke('start'); await ready;
	f.owner.sender.emit('render-process-gone');
	await assert.rejects(starting, /startup cancelled/);
});

test('preload API reaches the registered session routes and preserves audio payloads', async t => {
	const f = await fixture(t);
	const {createStreamrPreloadAPI} = await import(pathToFileURL(path.join(desktop, 'src/preload/StreamrPreload.ts')));
	const api = createStreamrPreloadAPI(); assert.ok(Object.isFrozen(api));
	assert.equal((await api.status()).enrolled, false);
	await api.saveSample(new Float32Array(144000).fill(0.25));
	const {id} = await api.start(), audio = new Float32Array(960).fill(-0.125);
	assert.deepEqual(await api.push(id, audio), audio);
	await api.stop(id); assert.equal(f.natives[0].closed, true);
	await api.clearSample(); assert.equal((await api.status()).enrolled, false);
});

test('finite decoded enrollment peaks above unity survive saving; nonfinite samples are rejected', async t => {
	const f = await fixture(t), sample = new Float32Array(144000).fill(0.25);
	sample[100] = 1.05; sample[101] = -1.05;
	await f.invoke('save-sample', sample);
	assert.deepEqual(await fs.readFile(path.join(f.root, 'user/streamr/voice.f32')), Buffer.from(sample.buffer));
	sample[100] = NaN;
	await assert.rejects(f.invoke('save-sample', sample), /Invalid Streamr voice sample/);
});

test('watcher installation failure releases the reserved session slot', async t => {
	const f = await fixture(t); await f.invoke('save-sample', new Float32Array(144000).fill(0.25));
	state.failWatcher = true;
	for (let attempt = 0; attempt < 3; attempt++) {
		await assert.rejects(f.invoke('start'), /watcher unavailable/);
	}
	state.failWatcher = false;
	const {id} = await f.invoke('start'); assert.equal(typeof id, 'string');
	assert.equal(f.natives.length, 1);
});

test.after(() => {hooks.deregister(); delete globalThis.__streamrIpcTest;});


test('Gamr starts without enrollment or VoxLock files through the preload contract', async t => {
 const f = await fixture(t);
 for (const name of await fs.readdir(path.join(f.root, 'streamr-models'))) {
  if (name.startsWith('vox_')) await fs.rm(path.join(f.root, 'streamr-models', name));
 }
 const original = state.createNativeStream;
 state.createNativeStream = options => {
  assert.equal(options.mode, 'gamr');
  assert.equal(options.sample, undefined);
  return original(options);
 };
 const {createStreamrPreloadAPI} = await import(pathToFileURL(path.join(desktop, 'src/preload/StreamrPreload.ts')));
 const api = createStreamrPreloadAPI();
 const {id} = await api.start('gamr');
 const audio = new Float32Array(960).fill(0.2);
 assert.deepEqual(await api.push(id, audio), audio);
 await assert.rejects(fs.access(path.join(f.root, 'user/streamr/voice.f32')), {code:'ENOENT'});
 await assert.rejects(async () => f.other('push', event(), id, audio), /not owned/);
 await api.stop(id);
 assert.equal(f.natives[0].closed, true);
 await assert.rejects(api.start('invalid'), /Invalid voice processing mode/);
});

test('clearing Streamr enrollment preserves an active Gamr session', async t => {
 const f = await fixture(t);
 const gamr = await f.invoke('start', 'gamr');
 await f.invoke('save-sample', new Float32Array(144000).fill(0.25));
 await f.invoke('start', 'streamr');
 await f.invoke('clear-sample');
 assert.equal(f.natives[0].closed, false);
 assert.equal(f.natives[1].closed, true);
 const audio = new Float32Array(960).fill(0.1);
 assert.deepEqual(await f.invoke('push', gamr.id, audio), audio);
 await assert.rejects(f.invoke('start', 'streamr'), /Record a Streamr voice sample/);
});


test('direct audio port delivers samples, rejects duplicate attachment, and closes on navigation', {timeout:3000}, async t => {
 const f=await fixture(t);const {id}=await f.invoke('start','gamr');
 let port;f.owner.sender.on('port',event=>{assert.equal(event.id,id);port=event.ports[0];});
 await assert.rejects(async()=>f.other('connect',event(),id),/not owned/);
 await f.invoke('connect',id);assert.ok(port);t.after(()=>port.close());
 await assert.rejects(f.invoke('connect',id),/already connected/);
 await assert.rejects(f.invoke('push',id,new Float32Array(960)),/direct transport/);
 const reply=new Promise(resolve=>port.once('message',resolve));
 const audio=new Float32Array(960).fill(.25);port.postMessage({type:'audio',audio});
 assert.deepEqual(await reply,{type:'processed',audio});
 port.on('message',()=>{});
 const closed=new Promise(resolve=>port.once('close',resolve));
 f.owner.sender.emit('render-process-gone');await closed;assert.equal(f.natives[0].closed,true);
});

test('closing the renderer audio port stops its native session', async t => {
 const f=await fixture(t);const {id}=await f.invoke('start','gamr');let port;
 f.owner.sender.on('port',event=>{port=event.ports[0];});await f.invoke('connect',id);port.close();
 for(let i=0;i<20&&!f.natives[0].closed;i++)await new Promise(r=>setTimeout(r,5));
 assert.equal(f.natives[0].closed,true);
});


test('invalid audio on the direct port closes the session with a diagnostic', {timeout:3000}, async t => {
 const f=await fixture(t);const {id}=await f.invoke('start','gamr');let port;
 f.owner.sender.on('port',event=>{port=event.ports[0];});await f.invoke('connect',id);t.after(()=>port.close());
 const reply=new Promise(resolve=>port.once('message',resolve));
 port.postMessage({type:'audio',audio:new Float32Array([NaN])});
 const error=await reply;assert.equal(error.type,'error');assert.match(error.message,/960 finite/);assert.equal(f.natives[0].closed,true);
});
