# Device revocation prerequisite

Device revocation remains unavailable in Native. The protocol crate can parse a
`SUDO_MODE_REQUIRED` challenge, but does not yet retain response headers, prompt
for proofs, retry the request, or handle reauthentication. Parsing a challenge
is not authorization.

Classic fetches `GET /auth/sessions` and sends `POST /auth/sessions/logout` with
`session_id_hashes` (at most 100). The IDs are base64url session hashes from the
list, and the server revokes the selected sessions for the authenticated user.
Classic skips an empty selection. The current session can be selected, so a
future implementation must handle its loss and clear local authentication.

The server runs sudo middleware before logout. A valid
`X-Fluxer-Sudo-Mode-JWT` is scoped to the user, not the individual session;
MFA and email-code verification can return a token in the same header, valid
for five minutes. Password verification returns no token. A request without a
proof can succeed for accounts with no verifiable credentials, including a
recent SSO sign-in in the server's ten-minute window. Thus an initial logout
request is itself potentially destructive, not a harmless challenge probe.

On 403 `SUDO_MODE_REQUIRED`, the top-level `has_mfa` and `methods` describe the
available proofs. Password is only accepted when usable MFA is absent; backup
codes use `mfa_method: totp` and `mfa_code`. WebAuthn and email codes need their
own challenge or ticket flow; SSO can require signing in again. Future work must
keep the selection and authenticated account/instance bound to the pending
operation, handle password and all advertised methods, protect proof and token
secrets, capture and scope returned headers, and avoid automatic retries after
ambiguous transport failures. It must also handle verification failures and
refresh the list only after a confirmed success. The parser intentionally does
not initiate any of these actions.

Source: `fluxer_api/src/api/auth/AuthController.ts`, `AuthSession.ts`,
`services/SudoVerificationService.ts`, `services/SudoMethods.ts`,
`middleware/SudoModeMiddleware.ts`, and classic `AuthSessionCommands.ts` and
`RestTransport.ts`.
