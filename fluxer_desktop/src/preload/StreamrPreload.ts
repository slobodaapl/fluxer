// SPDX-License-Identifier: AGPL-3.0-or-later
import {STREAMR_CHANNELS, type StreamrAPI} from '@fluxer/desktop_ipc/src/StreamrContract';
import {ipcRenderer} from 'electron';

export function createStreamrPreloadAPI(): StreamrAPI {
	return Object.freeze({
		status: () => ipcRenderer.invoke(STREAMR_CHANNELS.status),
		saveSample: (sample: Float32Array) => ipcRenderer.invoke(STREAMR_CHANNELS.saveSample, sample),
		clearSample: () => ipcRenderer.invoke(STREAMR_CHANNELS.clearSample),
		start: () => ipcRenderer.invoke(STREAMR_CHANNELS.start),
		push: (id: string, audio: Float32Array) => ipcRenderer.invoke(STREAMR_CHANNELS.push, id, audio),
		stop: (id: string) => ipcRenderer.invoke(STREAMR_CHANNELS.stop, id),
	});
}
