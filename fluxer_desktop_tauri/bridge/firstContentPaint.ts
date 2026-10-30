// SPDX-License-Identifier: AGPL-3.0-or-later

// `createFirstContentPaintSignal` of fluxer_desktop/src/preload/FirstContentPaint.ts: the first
// call of a document tells the shell two frames later, once the content is on screen; later
// calls do nothing. The shell reveals the main window on it.
//
// Chromium painted a hidden window, so Electron could wait for frames before showing it. WebKit
// renders no frames for a hidden page, and the main window stays hidden until this signal, so a
// hidden document reports at once; the window then shows with the committed content over its
// theme background.

import {send} from './ipc';

const CHANNEL = 'window_first_content_painted';

export function createFirstContentPaintSignal(): () => void {
	let signalled = false;
	return () => {
		if (signalled) return;
		signalled = true;
		if (document.visibilityState === 'hidden') {
			send(CHANNEL);
			return;
		}
		window.requestAnimationFrame(() => {
			window.requestAnimationFrame(() => {
				send(CHANNEL);
			});
		});
	};
}
