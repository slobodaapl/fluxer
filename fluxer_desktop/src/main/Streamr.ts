// SPDX-License-Identifier: AGPL-3.0-or-later
import {randomUUID} from 'node:crypto';
import {existsSync, readFileSync} from 'node:fs';
import fs from 'node:fs/promises';
import path from 'node:path';
import {writeFileAtomically} from '@electron/main/AtomicFile';
import {createPrivilegedRendererDocumentOwners} from '@electron/main/PrivilegedRendererDocuments';
import type {RendererDocumentOwner, RendererDocumentOwnerWatcher} from '@electron/main/RendererDocumentOwnership';
import {createNativeStream, type NativeStream} from '@electron/main/streamr/Native.cjs';
import {STREAMR_CHANNELS, type StreamrMode} from '@fluxer/desktop_ipc/src/StreamrContract';
import {app, type IpcMainInvokeEvent, ipcMain, MessageChannelMain, type MessagePortMain} from 'electron';

interface Session {
	mode: StreamrMode;
	owner: RendererDocumentOwner;
	controller: AbortController;
	watcher?: RendererDocumentOwnerWatcher;
	native?: NativeStream;
	port?: MessagePortMain;
}

function voiceSample(value: unknown): Float32Array {
	if (!(value instanceof Float32Array) || value.length !== 144000) {
		throw new Error('Streamr voice sample must be three seconds at 48 kHz');
	}
	let energy = 0;
	for (const x of value) {
		if (!Number.isFinite(x)) throw new Error('Invalid Streamr voice sample');
		energy += x * x;
	}
	if (energy / value.length < 1e-7) throw new Error('Voice sample is too quiet. Speak closer to the microphone.');
	return value;
}

export function registerStreamrIpcHandlers(): void {
	const owners = createPrivilegedRendererDocumentOwners('Streamr');
	const sessions = new Map<string, Session>();
	const samplePath = () => path.join(app.getPath('userData'), 'streamr', 'voice.f32');
	const modelDir = () => path.join(app.isPackaged ? process.resourcesPath : app.getAppPath(), 'streamr-models');
	const executable = () =>
		path.join(
			app.isPackaged
				? path.join(process.resourcesPath, 'streamr-native')
				: path.join(app.getAppPath(), 'native', 'streamr', 'artifact'),
			process.platform === 'win32' ? 'streamr-native.exe' : 'streamr-native',
		);
	const hasModels = (mode: StreamrMode = 'streamr') =>
		process.arch === 'x64' &&
		existsSync(executable()) &&
		[
			'reuse.safetensors',
			'resampler.f64',
			...(mode === 'streamr'
				? [
						'vox_cache_names.json',
						'vox_enroll_fp32.nnef',
						'vox_cache_fp32.nnef',
						'vox_chunk_fp32.nnef',
						'vox_chunk_fp32.safetensors',
					]
				: []),
		].every((name) => existsSync(path.join(modelDir(), name)));
	const capture = (event: IpcMainInvokeEvent) => owners.capture(event, 'Streamr');
	const stop = (id: string, message = 'Streamr session closed') => {
		const session = sessions.get(id);
		if (!session) return;
		sessions.delete(id);
		session.port?.postMessage({type: 'error', message});
		session.port?.close();
		session.watcher?.dispose();
		session.controller.abort();
		session.native?.close(new Error(message));
	};
	const ownedSession = (event: IpcMainInvokeEvent, id: unknown) => {
		capture(event);
		if (typeof id !== 'string') throw new Error('Invalid Streamr session');
		const session = sessions.get(id);
		if (!session?.owner.matchesEvent(event)) throw new Error('Streamr session is not owned by this document');
		return session;
	};
	ipcMain.handle(STREAMR_CHANNELS.status, (event) => {
		capture(event);
		return {enrolled: existsSync(samplePath()), modelsAvailable: hasModels()};
	});
	ipcMain.handle(STREAMR_CHANNELS.saveSample, async (event, value: unknown) => {
		const owner = capture(event);
		const sample = voiceSample(value);
		await fs.mkdir(path.dirname(samplePath()), {recursive: true, mode: 0o700});
		owner.requireCurrent('Streamr enrollment');
		await writeFileAtomically(samplePath(), new Uint8Array(sample.buffer, sample.byteOffset, sample.byteLength), 0o600);
	});
	ipcMain.handle(STREAMR_CHANNELS.clearSample, async (event) => {
		capture(event);
		for (const [id, session] of sessions) if (session.mode === 'streamr') stop(id, 'Voice sample cleared');
		await fs.rm(samplePath(), {force: true});
	});
	ipcMain.handle(STREAMR_CHANNELS.start, async (event, mode: unknown = 'streamr') => {
		const owner = capture(event);
		if (mode !== 'streamr' && mode !== 'gamr') throw new Error('Invalid voice processing mode');
		if (!hasModels(mode))
			throw new Error(`${mode === 'gamr' ? 'Gamr' : 'Streamr'} is not installed. Run the setup steps in STREAMR.md.`);
		if ([...sessions.values()].filter((s) => s.owner.sender === event.sender).length >= 2 || sessions.size >= 4) {
			throw new Error('Too many Streamr sessions');
		}
		let processing: {mode: 'streamr'; sample: Float32Array} | {mode: 'gamr'};
		if (mode === 'streamr') {
			if (!existsSync(samplePath())) throw new Error('Record a Streamr voice sample in Voice Settings first.');
			const bytes = readFileSync(samplePath());
			if (bytes.byteLength !== 144000 * 4) throw new Error('Invalid saved Streamr sample. Record it again.');
			processing = {
				mode,
				sample: voiceSample(
					new Float32Array(bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength)),
				),
			};
		} else {
			processing = {mode};
		}
		owner.requireCurrent('Streamr start');
		const id = randomUUID();
		const session: Session = {mode, owner, controller: new AbortController()};
		sessions.set(id, session);
		try {
			session.watcher = owner.watchInvalidation(() => stop(id));
			const native = await createNativeStream({
				executable: executable(),
				modelDir: modelDir(),
				...processing,
				signal: session.controller.signal,
				onFailure: (error) => stop(id, error.message),
			});
			session.native = native;
			owner.requireCurrent('Streamr ready');
			return {id, backend: native.backend};
		} catch (error) {
			stop(id);
			throw error;
		}
	});
	ipcMain.handle(STREAMR_CHANNELS.connect, (event, id: unknown) => {
		const session = ownedSession(event, id);
		if (!session.native) throw new Error('Native voice processing is still starting');
		if (session.port) throw new Error('Native voice transport is already connected');
		const {port1, port2} = new MessageChannelMain();
		session.port = port1;
		let busy = false;
		port1.on('message', async ({data}) => {
			if (sessions.get(id as string) !== session) return;
			try {
				if (busy) throw new Error('Concurrent native voice audio requests');
				if (
					data?.type !== 'audio' ||
					!(data.audio instanceof Float32Array) ||
					data.audio.length !== 960 ||
					!data.audio.every(Number.isFinite)
				) {
					throw new Error('Native voice transport requires 960 finite float32 samples');
				}
				busy = true;
				const audio = await session.native!.push(data.audio);
				if (sessions.get(id as string) === session) port1.postMessage({type: 'processed', audio});
			} catch (error) {
				stop(id as string, error instanceof Error ? error.message : 'Native voice transport failed');
			} finally {
				busy = false;
			}
		});
		port1.on('close', () => {
			session.port = undefined;
			stop(id as string);
		});
		port1.start();
		try {
			session.owner.requireCurrent('Native voice transport');
			session.owner.frame.postMessage(STREAMR_CHANNELS.port, id, [port2]);
		} catch (error) {
			port2.close();
			stop(id as string);
			throw error;
		}
	});
	ipcMain.handle(STREAMR_CHANNELS.push, (event, id: unknown, value: unknown) => {
		const session = ownedSession(event, id);
		if (session.port) throw new Error('Audio is owned by the direct transport');
		if (!(value instanceof Float32Array) || value.length !== 960 || !value.every(Number.isFinite)) {
			throw new Error('Streamr requires 960 finite float32 samples per block');
		}
		if (!session.native) throw new Error('Streamr is still starting');
		return session.native.push(value);
	});
	ipcMain.handle(STREAMR_CHANNELS.stop, (event, id: string) => {
		capture(event);
		if (!sessions.has(id)) return;
		ownedSession(event, id);
		stop(id);
	});
	app.on('before-quit', () => {
		for (const id of sessions.keys()) stop(id);
	});
}
