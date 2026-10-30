// SPDX-License-Identifier: AGPL-3.0-or-later
import {STREAMR_CHANNELS, type StreamrAPI, type StreamrMode} from '@fluxer/desktop_ipc/src/StreamrContract';
import {ipcRenderer} from 'electron';

export function createStreamrPreloadAPI(): StreamrAPI {
	ipcRenderer.on(STREAMR_CHANNELS.port, (event, id: string) => {
		window.postMessage({type: STREAMR_CHANNELS.port, id}, '*', event.ports);
	});
	return Object.freeze({
		status: () => ipcRenderer.invoke(STREAMR_CHANNELS.status),
		saveSample: (sample: Float32Array) => ipcRenderer.invoke(STREAMR_CHANNELS.saveSample, sample),
		clearSample: () => ipcRenderer.invoke(STREAMR_CHANNELS.clearSample),
		start: (mode?: StreamrMode) => ipcRenderer.invoke(STREAMR_CHANNELS.start, mode),
		push: (id: string, audio: Float32Array) => ipcRenderer.invoke(STREAMR_CHANNELS.push, id, audio),
		connect: (id: string) => ipcRenderer.invoke(STREAMR_CHANNELS.connect, id),
		stop: (id: string) => ipcRenderer.invoke(STREAMR_CHANNELS.stop, id),
	});
}
