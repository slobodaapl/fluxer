// SPDX-License-Identifier: AGPL-3.0-or-later

// The accelerators of Electron's application menu (fluxer_desktop/src/main/Menu.ts) outside
// macOS, where macOS has the native menu instead. Electron hid that menu bar on Windows and
// Linux but its accelerators stayed live, firing for any real key press the page had not
// `preventDefault`ed. The renderer's own shortcuts (zoom, settings, Ctrl+M) call
// `preventDefault`, so they do not fire twice.
//
// The listener runs first, in the capture phase, so nothing the page does stops it from
// seeing the key; whether the page handled it is read once the event has been dispatched.
// Keys are matched by `event.code`, the physical key; Electron matched key codes, which follow
// the layout on some systems. The edit roles (undo, cut, ...) are left to the webview's own
// editing keys.

import {dispatch, invoke, send} from './ipc';

type Platform = 'win32' | 'linux';
type WindowAction = 'toggle-devtools' | 'reload' | 'force-reload' | 'toggle-fullscreen' | 'quit';

interface Accelerator {
	readonly code: string;
	readonly ctrl?: boolean;
	readonly shift?: boolean;
	readonly alt?: boolean;
	readonly run: () => void;
}

function windowAction(action: WindowAction): () => void {
	return () => {
		invoke('app_menu_action', {action}).catch((error: unknown) => {
			console.error(`[desktop bridge] ${action} failed`, error);
		});
	};
}

function accelerators(platform: Platform): ReadonlyArray<Accelerator> {
	const zoomIn = () => dispatch('zoom-in', undefined);
	const toggleFullscreen = windowAction('toggle-fullscreen');
	return [
		// File: Preferences (Ctrl+,), then the quit role, which has no accelerator on Windows.
		{code: 'Comma', ctrl: true, run: () => dispatch('open-settings', undefined)},
		...(platform === 'linux' ? [{code: 'KeyQ', ctrl: true, run: windowAction('quit')}] : []),
		// View.
		{code: 'KeyR', ctrl: true, run: windowAction('reload')},
		{code: 'KeyR', ctrl: true, shift: true, run: windowAction('force-reload')},
		{code: 'KeyI', ctrl: true, shift: true, run: windowAction('toggle-devtools')},
		{code: 'Digit0', ctrl: true, run: () => dispatch('zoom-reset', undefined)},
		// `CmdOrCtrl+Plus` (shifted `=`) and the hidden `CmdOrCtrl+=`.
		{code: 'Equal', ctrl: true, shift: true, run: zoomIn},
		{code: 'Equal', ctrl: true, run: zoomIn},
		{code: 'Minus', ctrl: true, run: () => dispatch('zoom-out', undefined)},
		{code: 'F11', run: toggleFullscreen},
		{code: 'Enter', alt: true, run: toggleFullscreen},
		{code: 'NumpadEnter', alt: true, run: toggleFullscreen},
		// Window: the minimize role.
		{code: 'KeyM', ctrl: true, run: () => send('window_minimize')},
	];
}

function matches(accelerator: Accelerator, event: KeyboardEvent): boolean {
	return (
		event.code === accelerator.code &&
		event.ctrlKey === (accelerator.ctrl ?? false) &&
		event.shiftKey === (accelerator.shift ?? false) &&
		event.altKey === (accelerator.alt ?? false) &&
		!event.metaKey
	);
}

export function installMenuAccelerators(platform: Platform): void {
	const table = accelerators(platform);
	window.addEventListener(
		'keydown',
		(event) => {
			if (!event.isTrusted) return;
			const accelerator = table.find((candidate) => matches(candidate, event));
			if (accelerator === undefined) return;
			setTimeout(() => {
				if (!event.defaultPrevented) accelerator.run();
			}, 0);
		},
		true,
	);
}
