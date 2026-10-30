// SPDX-License-Identifier: AGPL-3.0-or-later

import {Channel, invoke as tauriInvoke} from '@tauri-apps/api/core';

interface BridgeEvent {
	readonly name: string;
	readonly payload: unknown;
}

/** Serialised form of `BridgeError` in src-tauri/src/error.rs. */
interface BridgeErrorPayload {
	readonly name: string;
	readonly message: string;
}

type Listener = (payload: unknown) => void;

const TOKEN_BYTES = 32;

const listeners = new Map<string, Set<Listener>>();
export const documentToken = mintDocumentToken();

function mintDocumentToken(): string {
	const bytes = crypto.getRandomValues(new Uint8Array(TOKEN_BYTES));
	return Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('');
}

function isBridgeErrorPayload(value: unknown): value is BridgeErrorPayload {
	if (value === null || typeof value !== 'object') return false;
	const record = value as Record<string, unknown>;
	return typeof record.name === 'string' && typeof record.message === 'string';
}

/**
 * Rebuilds the Error Electron's `ipcRenderer.invoke` rejected with. The renderer matches on
 * the error name inside the message (for example BootstrapErrorKind.ts), so the format is kept.
 */
function toInvokeError(command: string, reason: unknown): Error {
	if (isBridgeErrorPayload(reason)) {
		const error = new Error(`Error invoking remote method '${command}': ${reason.name}: ${reason.message}`);
		error.name = reason.name;
		return error;
	}
	if (reason instanceof Error) return reason;
	return new Error(`Error invoking remote method '${command}': ${String(reason)}`);
}

export async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
	try {
		return await tauriInvoke<T>(command, args);
	} catch (reason) {
		throw toInvokeError(command, reason);
	}
}

/** Invoke for commands that bind the calling renderer document (RendererDocumentOwner.ts). */
export function invokeOwned<T>(command: string, args?: Record<string, unknown>): Promise<T> {
	return invoke<T>(command, {...args, documentToken});
}

/**
 * `invokeOwned` for a command whose argument is a raw byte body. The document token, and any
 * other argument, travel as request headers.
 */
export async function invokeOwnedBytes<T>(
	command: string,
	body: Uint8Array,
	headers: Record<string, string>,
): Promise<T> {
	try {
		return await tauriInvoke<T>(command, body, {headers: {...headers, 'x-fluxer-document-token': documentToken}});
	} catch (reason) {
		throw toInvokeError(command, reason);
	}
}

/** Fire-and-forget command, the counterpart of `ipcRenderer.send`. */
export function send(command: string, args?: Record<string, unknown>): void {
	invoke(command, args).catch((error: unknown) => {
		console.error(`[desktop bridge] ${command} failed`, error);
	});
}

/** Subscribes to an event the shell pushes on this webview's channel. */
export function subscribe<T>(name: string, listener: (payload: T) => void): () => void {
	let set = listeners.get(name);
	if (set === undefined) {
		set = new Set();
		listeners.set(name, set);
	}
	const entry = listener as Listener;
	set.add(entry);
	return () => {
		set.delete(entry);
	};
}

/** Delivers an event to this document's listeners as if the shell had pushed it. */
export function dispatch(name: string, payload: unknown): void {
	const set = listeners.get(name);
	if (set === undefined) return;
	for (const listener of [...set]) {
		try {
			listener(payload);
		} catch (error) {
			console.error(`[desktop bridge] ${name} listener threw`, error);
		}
	}
}

/** Registers this document and its event channel. Runs once per document, before page scripts. */
export function attachDocument(): void {
	send('register_document', {token: documentToken});
	const channel = new Channel<BridgeEvent>();
	channel.onmessage = ({name, payload}) => dispatch(name, payload);
	send('bridge_attach', {channel});
}
