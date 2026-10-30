// SPDX-License-Identifier: AGPL-3.0-or-later

import {getElectronAPI} from '@app/features/ui/utils/NativeUtils';
import {createNativeVoiceIsolationNode} from '@app/features/voice/utils/noise_suppression/NativeVoiceIsolation';
import {STREAMR_CHANNELS} from '@fluxer/desktop_ipc/src/StreamrContract';

const workletURL = new URL('./streamrProcessor.worklet.js', import.meta.url).href;

export async function createStreamrNode(
	context: AudioContext,
	signal: AbortSignal,
	onFailure: (error: unknown) => void,
	mode: 'streamr' | 'gamr' = 'streamr',
) {
	const api = getElectronAPI()?.streamr;
	const label = mode === 'gamr' ? 'Gamr' : 'Streamr';
	if (!api) throw new Error(`${label} needs the native desktop build`);
	return createNativeVoiceIsolationNode(
		context,
		signal,
		onFailure,
		{
			start: async () => (await api.start(mode)).id,
			connect: (id) =>
				new Promise<MessagePort>((resolve, reject) => {
					const cleanup = () => {
						clearTimeout(timer);
						window.removeEventListener('message', receive);
						signal.removeEventListener('abort', abort);
					};
					const abort = () => {
						cleanup();
						reject(signal.reason);
					};
					const receive = (event: MessageEvent) => {
						if (
							event.source !== window ||
							event.data?.type !== STREAMR_CHANNELS.port ||
							event.data.id !== id ||
							!event.ports[0]
						)
							return;
						cleanup();
						resolve(event.ports[0]);
					};
					const timer = setTimeout(() => {
						cleanup();
						reject(new Error(`${label} audio transport did not connect`));
					}, 5000);
					window.addEventListener('message', receive);
					signal.addEventListener('abort', abort, {once: true});
					if (signal.aborted) {
						abort();
						return;
					}
					void api.connect(id).catch((error) => {
						cleanup();
						reject(error);
					});
				}),
			stop: (id) => api.stop(id),
		},
		workletURL,
		'fluxer-streamr',
		label,
	);
}
