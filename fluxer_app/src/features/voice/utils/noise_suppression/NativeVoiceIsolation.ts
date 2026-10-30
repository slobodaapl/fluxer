// SPDX-License-Identifier: AGPL-3.0-or-later
interface NativeIsolationAPI {
	start(): Promise<string>;
	connect(id: string): Promise<MessagePort>;
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
	let transport: MessagePort | undefined;
	try {
		transport = await api.connect(id);
		signal.throwIfAborted();
		node = new AudioWorkletNode(context, processor, {
			numberOfInputs: 2,
			numberOfOutputs: 1,
			outputChannelCount: [1],
			channelCount: 1,
			channelCountMode: 'explicit',
			processorOptions: {label},
		});
	} catch (error) {
		transport?.close();
		await api.stop(id);
		throw error;
	}
	let disposed = false;
	const dispose = () => {
		if (disposed) return;
		disposed = true;
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
	node.port.onmessage = ({data}) => {
		if (data.type === 'error') fail(new Error(data.message));
	};
	node.port.postMessage({type: 'transport', port: transport}, [transport]);
	node.onprocessorerror = () => fail(new Error(`${label} audio worklet failed`));
	signal.addEventListener('abort', dispose, {once: true});
	return {node, dispose};
}
