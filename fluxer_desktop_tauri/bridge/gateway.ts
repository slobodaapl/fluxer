// SPDX-License-Identifier: AGPL-3.0-or-later

// `nativeGatewayTransport`, the port of fluxer_desktop/src/preload/NativeGatewayPreload.ts,
// served by src-tauri/src/gateway/commands.rs.
//
// Electron pushed every connection's events on one IPC channel. Here `create` hands the shell a
// Tauri channel per connection; its messages carry no connection id (the bridge adds it) and a
// binary frame arrives as a bare ArrayBuffer. Both are rebuilt into the Electron event shape and
// fed to the same event listeners, which validate them as the preload did.

import {
	NATIVE_GATEWAY_TRANSPORT_EVENT_CHANNEL,
	type NativeGatewayTransportAPI,
	type NativeGatewayTransportCloseRequest,
	type NativeGatewayTransportCreateRequest,
	type NativeGatewayTransportCreateResult,
	type NativeGatewayTransportEvent,
	NativeGatewayTransportEventKind,
	type NativeGatewayTransportSendBinaryRequest,
	type NativeGatewayTransportSendTextRequest,
} from '@fluxer/desktop_ipc/src/GatewayTransportContract';
import {Channel} from '@tauri-apps/api/core';
import type {BridgeConfig} from './config';
import {parseNativeGatewayTransportEvent, readExactPlainRecord} from './gatewayEvent';
import {dispatch, invoke, invokeOwned, invokeOwnedBytes, subscribe} from './ipc';

const SEND_REQUEST_KEYS = Object.freeze(['connectionId', 'payload']);
const CONNECTION_ID_HEADER = 'x-fluxer-connection-id';

class NativeGatewayTransportUnavailableError extends Error {
	public constructor() {
		super('The native gateway transport is unavailable in this desktop shell');
		this.name = 'NativeGatewayTransportUnavailableError';
	}
}

/** The checks of `requireNativeGatewaySendBinaryRequest` the shell cannot make on a raw body. */
class InvalidNativeGatewayTransportRequestError extends TypeError {
	public constructor(reason: string) {
		super(`Native gateway transport request is invalid: ${reason}`);
		this.name = 'InvalidNativeGatewayTransportRequestError';
	}
}

function requestConnectionId(request: unknown): string | null {
	if (request === null || typeof request !== 'object') return null;
	const {connectionId} = request as {connectionId?: unknown};
	return typeof connectionId === 'string' ? connectionId : null;
}

/** The Electron event a channel message stands for. */
function connectionEvent(connectionId: string, message: unknown): unknown {
	if (message instanceof ArrayBuffer) {
		return {
			connectionId,
			kind: NativeGatewayTransportEventKind.BINARY,
			data: null,
			binary: message,
			code: null,
			reason: null,
			wasClean: null,
			message: null,
		};
	}
	if (message === null || typeof message !== 'object') return message;
	return {connectionId, ...message};
}

function binaryBody(request: unknown): {connectionId: string; payload: Uint8Array} {
	const record = readExactPlainRecord(request, SEND_REQUEST_KEYS);
	if (record == null) {
		throw new InvalidNativeGatewayTransportRequestError(
			'binary send request must be a plain object with exactly its own keys',
		);
	}
	const {connectionId, payload} = record;
	if (typeof connectionId !== 'string') {
		throw new InvalidNativeGatewayTransportRequestError('connection id must be a string');
	}
	if (payload instanceof ArrayBuffer) {
		return {connectionId, payload: new Uint8Array(payload)};
	}
	if (ArrayBuffer.isView(payload)) {
		return {connectionId, payload: new Uint8Array(payload.buffer, payload.byteOffset, payload.byteLength)};
	}
	throw new InvalidNativeGatewayTransportRequestError('binary payload must be a byte buffer');
}

export function createNativeGatewayTransport(config: BridgeConfig): NativeGatewayTransportAPI {
	// Electron's main window carried `--fluxer-native-gateway-available=1` when the addon loaded;
	// the main window is the only one this shell opens, so the capability manifest decides.
	const isAvailable = config.capabilities.gatewaySocket;
	let availabilityProbe: Promise<boolean> | null = null;
	const probeAvailability = (): Promise<boolean> => {
		availabilityProbe ??= invoke<boolean>('native_gateway_transport_is_available')
			.then((value) => value === true)
			.catch(() => false);
		return availabilityProbe;
	};
	return Object.freeze<NativeGatewayTransportAPI>({
		isAvailable,
		create: async (request: NativeGatewayTransportCreateRequest): Promise<NativeGatewayTransportCreateResult> => {
			if (!(await probeAvailability())) {
				throw new NativeGatewayTransportUnavailableError();
			}
			const channel = new Channel<unknown>();
			const connectionId = requestConnectionId(request);
			if (connectionId !== null) {
				channel.onmessage = (message) =>
					dispatch(NATIVE_GATEWAY_TRANSPORT_EVENT_CHANNEL, connectionEvent(connectionId, message));
			}
			return await invokeOwned<NativeGatewayTransportCreateResult>('native_gateway_transport_create', {
				request,
				channel,
			});
		},
		sendText: async (request: NativeGatewayTransportSendTextRequest): Promise<void> => {
			await invokeOwned('native_gateway_transport_send_text', {request});
		},
		sendBinary: async (request: NativeGatewayTransportSendBinaryRequest): Promise<void> => {
			const {connectionId, payload} = binaryBody(request);
			await invokeOwnedBytes('native_gateway_transport_send_binary', payload, {
				[CONNECTION_ID_HEADER]: connectionId,
			});
		},
		close: async (request: NativeGatewayTransportCloseRequest): Promise<void> => {
			await invokeOwned('native_gateway_transport_close', {request});
		},
		dispose: async (connectionId: string): Promise<void> => {
			await invokeOwned('native_gateway_transport_dispose', {connectionId});
		},
		onEvent: (callback: (event: NativeGatewayTransportEvent) => void): (() => void) =>
			subscribe<unknown>(NATIVE_GATEWAY_TRANSPORT_EVENT_CHANNEL, (payload) => {
				let reconstructed: NativeGatewayTransportEvent;
				try {
					reconstructed = parseNativeGatewayTransportEvent(payload);
				} catch {
					return;
				}
				callback(reconstructed);
			}),
	});
}
