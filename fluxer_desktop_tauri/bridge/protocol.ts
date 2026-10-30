// SPDX-License-Identifier: AGPL-3.0-or-later

// Electron's webRequest hook added a secret header to every local app request the app's own
// frames made (LocalAppProtocolAuthorization.ts). Tauri has no such hook, so the bridge adds it:
// main-frame fetch and XMLHttpRequest calls to /api and /proxy carry this document's token,
// which src-tauri/src/local_app/proxy/admission.rs checks. Media elements and download links
// cannot carry it; the shell leaves /proxy reads open for them.

const PROTOCOL_AUTHORIZATION_HEADER = 'X-Fluxer-Desktop-Protocol-Authorization';
const AUTHORIZED_PATH_PREFIXES = ['/api/', '/proxy/'];

// `fluxer-app:` is not a special scheme, so URL.origin is opaque; compare scheme and host.
function needsAuthorization(url: string): boolean {
	let parsed: URL;
	try {
		parsed = new URL(url, window.location.href);
	} catch {
		return false;
	}
	return (
		parsed.protocol === window.location.protocol &&
		parsed.host === window.location.host &&
		AUTHORIZED_PATH_PREFIXES.some((prefix) => parsed.pathname.startsWith(prefix))
	);
}

function requestUrl(input: RequestInfo | URL): string {
	if (input instanceof Request) return input.url;
	if (input instanceof URL) return input.href;
	return String(input);
}

export function installProtocolAuthorization(token: string): void {
	const nativeFetch = window.fetch;
	window.fetch = function fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
		if (!needsAuthorization(requestUrl(input))) {
			return nativeFetch.call(window, input, init);
		}
		// Explicit init headers replace a Request's own, as fetch() itself merges them.
		const headers = new Headers(init?.headers ?? (input instanceof Request ? input.headers : undefined));
		headers.set(PROTOCOL_AUTHORIZATION_HEADER, token);
		return nativeFetch.call(window, input, {...init, headers});
	};

	const openedUrls = new WeakMap<XMLHttpRequest, string>();
	const nativeOpen = XMLHttpRequest.prototype.open;
	const nativeSend = XMLHttpRequest.prototype.send;
	XMLHttpRequest.prototype.open = function open(this: XMLHttpRequest, ...args: Array<unknown>): void {
		openedUrls.set(this, String(args[1]));
		(nativeOpen as (...openArgs: Array<unknown>) => void).apply(this, args);
	};
	XMLHttpRequest.prototype.send = function send(
		this: XMLHttpRequest,
		body?: Document | XMLHttpRequestBodyInit | null,
	): void {
		const url = openedUrls.get(this);
		if (url !== undefined && needsAuthorization(url)) {
			this.setRequestHeader(PROTOCOL_AUTHORIZATION_HEADER, token);
		}
		nativeSend.call(this, body);
	};
}
