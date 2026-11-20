# Animated custom emoji setting

The Messages & media setting writes the classic account field `animate_emoji` as a partial settings PATCH. Its default is enabled, matching `UserSettings.ts`; the existing native emoji presentation policy uses this preference independently of GIF autoplay and pauses automatic animation when reduced motion is enabled. Hover playback remains available through the existing emoji renderer. The account setting remains visible when reduced motion overrides playback. Failed saves use the existing optimistic rollback and error path.

Theme selection is not exposed: native desktop views contain hardcoded dark colors throughout, so changing only a subset would produce an inconsistent light theme. Manual GUI and real-account acceptance remain pending.
