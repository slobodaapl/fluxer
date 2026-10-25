// SPDX-License-Identifier: AGPL-3.0-or-later
import {getElectronAPI} from '@app/features/ui/utils/NativeUtils';
import {createNativeVoiceIsolationNode} from '@app/features/voice/utils/noise_suppression/NativeVoiceIsolation';

const workletURL = new URL('./streamrProcessor.worklet.js', import.meta.url).href;

export async function createStreamrNode(context: AudioContext, signal: AbortSignal, onFailure: (error: unknown) => void) {
	const api = getElectronAPI()?.streamr;
	if (!api) throw new Error('Streamr needs the native desktop build');
	return createNativeVoiceIsolationNode(context, signal, onFailure, {
		start: async () => (await api.start()).id,
		push: (id, audio) => api.push(id, audio),
		stop: (id) => api.stop(id),
	}, workletURL, 'fluxer-streamr', 'Streamr');
}
