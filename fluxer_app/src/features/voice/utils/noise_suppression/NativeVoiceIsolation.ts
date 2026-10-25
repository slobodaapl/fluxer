// SPDX-License-Identifier: AGPL-3.0-or-later
interface NativeIsolationAPI {
	start(): Promise<string>;
	push(id: string, audio: Float32Array): Promise<Float32Array>;
	stop(id: string): Promise<void>;
}

const loaded = new WeakMap<BaseAudioContext, Map<string, Promise<void>>>();

export async function createNativeVoiceIsolationNode(
	context: AudioContext,
	signal: AbortSignal,
	onFailure: (error: unknown) => void,
	api: NativeIsolationAPI,
	moduleURL: string,
	processor: string,
	label: string,
) {
	if (context.sampleRate !== 48000) throw new Error(`${label} requires a 48 kHz input context`);
	const modules = loaded.get(context) ?? new Map<string, Promise<void>>();
	loaded.set(context, modules);
	let module = modules.get(moduleURL);
	if (!module) {
		module = context.audioWorklet.addModule(moduleURL).catch((error) => {
			modules.delete(moduleURL);
			throw error;
		});
		modules.set(moduleURL, module);
	}
	await module;
	signal.throwIfAborted();
	const id = await api.start();
	if (signal.aborted) {
		await api.stop(id);
		signal.throwIfAborted();
	}
	let node: AudioWorkletNode;
	try {
		node = new AudioWorkletNode(context, processor, {
			numberOfInputs: 2, numberOfOutputs: 1, outputChannelCount: [1], channelCount: 1, channelCountMode: 'explicit',
		});
	} catch (error) {
		await api.stop(id);
		throw error;
	}
	let disposed = false, busy = false;
	const queue: Array<Float32Array> = [];
	const dispose = () => {
		if (disposed) return;
		disposed = true;
		queue.length = 0;
		signal.removeEventListener('abort', dispose);
		node.port.onmessage = null;
		node.onprocessorerror = null;
		node.port.postMessage({type: 'dispose'});
		node.port.close();
		node.disconnect();
		void api.stop(id).catch(() => {});
	};
	const fail = (error: unknown) => {
		if (disposed) return;
		dispose();
		onFailure(error);
	};
	const drain = async () => {
		if (busy || disposed) return;
		busy = true;
		try {
			while (queue.length && !disposed) {
				const audio = await api.push(id, queue.shift()!);
				if (!disposed) node.port.postMessage({type: 'processed', audio}, [audio.buffer]);
			}
		} catch (error) { fail(error); }
		finally { busy = false; }
	};
	node.port.onmessage = ({data}) => {
		if (data.type === 'error') { fail(new Error(data.message)); return; }
		if (data.type !== 'audio' || disposed) return;
		if (queue.length >= 2) { fail(new Error(`${label} processing queue overflow`)); return; }
		queue.push(data.audio);
		void drain();
	};
	node.onprocessorerror = () => fail(new Error(`${label} audio worklet failed`));
	signal.addEventListener('abort', dispose, {once: true});
	return {node, dispose};
}
