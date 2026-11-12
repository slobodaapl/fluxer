// SPDX-License-Identifier: AGPL-3.0-or-later
// The pinned LiveKit WebRTC archive contains weak pw_* forwarding symbols.
// They must be loaded before pipewire-rs can call its system-library bindings.
// Declaration from WebRTC modules/portal/pipewire_utils.h. No C++ STL types
// cross this boundary, so the wrapper needs neither SDK headers nor its libc++.
namespace webrtc {
bool InitializePipeWire();
}
extern "C" bool fluxer_initialize_webrtc_pipewire() {
  return webrtc::InitializePipeWire();
}
