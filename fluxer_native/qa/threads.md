# Text and announcement threads

Open a thread-enabled community and find its active threads under their text or
announcement channels in the sidebar. Select a thread to load its own history
and send a message, then use the back arrow in the header to return to its
parent. A thread that archives while open displays its archive state. The
server handles automatic unarchiving and membership when sending permits it.

Use **New thread** in a text or announcement channel header, enter a name,
and choose **Create**. The newly created thread opens if the parent is still
selected. The server enforces channel permissions, account restrictions, and
creation limits.

To check live discovery, use two accounts with access to the same parent.
Create a thread with one account and check that it appears for the other.
Open the thread on the second account, send a message, and confirm both
accounts see it in the thread timeline. Delete the thread from an authorized
client and confirm it disappears from the sidebar.

This slice includes active thread discovery, standalone public and
announcement thread creation, history, sending, and navigation to the parent.
Archived browsing, explicit membership actions, the thread roster, private
and message-started creation, archive and lock controls, and forum and media
post browsing are outside this slice.

The wire contracts are in the repository's HTTP API and Gateway thread
documentation and the server's `ThreadController.ts`.
