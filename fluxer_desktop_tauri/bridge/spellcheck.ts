// SPDX-License-Identifier: AGPL-3.0-or-later

/**
 * Spellcheck members of the bridge, plus the two jobs Electron's main process did on the
 * renderer's behalf that need the DOM here:
 *
 * - Context menu. Electron suppressed Chromium's menu on editable targets and sent
 *   `textarea-context-menu` with the misspelled word Chromium had marked, its suggestions and
 *   the edit flags. WebKitGTK's own menu is suppressed in src-tauri/src/linux_webview.rs; this
 *   module finds the word under the pointer, asks the shell whether it is misspelled, and
 *   dispatches the same event. It keeps the word's range for `spellcheckReplaceMisspelling`,
 *   which Electron delegated to `webContents.replaceMisspelling`.
 * - Language auto-detection. Electron's preload sent the composer text to main, which ran
 *   franc-min; detection runs here and only the detected dictionary base goes to the shell.
 *
 * Edit flags follow Chromium's rules: copy needs a selected range outside a password field,
 * cut additionally an editable target (always true here), paste only an editable target, and
 * select-all is always offered. Undo and redo come from `document.queryCommandEnabled`.
 */

import type {ElectronAPI, SpellcheckState, TextareaContextMenuParams} from '@app/types/electron';
import type {
	SpellcheckBundledDictionary,
	SpellcheckResolvedEngineInfo,
} from '@fluxer/desktop_ipc/src/DesktopBridgeTypes';
import {franc} from 'franc-min';
import {dispatch, invoke, send, subscribe} from './ipc';

const TEXTAREA_CONTEXT_MENU_EVENT = 'textarea-context-menu';
const STATE_CHANGED_EVENT = 'spellcheck-state-changed';
const ENGINE_RESOLVED_EVENT = 'spellcheck-engine-resolved';

/** Input types Chromium treats as editable text fields. */
const TEXT_INPUT_TYPES = new Set(['text', 'search', 'url', 'tel', 'email', 'password', 'number']);
/** Input types whose words are checked; the others are not prose or lack the selection API. */
const SPELLCHECKED_INPUT_TYPES = new Set(['text', 'search']);

const AUTODETECT_DEBOUNCE_MS = 750;
const AUTODETECT_PREFIX_SKIP_CHARS = 20;
const AUTODETECT_MAX_TEXT_LENGTH = 2000;
/** `ISO6393_TO_DICTIONARY_BASE` in fluxer_desktop/src/main/Spellcheck.ts. */
const ISO6393_TO_DICTIONARY_BASE: Readonly<Record<string, string>> = {
	bul: 'bg',
	cat: 'ca',
	ces: 'cs',
	dan: 'da',
	deu: 'de',
	ell: 'el',
	eng: 'en',
	epo: 'eo',
	est: 'et',
	eus: 'eu',
	fas: 'fa',
	fra: 'fr',
	glg: 'gl',
	heb: 'he',
	hrv: 'hr',
	hun: 'hu',
	isl: 'is',
	ita: 'it',
	kor: 'ko',
	lav: 'lv',
	lit: 'lt',
	nld: 'nl',
	nob: 'nb',
	pol: 'pl',
	por: 'pt',
	ron: 'ro',
	rus: 'ru',
	slk: 'sk',
	slv: 'sl',
	spa: 'es',
	srp: 'sr',
	swe: 'sv',
	tur: 'tr',
	ukr: 'uk',
	vie: 'vi',
};
const AUTODETECT_SUPPORTED_ISO6393 = Object.keys(ISO6393_TO_DICTIONARY_BASE);

type TextControl = HTMLInputElement | HTMLTextAreaElement;

interface EditableTarget {
	/** The text control, or the editing host of contenteditable content. */
	readonly element: HTMLElement;
	readonly control: TextControl | null;
}

type Misspelling =
	| {readonly word: string; readonly control: TextControl; readonly start: number; readonly end: number}
	| {readonly word: string; readonly host: HTMLElement; readonly range: Range};

interface AutodetectContext {
	lastSentText: string;
	pendingText: string;
	/** Main's per-context cache in Electron: the text last detected and its result. */
	detected: {text: string; language: string | null} | null;
}

const wordSegmenter = new Intl.Segmenter(undefined, {granularity: 'word'});
const autodetectContexts = new WeakMap<HTMLTextAreaElement, AutodetectContext>();
let autodetectTimer: ReturnType<typeof setTimeout> | null = null;
/** Mirrors the shell's `enabled && autoDetect`; Electron's main skipped detection otherwise. */
let autodetectActive = true;
let pendingMisspelling: Misspelling | null = null;

function trackState(state: SpellcheckState): SpellcheckState {
	autodetectActive = state.enabled && state.autoDetect !== false;
	return state;
}

function editingHost(element: HTMLElement): HTMLElement {
	let host = element;
	while (host.parentElement?.isContentEditable) {
		host = host.parentElement;
	}
	return host;
}

/** Chromium's `isEditable`: a writable text field or contenteditable content. */
function editableTarget(node: EventTarget | null): EditableTarget | null {
	if (node instanceof HTMLTextAreaElement || (node instanceof HTMLInputElement && TEXT_INPUT_TYPES.has(node.type))) {
		return node.readOnly || node.disabled ? null : {element: node, control: node};
	}
	if (node instanceof HTMLElement && node.isContentEditable) {
		return {element: editingHost(node), control: null};
	}
	return null;
}

function wordAt(text: string, offset: number): {start: number; end: number; word: string} | null {
	const segments = wordSegmenter.segment(text);
	for (const index of [offset, offset - 1]) {
		if (index < 0 || index >= text.length) continue;
		const segment = segments.containing(index);
		if (segment?.isWordLike) {
			return {start: segment.index, end: segment.index + segment.segment.length, word: segment.segment};
		}
	}
	return null;
}

/**
 * The word under the pointer, where the target is spell checked. A text control needs
 * `caretPositionFromPoint`, which reports the offset into the control's value; contenteditable
 * content also works with `caretRangeFromPoint`.
 */
function wordUnderPointer(target: EditableTarget, x: number, y: number): Misspelling | null {
	const {element, control} = target;
	if (!element.spellcheck || (control instanceof HTMLInputElement && !SPELLCHECKED_INPUT_TYPES.has(control.type))) {
		return null;
	}
	const position = typeof document.caretPositionFromPoint === 'function' ? document.caretPositionFromPoint(x, y) : null;
	if (control !== null) {
		if (position?.offsetNode !== control) return null;
		const found = wordAt(control.value, position.offset);
		return found && {word: found.word, control, start: found.start, end: found.end};
	}
	let node: Node | null = position?.offsetNode ?? null;
	let offset = position?.offset ?? 0;
	if (position === null && typeof document.caretRangeFromPoint === 'function') {
		const range = document.caretRangeFromPoint(x, y);
		node = range?.startContainer ?? null;
		offset = range?.startOffset ?? 0;
	}
	if (!(node instanceof Text) || !element.contains(node)) return null;
	const found = wordAt(node.data, offset);
	if (found === null) return null;
	const range = document.createRange();
	range.setStart(node, found.start);
	range.setEnd(node, found.end);
	return {word: found.word, host: element, range};
}

function hasSelectedRange(target: EditableTarget): boolean {
	if (target.control !== null) {
		const {selectionStart, selectionEnd} = target.control;
		return selectionStart !== null && selectionStart !== selectionEnd;
	}
	const selection = window.getSelection();
	if (selection === null || selection.isCollapsed || selection.rangeCount === 0) return false;
	return target.element.contains(selection.getRangeAt(0).commonAncestorContainer);
}

function editFlags(target: EditableTarget): Record<string, boolean> {
	const canCopy = hasSelectedRange(target) && target.control?.type !== 'password';
	return {
		canUndo: document.queryCommandEnabled('undo'),
		canRedo: document.queryCommandEnabled('redo'),
		canCut: canCopy,
		canCopy,
		canPaste: true,
		canSelectAll: true,
	};
}

async function reportContextMenu(
	event: MouseEvent,
	candidate: Misspelling | null,
	flags: Record<string, boolean>,
): Promise<void> {
	// Chromium raised its context menu event only when the page left the default action alone.
	if (event.defaultPrevented) return;
	let suggestions: Array<string> | null = null;
	if (candidate !== null) {
		try {
			suggestions = await invoke<Array<string> | null>('spellcheck_check_word', {word: candidate.word});
		} catch (error) {
			console.error('[desktop bridge] spellcheck_check_word failed', error);
		}
	}
	pendingMisspelling = suggestions === null ? null : candidate;
	const params: TextareaContextMenuParams = {
		x: event.clientX,
		y: event.clientY,
		misspelledWord: suggestions === null ? undefined : candidate?.word,
		suggestions: suggestions ?? [],
		editFlags: flags,
	};
	dispatch(TEXTAREA_CONTEXT_MENU_EVENT, params);
}

function onContextMenu(event: MouseEvent): void {
	const target = editableTarget(event.target);
	if (target === null) return;
	// Read the pointer's word and the selection now; page handlers may move them.
	const candidate = wordUnderPointer(target, event.clientX, event.clientY);
	const flags = editFlags(target);
	setTimeout(() => void reportContextMenu(event, candidate, flags), 0);
}

/** Selects the word the last context menu reported and types the replacement over it. */
function replaceMisspelling(replacement: string): void {
	const misspelling = pendingMisspelling;
	pendingMisspelling = null;
	if (misspelling === null) return;
	if ('control' in misspelling) {
		const {control, start, end, word} = misspelling;
		if (!control.isConnected || control.value.slice(start, end) !== word) return;
		control.focus({preventScroll: true});
		control.setSelectionRange(start, end);
	} else {
		const {host, range, word} = misspelling;
		const selection = window.getSelection();
		if (!host.isConnected || range.toString() !== word || selection === null) return;
		host.focus({preventScroll: true});
		selection.removeAllRanges();
		selection.addRange(range);
	}
	// insertText keeps the edit on the undo stack and fires the input events React listens to.
	document.execCommand('insertText', false, replacement);
}

function isTinyPrefixChange(previous: string, next: string): boolean {
	if (previous.length === 0 || next.length === 0) return false;
	if (Math.abs(next.length - previous.length) >= AUTODETECT_PREFIX_SKIP_CHARS) return false;
	return next.startsWith(previous) || previous.startsWith(next);
}

function detectLanguage(context: AutodetectContext, text: string): string | null {
	const previous = context.detected;
	if (previous !== null && (previous.text === text || isTinyPrefixChange(previous.text, text))) {
		return previous.language;
	}
	const code =
		text.trim().length === 0
			? null
			: franc(text.slice(0, AUTODETECT_MAX_TEXT_LENGTH), {minLength: 3, only: [...AUTODETECT_SUPPORTED_ISO6393]});
	const language = (code !== null && ISO6393_TO_DICTIONARY_BASE[code]) || null;
	context.detected = {text, language};
	return language;
}

/** The preload's debounced `spellcheck:update-autodetect-text`, then main's detection. */
function onInput(event: Event): void {
	const target = event.target;
	if (!(target instanceof HTMLTextAreaElement)) return;
	let context = autodetectContexts.get(target);
	if (context === undefined) {
		context = {lastSentText: '', pendingText: '', detected: null};
		autodetectContexts.set(target, context);
	}
	context.pendingText = target.value;
	if (autodetectTimer !== null) clearTimeout(autodetectTimer);
	const pending = context;
	autodetectTimer = setTimeout(() => {
		autodetectTimer = null;
		if (pending.pendingText === pending.lastSentText) return;
		if (isTinyPrefixChange(pending.lastSentText, pending.pendingText)) return;
		pending.lastSentText = pending.pendingText;
		if (!autodetectActive) return;
		send('spellcheck_update_autodetect_language', {language: detectLanguage(pending, pending.pendingText)});
	}, AUTODETECT_DEBOUNCE_MS);
}

/** Installs this document's context menu and auto-detection listeners. */
export function startSpellcheck(): void {
	window.addEventListener('contextmenu', onContextMenu, true);
	window.addEventListener('input', onInput, true);
	subscribe<SpellcheckState>(STATE_CHANGED_EVENT, trackState);
}

export const spellcheck = {
	spellcheckGetAvailableLanguages: () => invoke<Array<string>>('spellcheck_get_available_languages'),
	spellcheckGetBundledDictionaries: () =>
		invoke<Array<SpellcheckBundledDictionary>>('spellcheck_get_bundled_dictionaries'),
	spellcheckSetState: (state) => invoke<SpellcheckState>('spellcheck_set_state', {patch: state}).then(trackState),
	onSpellcheckEngineResolved: (callback) => subscribe<SpellcheckResolvedEngineInfo>(ENGINE_RESOLVED_EVENT, callback),
	spellcheckReplaceMisspelling: replaceMisspelling,
	spellcheckAddWordToDictionary: (word) => invoke<null>('spellcheck_add_word_to_dictionary', {word}).then(() => {}),
} satisfies Pick<
	ElectronAPI,
	| 'spellcheckGetAvailableLanguages'
	| 'spellcheckGetBundledDictionaries'
	| 'spellcheckSetState'
	| 'onSpellcheckEngineResolved'
	| 'spellcheckReplaceMisspelling'
	| 'spellcheckAddWordToDictionary'
>;
