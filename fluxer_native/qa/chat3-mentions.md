# Mention interaction QA

Launch the offline demo with `--demo --demo-mentions`. The fixture adds populated, empty, long-name, unresolved, concealed and code mentions. It uses synthetic local users, roles and messages only. Resize the window narrowly and scroll the fixture near each edge to inspect popup placement and clipping.

- Click precisely on each resolved user/role glyph and then on adjacent whitespace. Users open the existing profile with the channel's community context. Resolved roles open a compact role card showing its color and cached assignment count, explicitly described as incomplete. Unresolved mentions stay inert.
- Check hidden spoilers before and after revealing them, and inline code: concealed and code content must not activate a target.
- Check normal, compact, reply, pinned and search message presentations; the activated mention must retain the originating channel context.
- Check Escape and outside-click dismissal and return to the previous focus. With a caret on a mention, Enter should activate it. Verify placement with pointer and keyboard, at all edges and in narrow windows.

The upstream `MentionRenderer.tsx` opens a user popout for resolved users, but renders role mentions as inert colored spans. The native role card is a requested extension; it reports only the locally cached assignments, never a total roster. The gateway member cache can be partial.

- In the long fixture, Tab and Shift-Tab through the first mention, the role in the last table column, and the final mention. Each must scroll into view; the table should also scroll horizontally. Enter or Space opens the popup beside the visible glyph. Repeat in search results and pins, and verify pointer clicks do not move the scroller.
