// SPDX-License-Identifier: AGPL-3.0-or-later

// Window dragging from the renderer's custom titlebar. Electron's Chromium dragged the window
// from `-webkit-app-region: drag` boxes; WebKit has no app regions, so a press is hit-tested
// against the titlebar markup instead: the nearest titlebar ancestor starts a drag unless an
// interactive or no-drag element lies between it and the pressed element.

import {send} from './ipc';

const DRAG_REGION_SELECTOR = [
	'[data-native-titlebar]',
	'#fluxer-startup-native-titlebar',
	'[data-flx="voice.popout-titlebar.titlebar"]',
].join(', ');
const NO_DRAG_SELECTOR = '[data-no-drag], .fluxer-native-titlebar__controls, [data-window-control]';
const INTERACTIVE_TAGS = new Set(['A', 'BUTTON', 'INPUT', 'SELECT', 'TEXTAREA', 'LABEL', 'SUMMARY']);
const INTERACTIVE_ROLES = new Set([
	'button',
	'link',
	'checkbox',
	'menuitem',
	'tab',
	'switch',
	'textbox',
	'slider',
	'option',
	'combobox',
]);

function isInteractive(element: Element): boolean {
	if (INTERACTIVE_TAGS.has(element.tagName)) return true;
	if (element instanceof HTMLElement && (element.isContentEditable || element.tabIndex >= 0)) return true;
	const role = element.getAttribute('role');
	return role !== null && INTERACTIVE_ROLES.has(role);
}

/** Whether the event's path reaches a drag region before anything that keeps the press. */
function startsInDragRegion(event: MouseEvent): boolean {
	for (const target of event.composedPath()) {
		if (!(target instanceof Element)) continue;
		if (isInteractive(target) || target.matches(NO_DRAG_SELECTOR)) return false;
		if (target.matches(DRAG_REGION_SELECTOR)) return true;
	}
	return false;
}

/**
 * Installs the drag handler for a window drawing its own titlebar. The press is not consumed,
 * so the renderer still sees the double click it maximizes on (NativeTitlebar.tsx). macOS has
 * no such handler there, and its system titlebar zooms on a double click, so the bridge does it
 * on the second release, as long as the pointer has not moved since the second press.
 */
export function installTitlebarDrag(platform: string): void {
	let doublePressX = 0;
	let doublePressY = 0;
	document.addEventListener('mousedown', (event) => {
		if (event.button !== 0 || event.defaultPrevented || !startsInDragRegion(event)) return;
		if (event.detail === 1) {
			send('window_start_dragging');
		} else if (event.detail === 2) {
			doublePressX = event.clientX;
			doublePressY = event.clientY;
		}
	});
	if (platform !== 'darwin') return;
	document.addEventListener('mouseup', (event) => {
		if (
			event.button === 0 &&
			event.detail === 2 &&
			event.clientX === doublePressX &&
			event.clientY === doublePressY &&
			startsInDragRegion(event)
		) {
			send('window_toggle_maximize');
		}
	});
}
